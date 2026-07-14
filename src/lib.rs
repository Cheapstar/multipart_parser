use std::{
    collections::HashMap,
    fmt,
    io::{BufRead, Error, Read, Seek, SeekFrom, Write},
};

use tempfile::NamedTempFile;

#[derive(Debug)]

enum DataType {
    Bytes(Vec<u8>),
    File(NamedTempFile),
}

impl DataType {
    fn write_all(&mut self, chunk: &[u8]) -> Result<(), std::io::Error> {
        match self {
            DataType::Bytes(items) => items.write_all(chunk),
            DataType::File(file) => file.write_all(chunk),
        }
    }
}
impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataType::Bytes(bytes) => write!(f, "Bytes({} bytes) {:#?}", bytes.len(), bytes),
            DataType::File(named_temp_file) => match named_temp_file.as_file().try_clone() {
                Ok(mut cloned) => {
                    if let Err(e) = cloned.seek(SeekFrom::Start(0)) {
                        return write!(f, "File(<error seeking: {}>)", e);
                    }
                    let mut buf = Vec::new();
                    match cloned.read_to_end(&mut buf) {
                        Ok(_) => write!(
                            f,
                            "File({} bytes, path={:?})",
                            buf.len(),
                            named_temp_file.path()
                        ),
                        Err(e) => write!(f, "File(<error reading file: {}>)", e),
                    }
                }
                Err(e) => write!(f, "File(<error cloning handle: {}>)", e),
            },
        }
    }
}
#[derive(Debug)]
struct MultiPart {
    headers: Option<HashMap<String, String>>,
    data: Option<DataType>,
    content_type: Option<String>,
    max_body_limit_until_file: u32,
    max_file_size: u32,
}

impl MultiPart {
    fn new(max_body_limit_until_file: u32, max_file_size: u32) -> Self {
        Self {
            headers: None,
            data: None,
            content_type: None,
            max_body_limit_until_file,
            max_file_size,
        }
    }

    fn set_headers(&mut self, header_bytes: &[u8]) {
        let mut headers = HashMap::<String, String>::new();
        header_bytes.lines().for_each(|s| {
            let line = s.unwrap();

            let mut line_iter = line.split(";");

            let main_header = line_iter.next();

            main_header.unwrap().split_once(":").map(|(key, value)| {
                headers.insert(key.trim().to_owned(), value.trim().to_owned());
                Some(())
            });

            for pair in line_iter {
                if !pair.contains("=") {
                    break;
                }
                let (key, value) = pair.split_once("=").unwrap();
                headers.insert(key.trim().to_owned(), value.trim().to_owned());
            }
        });

        let content_type = headers.get("Content-Type").map(|s| s.to_owned());

        self.content_type = content_type;

        self.headers = Some(headers);
    }

    fn write(&mut self, chunk: &[u8]) -> Result<(), MultiPartParserError> {
        let headers = self.headers.as_ref().unwrap();
        let is_file_part = headers
            .get("content-disposition")
            .map(|cd| cd.contains("filename="))
            .unwrap_or(false);

        match self.data.take() {
            // First chunk for this part
            None => {
                if is_file_part {
                    let mut temp_file = tempfile::Builder::new()
                        .prefix("multipart")
                        .suffix(".tmp")
                        .tempfile_in("temp_files/")
                        .map_err(|e| MultiPartParserError::IOError(e))?;

                    temp_file
                        .write_all(chunk)
                        .map_err(|e| MultiPartParserError::IOError(e))?;

                    self.data = Some(DataType::File(temp_file));
                } else {
                    let mut buf = Vec::with_capacity(1024);
                    buf.extend_from_slice(chunk);
                    self.data = Some(DataType::Bytes(buf));
                }
            }

            // Already have in-memory bytes — maybe promote to file
            Some(DataType::Bytes(mut items)) => {
                if items.len() + chunk.len() > self.max_body_limit_until_file as usize {
                    let mut temp_file = tempfile::Builder::new()
                        .prefix("multipart")
                        .suffix(".tmp")
                        .tempfile_in("temp_files/")
                        .map_err(|e| MultiPartParserError::IOError(e))?;

                    temp_file
                        .write_all(&items)
                        .map_err(|e| MultiPartParserError::IOError(e))?;
                    temp_file
                        .write_all(chunk)
                        .map_err(|e| MultiPartParserError::IOError(e))?;

                    self.data = Some(DataType::File(temp_file));
                } else {
                    items.extend_from_slice(chunk);
                    self.data = Some(DataType::Bytes(items));
                }
            }

            // Already writing to a file — just keep appending (this branch was missing!)
            Some(DataType::File(mut temp_file)) => {
                temp_file
                    .write_all(chunk)
                    .map_err(|e| MultiPartParserError::IOError(e))?;
                self.data = Some(DataType::File(temp_file));
            }
        }

        Ok(())
    }
}

