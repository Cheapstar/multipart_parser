use std::{
    collections::HashMap,
    fmt,
    io::{BufRead, Error, Read, Seek, SeekFrom, Write},
};

use tempfile::NamedTempFile;

use crate::{
    multipart::MultiPart,
    search::{find_double_newline, substring_partial_search, substring_search},
};

#[derive(Debug)]

pub enum DataType {
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
    TempFileError(),
}

#[derive(PartialEq)]
pub enum MultiPartParserState {
    Start,
    Done,
    AfterBoundary,
    Body,
    Boundary,
    Header,
}

pub struct MultiPartParser {
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
    index_after_child: Option<usize>,
    pub parts: Vec<MultiPart>,
}

impl MultiPartParser {
    pub fn new(
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

    pub fn parse(&mut self, chunk: &[u8]) -> Result<(), MultiPartParserError> {
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

    pub fn append(&mut self, chunk: &[u8]) -> Result<(), MultiPartParserError> {
        self.current_part.write(chunk)?;
        Ok(())
    }
}
