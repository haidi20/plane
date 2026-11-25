// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `multipart/form-data`, ported from Django's `MultiPartParser`.
//!
//! Django iterates over *encapsulations*, not over parts: the bytes before the
//! first boundary and after the last one are parts too (`RAW`). That is why
//! `DATA_UPLOAD_MAX_NUMBER_FIELDS` is compared against `max_fields + 2`, and
//! why a body without any boundary is accepted with an empty `request.POST`
//! instead of an error.
//!
//! The other observable rules:
//!
//! * the boundary must fullmatch `[ -~]{0,200}[!-~]`;
//! * a header block has to end within the first 1024 bytes; chunk sizes double
//!   up to that limit before Django gives up with `Request max total header
//!   size exceeded.`;
//! * `DATA_UPLOAD_MAX_MEMORY_SIZE` counts **fields only** (`len(name) + 2` per
//!   value as well), so a large file succeeds where a large field fails;
//! * `DATA_UPLOAD_MAX_NUMBER_FILES` counts only parts with a non-empty
//!   `filename`;
//! * filenames are HTML-unescaped, stripped of directories, filtered to
//!   printable characters, and dropped when they collapse to `.`/`..`;
//! * an invalid `base64` field falls back to its raw bytes, while an invalid
//!   `base64` file fails the request.
//!
//! Errors are split the way Django splits them: `MultiPartParserError` becomes
//! `Multipart form parse error - ...` and is answered by DRF as JSON, while
//! `RequestDataTooBig`, `TooManyFieldsSent`, and `TooManyFilesSent` are
//! `SuspiciousOperation`s that Django answers with its HTML 400 page.

use super::charset::Charset;
use super::content_type::parse_header_parameters;
use super::{ParserError, ParserLimits};

mod multipart_table;
use multipart_table::PRINTABLE_RANGES;

/// A file part, with its bytes kept so callers can store them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedFile {
    /// Field name from `Content-Disposition`.
    pub field: String,
    /// Sanitised filename.
    pub filename: String,
    /// The part's `Content-Type`, defaulting to `""`.
    pub content_type: String,
    /// Number of bytes in [`Self::bytes`].
    pub size: usize,
    /// Raw file content.
    pub bytes: Vec<u8>,
}

/// A parsed multipart body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MultipartBody {
    /// `request.POST`.
    pub fields: super::FormFields,
    /// `request.FILES`.
    pub files: Vec<UploadedFile>,
}

/// Django's `MAX_TOTAL_HEADER_SIZE`.
const MAX_TOTAL_HEADER_SIZE: usize = 1024;
/// The first chunk `parse_boundary_stream` reads; it doubles on each retry.
const INITIAL_HEADER_CHUNK: usize = 1024;
const CRLF_CRLF: &[u8] = b"\r\n\r\n";

/// Django's `RAW`/`FIELD`/`FILE` classification for one encapsulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartKind {
    Raw,
    Field,
    File,
}

/// Parse a multipart body.
///
/// `raw_content_type` is the full header (the boundary lives in its parameters)
/// and `content_length` is the already-parsed `Content-Length`.
///
/// # Errors
/// Returns the same [`ParserError`] variants Django/DRF surface.
pub fn parse(
    raw_content_type: &str,
    content_length: i64,
    body: &[u8],
    charset: Charset,
    limits: ParserLimits,
) -> Result<MultipartBody, ParserError> {
    let boundary = boundary_for(raw_content_type)?;
    if content_length == 0 {
        return Ok(MultipartBody::default());
    }
    if content_length < 0 {
        return Err(ParserError::Multipart(format!(
            "Invalid content length: {content_length}"
        )));
    }
    // Django wraps the stream in `LimitedStream(..., CONTENT_LENGTH)`.
    let limit = usize::try_from(content_length)
        .unwrap_or(body.len())
        .min(body.len());
    let body = &body[..limit];
    let mut separator = Vec::with_capacity(boundary.len() + 2);
    separator.extend_from_slice(b"--");
    separator.extend_from_slice(boundary.as_bytes());

    let mut parser = BodyParser {
        charset,
        limits,
        fields: super::FormFields::new(),
        files: Vec::new(),
        bytes_read: 0,
        parts: 0,
        files_seen: 0,
        pending_file: None,
    };
    let mut offset = 0usize;
    while offset < body.len() {
        let next = parser.next_encapsulation(body, &separator, offset)?;
        if next <= offset {
            break;
        }
        offset = next;
    }
    Ok(MultipartBody {
        fields: parser.fields,
        files: parser.files,
    })
}