impl fmt::Display for MultiPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "MultiPart {{")?;

        match &self.headers {
            Some(headers) if !headers.is_empty() => {
                writeln!(f, "  headers: {{")?;
                for (key, value) in headers {
                    writeln!(f, "    {}: {},", key, value)?;
                }
                writeln!(f, "  }},")?;
            }
            Some(_) => {
                writeln!(f, "  headers: {{}},")?;
            }
            None => {
                writeln!(f, "  headers: None,")?;
            }
        }

        match &self.data {
            Some(data) => {
                writeln!(f, "  data: {},", data)?;
            }
            None => {
                writeln!(f, "  data: None,")?;
            }
        }

        match &self.content_type {
            Some(ct) => {
                writeln!(f, "  content_type: {},", ct)?;
            }
            None => {
                writeln!(f, "  content_type: None,")?;
            }
        }

        writeln!(
            f,
            "  max_body_limit_until_file: {},",
            self.max_body_limit_until_file
        )?;
        writeln!(f, "  max_file_size: {},", self.max_file_size)?;
        write!(f, "}}")
    }
}
struct MultiPartParser {
    max_header_size: u16,
    max_body_limit_until_file: u32,
    max_file_size: u32,

    boundary: String,
    boundary_pattern: String,
    opening_boundary_length: u16,
    boundary_length: u16,
    buffer: Vec<u8>,
    current_part: MultiPart,
    state: MultiPartParserState,
    content_length: usize,
    child_parser: Option<Box<MultiPartParser>>,
    parts: Vec<MultiPart>,
    index_after_child: Option<usize>,
}

impl MultiPartParser {
    fn new(
        max_header_size: u16,
        max_body_limit_until_file: u32,
        max_file_size: u32,

        boundary: String,
    ) -> Self {
        let opening_boundary_length: u16 = boundary.len() as u16 + 2; // --boundary
        let boundary_length = boundary.len() as u16 + 4; // \r\n--boundary
        let boundary_pattern = format!("\r\n--{boundary}");

        Self {
            max_header_size,
            max_body_limit_until_file,
            max_file_size,
            opening_boundary_length,
            boundary_length,
            boundary,
            boundary_pattern,
            state: MultiPartParserState::Start,
            buffer: Vec::<u8>::new(),
            content_length: 0,
            child_parser: None,
            current_part: MultiPart::new(max_body_limit_until_file, max_file_size),
            parts: Vec::<MultiPart>::new(),
            index_after_child: None,
        }
    }

