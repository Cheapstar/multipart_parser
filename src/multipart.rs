use std::{
    collections::HashMap,
    fmt,
    io::{BufRead, Read, Seek, SeekFrom, Write},
};

use tempfile::NamedTempFile;

use crate::parser::MultiPartParserError;

///
///   This Represents the type of data that will multipart will contain.Meaning as the name suggests
///        1. Bytes ( vector of bytes )
///        2. File  ( NamedTempFile )
#[derive(Debug)]

pub enum DataType {
    Bytes(Vec<u8>),
    File(NamedTempFile),
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
pub struct MultiPart {
    pub headers: Option<HashMap<String, String>>,
    pub data: Option<DataType>,
    pub content_type: Option<String>,
    pub max_body_limit_until_file: u32,
    pub max_file_size: u32,
}

impl MultiPart {
    pub fn new(max_body_limit_until_file: u32, max_file_size: u32) -> Self {
        Self {
            headers: None,
            data: None,
            content_type: None,
            max_body_limit_until_file,
            max_file_size,
        }
    }

    pub fn set_headers(&mut self, header_bytes: &[u8]) {
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

    pub fn write(&mut self, chunk: &[u8]) -> Result<(), MultiPartParserError> {
        let headers = self.headers.as_ref().unwrap();
        let is_file_part = headers
            .get("content-disposition")
            .map(|cd| cd.contains("filename="))
            .unwrap_or(false);

        match self.data.take() {
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