fn boundary_for(raw_content_type: &str) -> Result<String, ParserError> {
    if !raw_content_type.starts_with("multipart/") {
        return Err(ParserError::Multipart(format!(
            "Invalid Content-Type: {raw_content_type}"
        )));
    }
    if !raw_content_type.is_ascii() {
        return Err(ParserError::Multipart(format!(
            "Invalid non-ASCII Content-Type in multipart: {raw_content_type}"
        )));
    }
    let parsed = parse_header_parameters(raw_content_type);
    // Django renders a missing parameter as the string `None`, because it
    // calls `force_str(boundary)` on the `None` returned by `opts.get`.
    let Some(boundary) = parsed.params.get("boundary").cloned() else {
        return Err(ParserError::Multipart(
            "Invalid boundary in multipart: None".to_string(),
        ));
    };
    if !valid_boundary(&boundary) {
        return Err(ParserError::Multipart(format!(
            "Invalid boundary in multipart: {boundary}"
        )));
    }
    Ok(boundary)
}

/// `MultiPartParser.boundary_re.fullmatch(r"[ -~]{0,200}[!-~]")`.
fn valid_boundary(boundary: &str) -> bool {
    let bytes = boundary.as_bytes();
    if bytes.is_empty() || bytes.len() > 201 {
        return false;
    }
    if !bytes.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
        return false;
    }
    (b'!'..=b'~').contains(&bytes[bytes.len() - 1])
}

struct BodyParser {
    charset: Charset,
    limits: ParserLimits,
    fields: super::FormFields,
    files: Vec<UploadedFile>,
    bytes_read: usize,
    parts: usize,
    files_seen: usize,
    /// Django signals file completion at the **start of the next iteration**,
    /// because a boundary is the only proof that a file is complete. Holding
    /// the file here reproduces that; a final file with no closing boundary is
    /// therefore dropped.
    pending_file: Option<UploadedFile>,
}

impl BodyParser {
    /// Consume the encapsulation that starts at `offset` and return the offset
    /// where the next one starts.
    ///
    /// `BoundaryIter` does not special-case the closing `--`; it simply looks
    /// for the separator again, so the bytes after `--boundary--` (at least
    /// `--`) become a trailing `RAW` part. That is what completes a file that
    /// sits just before the close.
    fn next_encapsulation(
        &mut self,
        body: &[u8],
        separator: &[u8],
        offset: usize,
    ) -> Result<usize, ParserError> {
        let Some(found) = find(&body[offset..], separator).map(|index| offset + index) else {
            if offset < body.len() {
                self.raw_part(&body[offset..])?;
            }
            return Ok(body.len());
        };
        // `_find_boundary` backs over the CRLF that belongs to the delimiter,
        // not to the payload.
        let mut end = found;
        if end > offset && body[end - 1] == b'\n' {
            end -= 1;
        }
        if end > offset && body[end - 1] == b'\r' {
            end -= 1;
        }
        self.raw_part(&body[offset..end])?;
        Ok(found + separator.len())
    }

    fn raw_part(&mut self, payload: &[u8]) -> Result<(), ParserError> {
        if let Some(file) = self.pending_file.take() {
            self.files.push(file);
        }
        let (kind, part) = parse_part(payload, self.charset)?;
        // `DATA_UPLOAD_MAX_NUMBER_FIELDS` counts `RAW` and `FIELD` only:
        // Django's `FIELD_TYPES` leaves `FILE` out, so a body made of files
        // hits `DATA_UPLOAD_MAX_NUMBER_FILES` instead. `max_fields + 2`
        // accounts for the two raw parts Django always sees, before the first
        // boundary and after the close.
        if matches!(kind, PartKind::Raw | PartKind::Field) {
            self.parts += 1;
            if self.parts > self.limits.max_fields + 2 {
                return Err(ParserError::TooManyFields);
            }
        }
        match kind {
            PartKind::Raw => Ok(()),
            PartKind::Field => {
                let Some(name) = part.field_name else {
                    return Ok(());
                };
                self.handle_field(&name, part.transfer_encoding.as_deref(), part.body)
            }
            PartKind::File => {
                let Some(field) = part.field_name else {
                    return Ok(());
                };
                self.handle_file(
                    field,
                    part.filename.unwrap_or_default(),
                    part.content_type.unwrap_or_default(),
                    part.transfer_encoding.as_deref(),
                    part.body,
                )
            }
        }
    }