    fn parse(&mut self, chunk: &[u8]) -> Result<(), MultiPartParserError> {
        if self.state == MultiPartParserState::Done {
            return Err(MultiPartParserError::UnexpectedDataAtEndOfStream);
        }

        let mut new_chunk = Vec::<u8>::new();
        let mut index: usize = 0;

        if self.child_parser.is_none() {
            if !self.buffer.is_empty() {
                let new_chunk_size = self.buffer.len() + chunk.len();
                new_chunk.reserve_exact(new_chunk_size);
                new_chunk.extend_from_slice(&self.buffer);
                new_chunk.extend_from_slice(chunk);
                self.buffer.clear();
                self.buffer.shrink_to(0);
            } else {
                new_chunk.extend_from_slice(chunk);
            }
        } else {
            new_chunk.extend_from_slice(chunk);

            if self.state != MultiPartParserState::Start {
                // boundary has been found out
                if self.buffer.is_empty() {
                    // if this chunk contain boundary of parent parser then keep it here
                    let boundary_index_inside_chunk =
                        substring_search(&new_chunk, index, self.boundary_pattern.as_bytes());

                    if boundary_index_inside_chunk.is_none() {
                        let partial_boundary_check =
                            substring_partial_search(&new_chunk, self.boundary_pattern.as_bytes());

                        self.index_after_child = partial_boundary_check;
                        if !partial_boundary_check.is_none() {
                            self.buffer.extend_from_slice(&new_chunk);
                        }
                    } else {
                        self.index_after_child = boundary_index_inside_chunk;
                        self.buffer.extend_from_slice(&new_chunk);
                    }
                } else {
                    // boundary has been previously found out , so just append the just to use it later on
                    self.buffer.extend_from_slice(&new_chunk);
                }
            }
        }

        loop {
            if self.state == MultiPartParserState::Start {
                if new_chunk.len() < self.opening_boundary_length as usize {
                    self.buffer.extend_from_slice(&new_chunk);
                    break;
                }

                if substring_search(&new_chunk, index, self.boundary_pattern[2..].as_bytes())
                    .is_none()
                {
                    return Err(MultiPartParserError::MissingInitialBoundary);
                }
                index = self.opening_boundary_length as usize;
                self.state = MultiPartParserState::AfterBoundary;
            }

            // we need this extra state to determine which boundary it is initial or final
            if self.state == MultiPartParserState::AfterBoundary {
                if new_chunk.len() - index < 2 {
                    self.buffer.extend_from_slice(&new_chunk[index..]);
                    break;
                }

                if new_chunk[(index) as usize] == b'-' && new_chunk[(index + 1) as usize] == b'-' {
                    self.state = MultiPartParserState::Done;
                    break;
                }

                if !(new_chunk[(index) as usize] == b'\r'
                    && new_chunk[(index + 1) as usize] == b'\n')
                {
                    return Err(MultiPartParserError::MalformedMultiPartBoundary);
                }

                index += 2; // skip \r\n
                self.state = MultiPartParserState::Header;
            }

            if self.state == MultiPartParserState::Header {
                // minimum we need to parse for headers
                if new_chunk.len() - index < 4 {
                    self.buffer.extend_from_slice(&new_chunk[index..]);
                    break;
                }

                let header_end_index = find_double_newline(&new_chunk, index);

                if header_end_index.is_none() {
                    if new_chunk.len() - index > (self.max_header_size as usize) {
                        return Err(MultiPartParserError::MaxHeaderLimitExceeded);
                    }

                    self.buffer.extend_from_slice(&new_chunk[index..]);

                    break;
                }

                let header_end_index = header_end_index.unwrap();
                if header_end_index - index > (self.max_header_size as usize) {
                    return Err(MultiPartParserError::MaxHeaderLimitExceeded);
                }

                let headers = &new_chunk[index..header_end_index];

                self.current_part.set_headers(headers);

                index = header_end_index + 4;
                self.state = MultiPartParserState::Body;
            }

            if self.state == MultiPartParserState::Body {
                if let Some(ref content_type) = self.current_part.content_type {
                    if content_type.starts_with("multipart/") {
                        if self.child_parser.is_none() {
                            let child_boundary = self
                                .current_part
                                .headers
                                .as_ref()
                                .and_then(|h| h.get("boundary"))
                                .cloned();

                            if child_boundary.is_none() {
                                return Err(MultiPartParserError::MaliciousPart);
                            }

                            let child_boundary = child_boundary.unwrap();

                            self.child_parser = Some(Box::new(MultiPartParser::new(
                                self.max_header_size,
                                self.max_body_limit_until_file,
                                self.max_file_size,
                                child_boundary,
                            )));
                        }

                        let child_parser = self.child_parser.as_mut().unwrap();

                        if child_parser.state != MultiPartParserState::Done {
                            child_parser.parse(&new_chunk[index..])?;
                            break;
                        } else {
                            // remove all the part of child and append it to this parser's
                            let number_of_child_parts = child_parser.parts.len();
                            for i in 0..number_of_child_parts {
                                let child_part = std::mem::replace(
                                    &mut child_parser.parts[i],
                                    MultiPart::new(
                                        self.max_body_limit_until_file,
                                        self.max_file_size,
                                    ),
                                );

                                self.parts.push(child_part);
                            }

                            // parser is working fine till here
                            self.child_parser = None;

                            if self.index_after_child.is_none() {
                                // this will be a multipart/mixed parent
                                let curr_part = std::mem::replace(
                                    &mut self.current_part,
                                    MultiPart::new(
                                        self.max_body_limit_until_file,
                                        self.max_file_size,
                                    ),
                                );

                                self.parts.push(curr_part);
                                break;
                            } else {
                                index = self.index_after_child.unwrap();
                                new_chunk.clear();
                                new_chunk.extend_from_slice(&self.buffer);
                                self.buffer.clear();
                            }
                        }
                    }
                }

                if new_chunk.len() - index < self.boundary.len() {
                    self.buffer.extend_from_slice(&new_chunk[index..]);
                    break;
                }

                let boundary_index =
                    substring_search(&new_chunk, index, self.boundary_pattern.as_bytes());
                // let stringified_bytes = String::from_utf8_lossy(&new_chunk);
                // println!("Header Bytes are: {}", stringified_bytes);
                if boundary_index.is_none() {
                    // No boundary found, but there may be a partial match at the end of the chunk.

                    let partial_tail_index =
                        substring_partial_search(&new_chunk, self.boundary_pattern.as_bytes());

                    if partial_tail_index.is_none() {
                        self.append(&new_chunk[index..])?;
                    } else {
                        let partial_tail_index = partial_tail_index.unwrap();
                        if partial_tail_index > index {
                            self.append(&new_chunk[index..partial_tail_index])?;
                        }
                        self.buffer = new_chunk[partial_tail_index..].to_vec();
                    }

                    break;
                }

                let boundary_index = boundary_index.unwrap();
                if boundary_index > index {
                    self.append(&new_chunk[index..boundary_index])?;
                }

                // means that current part has been initialised
                if !self.current_part.headers.is_none() {
                    let curr_part = std::mem::replace(
                        &mut self.current_part,
                        MultiPart::new(self.max_body_limit_until_file, self.max_file_size),
                    );

                    self.parts.push(curr_part);
                }

                index = boundary_index + self.boundary.len() + 2 + 2;

                self.state = MultiPartParserState::AfterBoundary;
            }
        }

        Ok(())
    }

