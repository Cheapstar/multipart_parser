# multipart_parser

A streaming, memory-safe parser for `multipart/*` request bodies, written in Rust.

Data is fed in incrementally in chunks rather than requiring the whole body to be loaded into memory. Small parts stay in memory; larger parts are automatically spilled to a temporary file once configurable size thresholds are exceeded, protecting against unbounded memory growth from large or malicious uploads.

## Features

- **Streaming / chunked input** — feed data as it arrives, no need to buffer the entire request body first.
- **Configurable size limits** — cap header size, in-memory body size, and maximum file size independently.
- **Automatic disk spillover** — large parts are written to a temporary file instead of exhausting memory.
- **Descriptive errors** — a dedicated `MultiPartParserError` type covering malformed boundaries, size-limit violations, and malicious input.

## Installation

Run:

```bash
cargo add multipart_parser
```

Or add it to your `Cargo.toml` manually:

```toml
[dependencies]
multipart_parser = "0.1"
```

## Usage

```rust
use std::io::Read;
use multipart_parser::MultiPartParser;

let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW".to_string();
let mut parser = MultiPartParser::new(1024, 1024, 8 * 1024, boundary);

let mut chunk = [0u8; 80];
let mut source = &payload[..]; // any `Read` source of multipart bytes

loop {
    let bytes_read = source.read(&mut chunk)?;
    if bytes_read == 0 {
        break;
    }
    parser.parse(&chunk[..bytes_read])?;
}

let parts:Vec<Multipart> = parser.get_parts();
```

## Configuration

`MultiPartParser::new` takes four parameters:

| Parameter                   | Type     | Description                                                                                            |
| --------------------------- | -------- | ------------------------------------------------------------------------------------------------------ |
| `max_header_size`           | `u16`    | Max size (bytes) allowed for a part's header section before an error is returned.                      |
| `max_body_limit_until_file` | `u32`    | Max size (bytes) a part's body may reach while held in memory before it's spilled to a temporary file. |
| `max_file_size`             | `u32`    | Max total size (bytes) allowed for a single file part.                                                 |
| `boundary`                  | `String` | The multipart boundary string used to separate parts.                                                  |

## The `MultiPart` type

Each parsed section of the body is represented by a `MultiPart`:

```rust
pub struct MultiPart {
    pub headers: Option<HashMap<String, String>>,
    pub data: Option<DataType>,
    pub content_type: Option<String>,
    // ...
}
```

| Field          | Description                                                                                                   |
| -------------- | ------------------------------------------------------------------------------------------------------------- |
| `headers`      | The part's headers (e.g. `Content-Disposition`), keyed by header name. `None` until headers have been parsed. |
| `data`         | The part's body data. See [`DataType`](#the-datatype-enum) below. `None` until data has been parsed.          |
| `content_type` | The part's `Content-Type`, if one was specified in its headers.                                               |

```rust
for part in parser.get_parts() {
    if let Some(content_type) = &part.content_type {
        println!("part content-type: {content_type}");
    }
}
```

## The `DataType` enum

A part's data is held as one of two variants, depending on its size relative to `max_body_limit_until_file`:

```rust
pub enum DataType {
    Bytes(Vec<u8>),
    File(NamedTempFile),
}
```

| Variant               | Description                                                                                          |
| --------------------- | ---------------------------------------------------------------------------------------------------- |
| `Bytes(Vec<u8>)`      | The part's data held entirely in memory, used when the part stays under `max_body_limit_until_file`. |
| `File(NamedTempFile)` | The part's data spilled to a temporary file on disk, used once the in-memory limit is exceeded.      |

Matching on it looks like:

```rust
match &part.data {
    Some(DataType::Bytes(bytes)) => {
        println!("in-memory part, {} bytes", bytes.len());
    }
    Some(DataType::File(file)) => {
        println!("part spilled to temp file at {:?}", file.path());
    }
    None => {
        println!("part has no data yet");
    }
}
```

## Error Handling

All parsing failures are returned as a [`MultiPartParserError`], which implements `std::error::Error` and `Display`:

```rust
match parser.parse(&chunk) {
    Ok(_) => {}
    Err(e) => eprintln!("parse failed: {e}"),
}
```

Variants include size-limit violations (`MaxHeaderLimitExceeded`, `MaxFileSizeExceededError`), malformed input (`InvalidCharacterInBoundary`, `MalformedMultiPartBoundary`, `MissingInitialBoundary`), stream issues (`UnexpectedDataAtEndOfStream`, `UnfinishedPart`), and `MaliciousPart` for detected abuse patterns.

## Documentation

Full API documentation is available on [docs.rs](https://docs.rs/multipart_parser).

## License

Licensed under

- Apache License, Version 2.0

## Contributing

Issues and pull requests are welcome. Please open an issue to discuss significant changes before submitting a PR.