    fn handle_field(
        &mut self,
        name: &str,
        transfer_encoding: Option<&str>,
        payload: &[u8],
    ) -> Result<(), ParserError> {
        let (data, raw_len) = if transfer_encoding == Some("base64") {
            let stripped = strip_whitespace(payload);
            match decode_base64(&stripped) {
                Ok(decoded) => (decoded, payload.len()),
                // Django falls back to the raw bytes for fields.
                Err(()) => (payload.to_vec(), payload.len()),
            }
        } else {
            (payload.to_vec(), payload.len())
        };
        // Django adds `len(field_name) + 2` to the same budget as the bytes.
        // Reading past the budget always pushes this check over the limit, so
        // reading the whole value first is equivalent.
        self.bytes_read += raw_len + name.len() + 2;
        if self.bytes_read > self.limits.max_memory {
            return Err(ParserError::RequestDataTooBig);
        }
        let text = self.charset.decode_lossy(&data);
        self.fields.append(name.to_string(), text);
        Ok(())
    }

    fn handle_file(
        &mut self,
        field: String,
        filename: String,
        content_type: String,
        transfer_encoding: Option<&str>,
        payload: &[u8],
    ) -> Result<(), ParserError> {
        self.files_seen += 1;
        if self.files_seen > self.limits.max_files {
            return Err(ParserError::TooManyFiles);
        }
        let Some(filename) = sanitize_file_name(&filename) else {
            return Ok(());
        };
        let bytes = if transfer_encoding == Some("base64") {
            let stripped = strip_whitespace(payload);
            decode_base64(&stripped)
                .map_err(|()| ParserError::Multipart("Could not decode base64 data.".to_string()))?
        } else {
            payload.to_vec()
        };
        self.pending_file = Some(UploadedFile {
            field,
            filename,
            content_type,
            size: bytes.len(),
            bytes,
        });
        Ok(())
    }
}

/// The pieces `parse_boundary_stream` extracts from one encapsulation.
struct PartPayload<'a> {
    field_name: Option<String>,
    filename: Option<String>,
    content_type: Option<String>,
    transfer_encoding: Option<String>,
    body: &'a [u8],
}

/// Port of `parse_boundary_stream`'s header search.
///
/// Django reads 1024 bytes, then 2048, and gives up as soon as the next size
/// would exceed `MAX_TOTAL_HEADER_SIZE`. A short read without `\r\n\r\n`
/// means the encapsulation has no headers and is `RAW`.
fn parse_part<'a>(
    payload: &'a [u8],
    charset: Charset,
) -> Result<(PartKind, PartPayload<'a>), ParserError> {
    let mut chunk_size = INITIAL_HEADER_CHUNK;
    loop {
        if chunk_size > MAX_TOTAL_HEADER_SIZE {
            return Err(ParserError::Multipart(
                "Request max total header size exceeded.".to_string(),
            ));
        }
        let chunk = &payload[..payload.len().min(chunk_size)];
        if let Some(index) = find(chunk, CRLF_CRLF) {
            return Ok(build_part(
                &payload[..index],
                &payload[index + 4..],
                charset,
            ));
        }
        if payload.len() < chunk_size {
            return Ok((PartKind::Raw, raw_payload(payload)));
        }
        chunk_size *= 2;
    }
}

fn build_part<'a>(header: &[u8], body: &'a [u8], charset: Charset) -> (PartKind, PartPayload<'a>) {
    let headers = parse_part_headers(header);
    let disposition = headers.get("content-disposition");
    let filename = disposition.and_then(|(_, params)| params.get("filename").cloned());
    let kind = match disposition {
        Some(_) if filename.as_deref().is_some_and(|name| !name.is_empty()) => PartKind::File,
        Some(_) => PartKind::Field,
        None => PartKind::Raw,
    };
    (
        kind,
        PartPayload {
            field_name: disposition
                .and_then(|(_, params)| params.get("name"))
                // Django strips the *bytes* value, so only ASCII whitespace
                // goes. `str::trim` would also drop U+00A0, which Python keeps.
                .map(|name| {
                    force_str(
                        name.trim_matches(|ch: char| ch.is_ascii_whitespace()),
                        charset,
                    )
                }),
            filename: filename.map(|name| force_str(&name, charset)),
            content_type: headers
                .get("content-type")
                .map(|(value, _)| value.trim().to_string()),
            transfer_encoding: headers
                .get("content-transfer-encoding")
                .map(|(value, _)| value.trim().to_string()),
            body,
        },
    )
}