    fn append(&mut self, chunk: &[u8]) -> Result<(), MultiPartParserError> {
        self.current_part.write(chunk)?;
        Ok(())
    }
}

#[derive(Debug)]
enum MultiPartParserError {
    IOError(Error),
    UnexpectedDataAtEndOfStream,
    InvalidCharacterInBoundary,
    MissingInitialBoundary,
    MalformedMultiPartBoundary,
    MaxHeaderLimitExceeded,
    MaxFileSizeExceededError,
    MaxTotalSizeExceededError,
    MaliciousPart,
    TempFileError(),
}

#[derive(PartialEq)]
enum MultiPartParserState {
    Start,
    Done,
    AfterBoundary,
    Body,
    Boundary,
    Header,
}

fn find_double_newline(chunk: &[u8], index: usize) -> Option<usize> {
    chunk[index..]
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| index + pos)
}

fn substring_search(haystack: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    let needle_end: u8 = needle.len() as u8 - 1;
    let mut skip_table = vec![needle.len(); 256];

    for i in 0..needle_end {
        skip_table[needle[i as usize] as usize] = (needle_end - i) as usize;
    }

    let haystack_length = haystack.len();
    let mut i = start + needle_end as usize;

    while (i as usize) < haystack_length {
        let mut j = needle_end;
        let mut k = i;

        while (j >= 0) && haystack[k as usize] == needle[j as usize] {
            if j == 0 {
                return Some(k as usize);
            }
            j -= 1;
            k -= 1;
        }

        i += skip_table[haystack[i as usize] as usize] as usize;
    }

    return None;
}

