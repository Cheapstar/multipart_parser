//! A streaming, memory-safe parser for `multipart/*` request bodies.
//!
//! `multipart_parser` is designed to be fed data incrementally, in chunks,
//! rather than requiring the entire request body to be buffered in memory
//! up front. Small parts are kept in memory, while larger ones are
//! automatically spilled to a temporary file once configurable size
//! thresholds are exceeded — guarding against unbounded memory growth from
//! large or malicious uploads.
//!
//!
//! Originally inspired by [remix-run/multipart-parser], a TypeScript
//! implementation focused on `multipart/form-data`. This crate ports the
//! core approach to Rust and extends it to handle `multipart/*` content
//! types more generally, rather than being limited to form data.
//!
//! [remix-run/multipart-parser]: https://github.com/remix-run/remix/tree/main/packages/multipart-parser
//!
//! # Example
//!
//! ```no_run
//! use std::io::Read;
//! use multipart_parser::MultiPartParser;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW".to_string();
//! let mut parser = MultiPartParser::new(1024, 1024, 8 * 1024, boundary);
//!
//! let mut chunk = [0u8; 80];
//! let mut source: &[u8] = &[]; // any `Read` source of multipart bytes
//!
//! loop {
//!     let bytes_read = source.read(&mut chunk)?;
//!     if bytes_read == 0 {
//!         break;
//!     }
//!     parser.parse(&chunk[..bytes_read])?;
//! }
//!
//! let parts = parser.get_parts();
//! # Ok(())
//! # }
//! ```
//!
//! # Size limits
//!
//! Three limits are configured up front via [`MultiPartParser::new`]:
//!
//! - `max_header_size` — maximum size of a part's header section.
//! - `max_body_limit_until_file` — maximum in-memory size before a part's
//!   body is spilled to disk.
//! - `max_file_size` — maximum total size allowed for a file part.
//!
//! # Errors
//!
//! Parsing failures are reported via [`MultiPartParserError`], which
//! implements [`std::error::Error`].

pub mod multipart;
pub mod parser;
mod search;

#[cfg(test)]
mod tests {
    use crate::parser::MultiPartParser;
    use std::io::Read;
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
        println!("-----------------Simple-Multipart Test------------------------------");

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

        let parts = parser
            .get_parts()
            .expect("Error Occured While Extracting Parts.");
        for ref part in parts {
            println!("Simple Multipart {}", part);
        }
    }
    #[test]
    fn parse_nested_multipart() {
        println!("-----------------Nested-Multipart Test------------------------------");

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

        let parts = parser
            .get_parts()
            .expect("Error Occured While Extracting Parts.");
        for ref part in parts {
            println!("Here are the Nested parts {}", part);
        }
    }

    #[test]
    fn parse_empty_field() {
        println!("-----------------Empty-Field Test------------------------------");

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
        let parts = parser
            .get_parts()
            .expect("Error Occured While Extracting Parts.");
        for ref part in parts {
            println!("Empty Multipart {}", part);
        }
    }

    #[test]
    fn parse_multiple_files() {
        println!("-----------------Multi-File Test------------------------------");

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

        let parts = parser
            .get_parts()
            .expect("Error Occured While Extracting Parts.");
        assert_eq!(parts.len(), 2);
        for ref part in parts {
            println!("Multi-File Part {}", part);
        }
    }

    #[test]
    fn parse_large_field_promotes_to_file() {
        println!("-----------------Large Field Test------------------------------");

        let boundary = "----LargeFieldBoundaryXYZ".to_owned();
        // max_body_limit_until_file is small (1024) so this 20,000-byte field
        // should get promoted to a temp file partway through.
        let mut parser = MultiPartParser::new(1024, 1024, 30000, boundary);

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

        let parts = parser
            .get_parts()
            .expect("Error Occured While Extracting Parts.");
        for ref part in parts {
            println!("Large Field {}", part);
        }
    }

    #[test]
    fn parse_unicode_field() {
        println!("-----------------Unicode Test------------------------------");

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

        let parts = parser
            .get_parts()
            .expect("Error Occured While Extracting Parts.");
        for ref part in parts {
            println!("Unicode {}", part);
        }
    }

    #[test]
    fn parse_custom_headers() {
        println!("-----------------CustomHeadersTest------------------------------");

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

        let parts = parser
            .get_parts()
            .expect("Error Occured While Extracting Parts.");
        for ref part in parts {
            println!("Custom {}", part);
        }
    }
}