fn raw_payload(payload: &[u8]) -> PartPayload<'_> {
    PartPayload {
        field_name: None,
        filename: None,
        content_type: None,
        transfer_encoding: None,
        body: payload,
    }
}

/// Django reads a field's charset from the part's own `Content-Type`, not from
/// the request header.
fn strip_whitespace(payload: &[u8]) -> Vec<u8> {
    payload
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect()
}

/// Port of CPython's `binascii.a2b_base64` with `strict_mode=0`, which is what
/// `base64.b64decode(..., validate=False)` calls.
///
/// The loop is deliberately close to `Modules/binascii.c`:
///
/// * characters outside the alphabet are skipped, so `Q!Q==` decodes like
///   `QQ==`;
/// * a `=` only ends processing once it is inside a quad at position 2 or 3
///   and the running pad count reaches the end of that quad; everything after
///   that `=` is ignored;
/// * any other position leaves `quad_pos` at 1, 2, or 3, which is reported as
///   a `binascii.Error` and is what makes Django fall back (fields) or fail
///   (files).
fn decode_base64(input: &[u8]) -> Result<Vec<u8>, ()> {
    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    let mut quad_pos = 0usize;
    let mut left: u8 = 0;
    let mut pads = 0usize;
    for &byte in input {
        if byte == b'=' {
            pads += 1;
            if quad_pos >= 2 && quad_pos + pads >= 4 {
                return Ok(output);
            }
            continue;
        }
        let Some(value) = base64_value(byte) else {
            continue;
        };
        pads = 0;
        match quad_pos {
            0 => {
                quad_pos = 1;
                left = value;
            }
            1 => {
                quad_pos = 2;
                output.push((left << 2) | (value >> 4));
                left = value & 0x0f;
            }
            2 => {
                quad_pos = 3;
                output.push((left << 4) | (value >> 2));
                left = value & 0x03;
            }
            _ => {
                quad_pos = 0;
                output.push((left << 6) | value);
                left = 0;
            }
        }
    }
    if quad_pos == 0 { Ok(output) } else { Err(()) }
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Parse one part's header block into `(name, params)` pairs.
///
/// Django splits the block on `\r\n` only (not on a bare `\n`) and decodes each
/// line with `line.decode()`. `UnicodeDecodeError` is a `ValueError` subclass,
/// so an undecodable line is silently skipped along with any other invalid
/// header. The name before `:` is compared verbatim, *not* trimmed: a header
/// written as `Content-Disposition : ...` is not a content disposition.
fn parse_part_headers(
    block: &[u8],
) -> std::collections::HashMap<String, (String, std::collections::BTreeMap<String, String>)> {
    let mut headers = std::collections::HashMap::new();
    for line in block.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Ok(line) = std::str::from_utf8(line) else {
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }
        let parsed = parse_header_parameters(line);
        let Some((name, value)) = parsed.media_type.split_once(':') else {
            continue;
        };
        headers.insert(name.to_lowercase(), (value.to_string(), parsed.params));
    }
    headers
}

/// `force_str(value, encoding, errors="replace")`.
fn force_str(value: &str, charset: Charset) -> String {
    force_str_bytes(value.as_bytes(), charset)
}

fn force_str_bytes(value: &[u8], charset: Charset) -> String {
    charset.decode_lossy(value)
}

/// `MultiPartParser.sanitize_file_name`.
#[must_use]
pub fn sanitize_file_name(file_name: &str) -> Option<String> {
    let file_name = html_unescape(file_name);
    let file_name = file_name.rsplit('/').next().unwrap_or("");
    let file_name = file_name.rsplit('\\').next().unwrap_or("");
    let file_name: String = file_name.chars().filter(|ch| is_printable(*ch)).collect();
    match file_name.as_str() {
        "" | "." | ".." => None,
        _ => Some(file_name),
    }
}

/// Python's `str.isprintable`, backed by the generated table.
fn is_printable(ch: char) -> bool {
    if ch == ' ' {
        return true;
    }
    let code = u32::from(ch);
    let index = PRINTABLE_RANGES.partition_point(|(start, _)| *start <= code);
    let Some(&(start, end)) = index.checked_sub(1).map(|index| &PRINTABLE_RANGES[index]) else {
        return false;
    };
    (start..=end).contains(&code)
}

