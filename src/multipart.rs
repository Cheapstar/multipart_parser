use std::{
    collections::HashMap,
    fmt,
    io::{BufRead, Read, Seek, SeekFrom, Write},
};

use tempfile::NamedTempFile;

use crate::parser::MultiPartParserError;

/// Represents the type of data a [`MultiPart`] contains.
///
/// A part's data is held as one of the following, depending on its size:
///
/// 1. [`Bytes`](DataType::Bytes) — held entirely in memory as a `Vec<u8>`.
/// 2. [`File`](DataType::File) — held in `NamedTempFile`.
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

/// Represents a single part of a multipart body.
#[derive(Debug)]
pub struct MultiPart {
    /// Headers associated with this part (e.g. `Content-Disposition`,
    /// `Content-Type`), keyed by header name.
    /// This is Option Because it is allowed by RFC for headers to be empty
    pub headers: Option<HashMap<String, String>>,
    /// The body data for this part. `None` until data has been parsed.
    /// See [`DataType`] for the possible in-memory vs. on-disk forms.
    pub data: Option<DataType>,
    /// The `Content-Type` of this part, if one was specified in its
    /// headers.
    pub content_type: Option<String>,

    /// Maximum size this part's body may reach while held in memory
    /// before being spilled to a temporary file.
    max_body_limit_until_file: u32,
    /// Maximum total size allowed for this part if it is a file.
    max_file_size: u32,
}

impl MultiPart {
    /// Creates a new, empty `MultiPart` with the given size limits.
    pub(crate) fn new(max_body_limit_until_file: u32, max_file_size: u32) -> Self {
        Self {
            headers: None,
            data: None,
            content_type: None,
            max_body_limit_until_file,
            max_file_size,
        }
    }

    /// Parses the given raw header bytes and populates `self.headers`
    pub(crate) fn set_headers(&mut self, header_bytes: &[u8]) {
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

    /// Populates to the current body of the Multipart as per the [`DataType`]
    pub(crate) fn write(&mut self, chunk: &[u8]) -> Result<(), MultiPartParserError> {
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
                        .tempfile()
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
                        .tempfile()
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
                let curr_size = temp_file
                    .as_file()
                    .metadata()
                    .map_err(|e| MultiPartParserError::IOError(e))?
                    .len() as usize;

                if curr_size + chunk.len() > (self.max_file_size as usize) {
                    return Err(MultiPartParserError::MaxFileSizeExceededError);
                }

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
