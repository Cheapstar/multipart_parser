use std::io::Error;

use crate::{
    multipart::MultiPart,
    search::{find_double_newline, substring_partial_search, substring_search},
};

#[derive(Debug)]
pub enum MultiPartParserError {
    IOError(Error),
    UnexpectedDataAtEndOfStream,
    InvalidCharacterInBoundary,
    MissingInitialBoundary,
    MalformedMultiPartBoundary,
    MaxHeaderLimitExceeded,
    MaxFileSizeExceededError,
    MaxTotalSizeExceededError,
    MaliciousPart,
    UnfinishedPart,
    TempFileError(),
}

#[derive(PartialEq)]
enum MultiPartParserState {
    Start,
    Done,
    AfterBoundary,
    Body,
    Header,
}

pub struct MultiPartParser {
    max_header_size: u16,
    max_body_limit_until_file: u32,
    max_file_size: u32,

    boundary: String,
    boundary_pattern: String,
    opening_boundary_length: u16,
    buffer: Vec<u8>,
    current_part: MultiPart,
    state: MultiPartParserState,
    child_parser: Option<Box<MultiPartParser>>,
    index_after_child: Option<usize>,
    parts: Vec<MultiPart>,
}

impl MultiPartParser {
    /// Creates a new instance of the multipart parser.
    ///
    /// # Arguments
    ///
    /// * `max_header_size`(bytes) - Maximum header size allowed. Exceeding this will throw an error.
    /// * `max_body_limit_until_file`(bytes) - Maximum size the body is allowed to remain in memory
    ///   (buffer) before being transferred to a file.
    /// * `max_file_size`(bytes) - Maximum allowed size for a received file.
    /// * `boundary` - The multipart boundary string.
    ///
    /// # Returns
    ///
    /// A new `MultipartParser` instance.
    pub fn new(
        max_header_size: u16,
        max_body_limit_until_file: u32,
        max_file_size: u32,

        boundary: String,
    ) -> Self {
        let opening_boundary_length: u16 = boundary.len() as u16 + 2; // --boundary
        let boundary_pattern = format!("\r\n--{boundary}");

        Self {
            max_header_size,
            max_body_limit_until_file,
            max_file_size,
            opening_boundary_length,
            boundary,
            boundary_pattern,
            state: MultiPartParserState::Start,
            buffer: Vec::<u8>::new(),
            child_parser: None,
            current_part: MultiPart::new(max_body_limit_until_file, max_file_size),
            parts: Vec::<MultiPart>::new(),
            index_after_child: None,
        }
    }

    /// Write a chunk of data to the parser.
    ///
    /// # Arguments
    ///
    /// * `chunk` - Chunk of body bytes to be parsed.
    ///
    /// # Errors
    ///
    /// Returns `MultiPartParserError`
    ///
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use std::io::Read;
    /// let mut parser = MultiPartParser::new(1024, 1024, 8 * 1024, boundary);
    /// let mut chunk = [0u8; 80];
    ///
    /// let v = Vec::from(MOCK_MULTIPART_PAYLOAD);
    /// let mut slice = &v[..];
    /// loop {
    ///     let bytes_read = slice.read(&mut chunk).unwrap();
    ///     if bytes_read == 0 {
    ///         break;
    ///     }
    ///     parser.parse(&chunk[..bytes_read]).unwrap();
    /// }
    ///
    /// let parts = parser.get_parts();
    /// ```
    pub fn parse(&mut self, chunk: &[u8]) -> Result<(), MultiPartParserError> {
        // Malicious Body
        if self.state == MultiPartParserState::Done {
            return Err(MultiPartParserError::UnexpectedDataAtEndOfStream);
        }

        // this holds the `previous unparsed data` + `current chunk` data
        let mut new_chunk = Vec::<u8>::new();

        // this is used as the `cursor` in the unparsed chunk
        let mut index: usize = 0;

        if self.child_parser.is_none() {
            if !self.buffer.is_empty() {
                // there is unparsed chunk from previous call
                // append that to new_chunk

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
                        // this section boundary is present in the current_chunk but the child_parser is unfinished
                        // so save that chunk for later processing
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
                    // chunk does not contain enough data for boundary
                    self.buffer.extend_from_slice(&new_chunk);
                    break;
                }

                // since this is the very first boundary we have to use it from 2.. onwards for boundary_patter
                if substring_search(&new_chunk, index, self.boundary_pattern[2..].as_bytes())
                    .is_none()
                {
                    return Err(MultiPartParserError::MissingInitialBoundary);
                }
                index = self.opening_boundary_length as usize; // this is the index of "\r" - start of boundary
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

                // double_newline cuz it the last header's end and then the empty line separating header and body
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
                            let mut child_parts = child_parser.get_parts()?;
                            self.parts.append(&mut child_parts);

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

    /// # Returns
    ///     All the vector of all parsed parts or `MultipartParserError`  
    pub fn get_parts(&mut self) -> Result<Vec<MultiPart>, MultiPartParserError> {
        if self.state != MultiPartParserState::Done {
            return Err(MultiPartParserError::UnfinishedPart);
        }

        let number_of_parts = self.parts.len();
        let mut ret: Vec<MultiPart> = Vec::with_capacity(number_of_parts);
        ret.append(&mut self.parts);

        Ok(ret)
    }
}