/// `html.unescape` for the references filenames carry in practice.
///
/// Python supports every HTML5 entity; filenames use the basic five plus
/// numeric escapes, which is what this covers.
fn html_unescape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        rest = &rest[index..];
        let Some(end) = rest.find(';').filter(|end| *end <= 10) else {
            output.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let replacement = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => numeric_entity(entity),
        };
        match replacement {
            Some(ch) => {
                output.push(ch);
                rest = &rest[end + 1..];
            }
            None => {
                output.push('&');
                rest = &rest[1..];
            }
        }
    }
    output.push_str(rest);
    output
}

fn numeric_entity(entity: &str) -> Option<char> {
    let rest = entity.strip_prefix('#')?;
    let code = match rest.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => rest.parse::<u32>().ok()?,
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::content_type::parse_header_parameters;

    const BOUNDARY: &str = "BoUnDaRyStRiNg";

    fn body(parts: &str) -> Vec<u8> {
        format!("--{BOUNDARY}\r\n{parts}--{BOUNDARY}--\r\n").into_bytes()
    }

    fn parse_body(raw: &[u8]) -> Result<MultipartBody, ParserError> {
        parse(
            &format!("multipart/form-data; boundary={BOUNDARY}"),
            raw.len() as i64,
            raw,
            Charset::Utf8,
            ParserLimits::default(),
        )
    }

    #[test]
    fn simple_fields_and_files() {
        let raw = body(concat!(
            "Content-Disposition: form-data; name=\"name\"\r\n\r\n",
            "Ren\u{e9}\r\n",
            "--BoUnDaRyStRiNg\r\n",
            "Content-Disposition: form-data; name=\"upload\"; filename=\"a.txt\"\r\n",
            "Content-Type: text/plain\r\n\r\n",
            "hello\r\n",
        ));
        let parsed = parse_body(&raw).expect("parse");
        assert_eq!(
            parsed.fields.get("name"),
            Some(&["Ren\u{e9}".to_string()][..])
        );
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].field, "upload");
        assert_eq!(parsed.files[0].filename, "a.txt");
        assert_eq!(parsed.files[0].content_type, "text/plain");
        assert_eq!(parsed.files[0].bytes, b"hello");
    }

    #[test]
    fn filename_sanitisation_follows_django() {
        assert_eq!(
            sanitize_file_name("../evil.txt"),
            Some("evil.txt".to_string())
        );
        assert_eq!(sanitize_file_name("a\\b.txt"), Some("b.txt".to_string()));
        assert_eq!(sanitize_file_name("."), None);
        assert_eq!(sanitize_file_name(".."), None);
        assert_eq!(sanitize_file_name(""), None);
        assert_eq!(
            sanitize_file_name("a&amp;b.txt"),
            Some("a&b.txt".to_string())
        );
        assert_eq!(
            sanitize_file_name("a\u{1}b.txt"),
            Some("ab.txt".to_string())
        );
    }

    #[test]
    fn quoted_boundary_and_limits() {
        let parsed = parse_header_parameters("multipart/form-data; boundary=\"----x\"");
        assert_eq!(
            parsed.params.get("boundary").map(String::as_str),
            Some("----x")
        );
        assert!(valid_boundary("a"));
        assert!(!valid_boundary(""));
        // The regex is `[ -~]{0,200}[!-~]`, so a leading space is allowed.
        assert!(valid_boundary(" a"));
        assert!(!valid_boundary(&"a".repeat(202)));
        assert!(valid_boundary(&"a".repeat(201)));
    }

    #[test]
    fn field_memory_is_separate_from_files() {
        // Plane sets `DATA_UPLOAD_MAX_MEMORY_SIZE` from `FILE_SIZE_LIMIT`
        // (5 MiB), so the field has to cross that line to fail.
        let big = vec![b'x'; crate::parser::DEFAULT_MAX_MEMORY + 1];
        let mut raw = Vec::new();
        raw.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        raw.extend_from_slice(b"Content-Disposition: form-data; name=\"big\"\r\n\r\n");
        raw.extend_from_slice(&big);
        raw.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        assert_eq!(parse_body(&raw), Err(ParserError::RequestDataTooBig));

        let mut raw = Vec::new();
        raw.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        raw.extend_from_slice(
            b"Content-Disposition: form-data; name=\"upload\"; filename=\"a.bin\"\r\n\r\n",
        );
        raw.extend_from_slice(&big);
        raw.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        let parsed = parse_body(&raw).expect("files may exceed the field budget");
        assert_eq!(parsed.files[0].size, big.len());
    }

    #[test]
    fn field_and_file_limits() {
        let mut raw = Vec::new();
        for index in 0..=1001 {
            raw.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            raw.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"f{index}\"\r\n\r\nx\r\n")
                    .as_bytes(),
            );
        }
        raw.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        assert_eq!(parse_body(&raw), Err(ParserError::TooManyFields));

        let mut raw = Vec::new();
        for index in 0..=100 {
            raw.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            raw.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"f\"; filename=\"f{index}.txt\"\r\n\r\nx\r\n"
                )
                .as_bytes(),
            );
        }
        raw.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        assert_eq!(parse_body(&raw), Err(ParserError::TooManyFiles));
    }

    #[test]
    fn parts_count_fields_and_raw_only() {
        // Django's `FIELD_TYPES` is `{FIELD, RAW}`, so a body made only of
        // files never trips `DATA_UPLOAD_MAX_NUMBER_FIELDS`.
        let limits = ParserLimits {
            max_fields: 1,
            ..ParserLimits::default()
        };
        let mut files = Vec::new();
        for index in 0..5 {
            files.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            files.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"u\"; filename=\"f{index}.txt\"\r\n\r\nx\r\n"
                )
                .as_bytes(),
            );
        }
        files.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        let parsed = parse(
            &format!("multipart/form-data; boundary={BOUNDARY}"),
            files.len() as i64,
            &files,
            Charset::Utf8,
            limits,
        )
        .expect("files do not count as fields");
        assert_eq!(parsed.files.len(), 5);

        // Two fields already exceed `max_fields + 2` with the two raw parts.
        let mut fields = Vec::new();
        for index in 0..2 {
            fields.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            fields.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"f{index}\"\r\n\r\nx\r\n")
                    .as_bytes(),
            );
        }
        fields.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        assert_eq!(
            parse(
                &format!("multipart/form-data; boundary={BOUNDARY}"),
                fields.len() as i64,
                &fields,
                Charset::Utf8,
                limits,
            ),
            Err(ParserError::TooManyFields)
        );
    }

    #[test]
    fn headers_are_matched_by_exact_lowercased_name() {
        // `parse_header_parameters` lowercases the main value; Django then
        // splits on `:` without trimming, so a space before the colon makes
        // the header unrecognisable.
        let spaced = format!(
            "--{BOUNDARY}\r\nContent-Disposition : form-data; name=\"a\"\r\n\r\nx\r\n--{BOUNDARY}--\r\n"
        );
        let parsed = parse_body(spaced.as_bytes()).expect("parse");
        assert!(parsed.fields.is_empty(), "a spaced name is not a header");

        // `str.strip()` on the *bytes* value keeps U+00A0.
        let nbsp_char = '\u{a0}';
        let nbsp = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{nbsp_char}a{nbsp_char}\"\r\n\r\nx\r\n--{BOUNDARY}--\r\n"
        );
        let parsed = parse_body(nbsp.as_bytes()).expect("parse");
        let nbsp_key = format!("{nbsp_char}a{nbsp_char}");
        assert_eq!(
            parsed.fields.get(&nbsp_key),
            Some(&["x".to_string()][..]),
            "only ASCII whitespace is stripped from a field name"
        );
        // A tab, however, is ASCII whitespace and goes.
        let tabbed = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"\ta\t\"\r\n\r\nx\r\n--{BOUNDARY}--\r\n"
        );
        let parsed = parse_body(tabbed.as_bytes()).expect("parse");
        assert_eq!(parsed.fields.get("a"), Some(&["x".to_string()][..]));
    }

    #[test]
    fn missing_or_bad_boundaries_follow_django() {
        let error = parse(
            "multipart/form-data",
            10,
            b"--x--\r\n",
            Charset::Utf8,
            ParserLimits::default(),
        )
        .expect_err("no boundary");
        assert_eq!(
            error,
            ParserError::Multipart("Invalid boundary in multipart: None".to_string())
        );
        let error = parse(
            "text/plain",
            10,
            b"--x--\r\n",
            Charset::Utf8,
            ParserLimits::default(),
        )
        .expect_err("wrong type");
        assert_eq!(
            error,
            ParserError::Multipart("Invalid Content-Type: text/plain".to_string())
        );
    }
}