fn substring_partial_search(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if haystack.is_empty() || needle.is_empty() {
        return None;
    }

    let mut byte_indexes = HashMap::<u8, Vec<usize>>::new();
    for (i, &byte) in needle.iter().enumerate() {
        byte_indexes.entry(byte).or_insert_with(Vec::new).push(i);
    }

    let haystack_end = haystack.len() - 1;

    if let Some(indexes) = byte_indexes.get(&haystack[haystack_end]) {
        for &i in indexes.iter().rev() {
            let mut j = i;
            let mut k = haystack_end;

            loop {
                if haystack[k] != needle[j] {
                    break;
                }
                if j == 0 {
                    return Some(k);
                }
                j -= 1;
                k -= 1;
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;
    const MOCK_MULTIPART_PAYLOAD: &[u8] = b"------WebKitFormBoundary7MA4YWxkTrZu0gW\r\nContent-Disposition: form-data; name=\"username\"\r\n\r\njohn_doe\r\n------WebKitFormBoundary7MA4YWxkTrZu0gW\r\nContent-Disposition: form-data; name=\"profile_picture\"; filename=\"profile.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n\xFF\xD8\xFF\xE0\x00\x10JFIF\x00\x01\x01\x01\x00\x60\x00\x60\x00\x00\xFF\xDB\x00\x43\x00\x08\x06\x06\r\n------WebKitFormBoundary7MA4YWxkTrZu0gW\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: application/json\r\n\r\n{\"age\": 30, \"location\": \"New York\"}\r\n------WebKitFormBoundary7MA4YWxkTrZu0gW--\r\n";
    const MOCK_NESTED_MULTIPART_PAYLOAD: &[u8] = b"------OuterBoundary7f3a9c2e\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: application/json\r\n\r\n{\"user_id\": 4821, \"action\": \"bulk_upload\", \"tags\": [\"invoice\", \"2026\", \"q3\"]}\r\n------OuterBoundary7f3a9c2e\r\nContent-Disposition: form-data; name=\"description\"\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThis upload contains a quarterly invoice batch along with its supporting attachments.\r\n------OuterBoundary7f3a9c2e\r\nContent-Disposition: form-data; name=\"attachments\"\r\nContent-Type: multipart/mixed; boundary=InnerBoundary1d5e8b41\r\n\r\n--InnerBoundary1d5e8b41\r\nContent-Disposition: attachment; filename=\"invoice_q3.pdf\"\r\nContent-Type: application/pdf\r\n\r\n%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n%%EOF\r\n--InnerBoundary1d5e8b41\r\nContent-Disposition: attachment; filename=\"receipt_scan.png\"\r\nContent-Type: image/png\r\n\r\n\xFF\xD8\xFF\xE0\x00\x10JFIF\x00\x01\x01\x01\x00\x60\x00\x60\x00\x00\xFF\xDB\x00\x43\x00\x08\x06\x06\r\n--InnerBoundary1d5e8b41\r\nContent-Disposition: attachment; filename=\"notes.txt\"\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nScanned receipt is slightly faded on the top-left corner, but totals are legible.\r\n--InnerBoundary1d5e8b41--\r\n\r\n------OuterBoundary7f3a9c2e\r\nContent-Disposition: form-data; name=\"submitted_by\"\r\nContent-Type: text/plain; charset=utf-8\r\n\r\njane.doe@example.com\r\n------OuterBoundary7f3a9c2e--\r\n";
    // 1. Empty body value (field with no content at all)
    const MOCK_EMPTY_FIELD_PAYLOAD: &[u8] = b"------EmptyBoundary123\r\nContent-Disposition: form-data; name=\"empty_field\"\r\n\r\n\r\n------EmptyBoundary123--\r\n";

    // 2. Multiple files in one payload (no text fields at all)
    const MOCK_MULTI_FILE_PAYLOAD: &[u8] = b"------MultiFileBoundaryAbc1\r\nContent-Disposition: form-data; name=\"file1\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nHello from file one.\r\n------MultiFileBoundaryAbc1\r\nContent-Disposition: form-data; name=\"file2\"; filename=\"b.txt\"\r\nContent-Type: text/plain\r\n\r\nHello from file two.\r\n------MultiFileBoundaryAbc1--\r\n";

    // 3. Large single field that should exceed max_body_limit_until_file and get promoted to a temp file
    //    (repeats "0123456789" 2000 times = 20,000 bytes)
    fn build_large_field_payload() -> Vec<u8> {
        let boundary = "----LargeFieldBoundaryXYZ";
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"big_text\"\r\n");
        body.extend_from_slice(b"Content-Type: text/plain\r\n\r\n");
        for _ in 0..2000 {
            body.extend_from_slice(b"0123456789");
        }
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
        body
    }

    // 4. Field name with unicode content
    const MOCK_UNICODE_PAYLOAD: &[u8] = b"------UnicodeBoundary456\r\nContent-Disposition: form-data; name=\"greeting\"\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\xE4\xBD\xA0\xE5\xA5\xBD, world! \xF0\x9F\x8C\x8D\r\n------UnicodeBoundary456--\r\n";

    // 5. Headers with extra/uncommon casing and additional custom header
    const MOCK_CUSTOM_HEADERS_PAYLOAD: &[u8] = b"------CustomHeaderBoundary789\r\ncontent-disposition: form-data; name=\"custom\"\r\nX-Custom-Meta: some-value\r\nContent-Type: text/plain\r\n\r\nField with custom headers.\r\n------CustomHeaderBoundary789--\r\n";

    #[test]
    fn parse_simple_multipart() {
        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW".to_owned();

        let mut parser = MultiPartParser::new(1024, 1024, 8 * 1024, boundary);

        let mut chunk = [0u8; 80];

        let v = Vec::from(MOCK_MULTIPART_PAYLOAD);
        let mut slice = &v[..];
        loop {
            let bytes_read = slice.read(&mut chunk).unwrap();

            if bytes_read == 0 {
                break;
            }

            parser.parse(&chunk[..bytes_read]).unwrap();
        }

        for ref part in parser.parts {
            println!("Here are the parts {}", part);
        }
    }
    #[test]
    fn parse_nested_multipart() {
        let boundary = "----OuterBoundary7f3a9c2e".to_owned();

        let mut parser = MultiPartParser::new(1024, 1024, 8 * 1024, boundary);

        let mut chunk = [0u8; 80];

        let v = Vec::from(MOCK_NESTED_MULTIPART_PAYLOAD);
        let mut slice = &v[..];
        loop {
            let bytes_read = slice.read(&mut chunk).unwrap();

            if bytes_read == 0 {
                break;
            }

            parser.parse(&chunk[..bytes_read]).unwrap();
        }

        for ref part in parser.parts {
            println!("Here are the Nested parts {}", part);
        }
    }

    #[test]
    fn parse_empty_field() {
        let boundary = "----EmptyBoundary123".to_owned();
        let mut parser = MultiPartParser::new(1024, 1024, 8 * 1024, boundary);

        let mut chunk = [0u8; 80];
        let v = Vec::from(MOCK_EMPTY_FIELD_PAYLOAD);
        let mut slice = &v[..];
        loop {
            let bytes_read = slice.read(&mut chunk).unwrap();
            if bytes_read == 0 {
                break;
            }
            parser.parse(&chunk[..bytes_read]).unwrap();
        }

        for ref part in parser.parts {
            println!("Empty field part: {}", part);
        }
    }

    #[test]
    fn parse_multiple_files() {
        let boundary = "----MultiFileBoundaryAbc1".to_owned();
        let mut parser = MultiPartParser::new(1024, 1024, 8 * 1024, boundary);

        let mut chunk = [0u8; 64];
        let v = Vec::from(MOCK_MULTI_FILE_PAYLOAD);
        let mut slice = &v[..];
        loop {
            let bytes_read = slice.read(&mut chunk).unwrap();
            if bytes_read == 0 {
                break;
            }
            parser.parse(&chunk[..bytes_read]).unwrap();
        }

        assert_eq!(parser.parts.len(), 2);
        for ref part in parser.parts {
            println!("Multi-file part: {}", part);
        }
    }

    #[test]
    fn parse_large_field_promotes_to_file() {
        let boundary = "----LargeFieldBoundaryXYZ".to_owned();
        // max_body_limit_until_file is small (1024) so this 20,000-byte field
        // should get promoted to a temp file partway through.
        let mut parser = MultiPartParser::new(1024, 1024, 8 * 1024, boundary);

        let payload = build_large_field_payload();
        let mut chunk = [0u8; 256];
        let mut slice = &payload[..];
        loop {
            let bytes_read = slice.read(&mut chunk).unwrap();
            if bytes_read == 0 {
                break;
            }
            parser.parse(&chunk[..bytes_read]).unwrap();
        }

        for ref part in parser.parts {
            println!("Large field part: {}", part);
            // Optionally assert it became a File variant, e.g.:
            // assert!(matches!(part.data, Some(DataType::File(_))));
        }
    }

    #[test]
    fn parse_unicode_field() {
        let boundary = "----UnicodeBoundary456".to_owned();
        let mut parser = MultiPartParser::new(1024, 1024, 8 * 1024, boundary);

        let mut chunk = [0u8; 32]; // small chunk size to force unicode bytes to split across reads
        let v = Vec::from(MOCK_UNICODE_PAYLOAD);
        let mut slice = &v[..];
        loop {
            let bytes_read = slice.read(&mut chunk).unwrap();
            if bytes_read == 0 {
                break;
            }
            parser.parse(&chunk[..bytes_read]).unwrap();
        }

        for ref part in parser.parts {
            println!("Unicode part: {}", part);
        }
    }

    #[test]
    fn parse_custom_headers() {
        let boundary = "----CustomHeaderBoundary789".to_owned();
        let mut parser = MultiPartParser::new(1024, 1024, 8 * 1024, boundary);

        let mut chunk = [0u8; 80];
        let v = Vec::from(MOCK_CUSTOM_HEADERS_PAYLOAD);
        let mut slice = &v[..];
        loop {
            let bytes_read = slice.read(&mut chunk).unwrap();
            if bytes_read == 0 {
                break;
            }
            parser.parse(&chunk[..bytes_read]).unwrap();
        }

        for ref part in parser.parts {
            println!("Custom header part: {}", part);
        }
    }
}
