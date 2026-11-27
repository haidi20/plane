// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Request-body parsing that reproduces DRF's default parser set.
//!
//! The order of decisions matters and is visible on the wire:
//!
//! 1. `Content-Length` is read the way `Request._load_stream` reads it; a value
//!    that is absent, unparseable, or `0` turns the stream into `None`, and
//!    `_parse` then answers 200 with an empty body **before** content
//!    negotiation. That is why a zero-length `text/plain` request is not a 415.
//! 2. A `Content-Type` longer than 256 bytes makes DRF's negotiator return no
//!    parser, so even `application/json; ...` becomes a 415.
//! 3. The parser is selected with DRF's `media_type_matches` (parameters must be
//!    present on both sides). Anything unmatched is a 415 whose message quotes
//!    the raw header.
//! 4. Only then does the selected parser decode the body, and its charset
//!    handling differs per parser:
//!    * JSON: `codecs.lookup` decides whether Django records the charset at
//!      all; a charset that resolves is then passed to `''.encode`, which is
//!      what rejects bytes-to-bytes codecs such as `bz2`.
//!    * form and multipart: the same `''.encode` check runs, then the body is
//!      decoded with the header's charset (`errors="replace"`).
//!
//! Errors keep DRF's exact sentences because clients match on them.

pub mod charset;
pub mod content_type;
pub mod form;
pub mod json_body;
pub mod multipart;

use crate::json::JsonError;

pub use charset::{Charset, CharsetResolution, DecodeError, resolve as resolve_charset};
pub use content_type::{
    ParsedContentType, is_form_media_type, media_type_matches, parse_header_parameters,
};
pub use form::{FormFields, parse_urlencoded};
pub use json_body::{JsonValue, Number, ParseFailure, PyString, parse_json};
pub use multipart::UploadedFile;

/// DRF's `MAX_MEDIA_TYPE_LENGTH`.
pub const MAX_MEDIA_TYPE_LENGTH: usize = 256;

/// Plane's `DATA_UPLOAD_MAX_MEMORY_SIZE` / `FILE_SIZE_LIMIT` default.
pub const DEFAULT_MAX_MEMORY: usize = 5_242_880;
/// Django's `DATA_UPLOAD_MAX_NUMBER_FIELDS` default.
pub const DEFAULT_MAX_FIELDS: usize = 1_000;
/// Django's `DATA_UPLOAD_MAX_NUMBER_FILES` default.
pub const DEFAULT_MAX_FILES: usize = 100;

/// The limits a caller must supply (Plane overrides the defaults in settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserLimits {
    /// `DATA_UPLOAD_MAX_MEMORY_SIZE`: the budget for multipart **fields** only.
    pub max_memory: usize,
    /// `DATA_UPLOAD_MAX_NUMBER_FIELDS`.
    pub max_fields: usize,
    /// `DATA_UPLOAD_MAX_NUMBER_FILES`.
    pub max_files: usize,
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            max_memory: DEFAULT_MAX_MEMORY,
            max_fields: DEFAULT_MAX_FIELDS,
            max_files: DEFAULT_MAX_FILES,
        }
    }
}

/// What DRF's `request.data` holds after a successful parse.
#[derive(Debug)]
pub enum ParsedBody {
    /// `request.data` plus an empty `request.FILES`.
    Json(JsonValue),
    /// `request.POST` and `request.FILES` as separate maps.
    Form {
        /// `request.POST`: repeated keys keep every value, in arrival order.
        fields: FormFields,
        /// `request.FILES`, in arrival order.
        files: Vec<UploadedFile>,
    },
}

impl ParsedBody {
    /// The shape `request.data` has after DRF merges files over fields.
    ///
    /// `request.data` is `request.POST` copied and then updated with
    /// `request.FILES`, so a key that exists in both keeps only the file value.
    /// This helper reports what a view sees through `request.data`.
    #[must_use]
    pub fn merged_len(&self) -> usize {
        match self {
            Self::Json(value) => value.len_hint(),
            Self::Form { fields, files } => {
                let file_keys: std::collections::HashSet<&str> =
                    files.iter().map(|file| file.field.as_str()).collect();
                fields.len() + file_keys.len()
            }
        }
    }
}

/// Why parsing failed, with the status and the exact DRF body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParserError {
    /// 415: no parser matched the media type. Carries the raw header.
    UnsupportedMediaType(String),
    /// 400: DRF rejected the charset before decoding because `''.encode`
    /// raised `LookupError`, which is how a bytes-to-bytes codec such as
    /// `bz2` shows up.
    UnsupportedCharset(String),
    /// 500: `codecs.lookup` resolved a text codec that this port cannot
    /// decode. DRF would decode it, so the byte-for-byte answer is undefined
    /// here; the caller must not guess a table (see `docs/rust-api.md` 8.7).
    UnsupportedCodec(String),
    /// 400: `JSONParser` wrapped the decoding/parse error.
    Json(String),
    /// 400: `MultiPartParser` wrapped a `MultiPartParserError`.
    Multipart(String),
    /// 413-ish 400: Django's `RequestDataTooBig` from a multipart field.
    RequestDataTooBig,
    /// 400: Django's `TooManyFieldsSent`.
    TooManyFields,
    /// 400: Django's `TooManyFilesSent`.
    TooManyFiles,
    /// 400: Django's `SuspiciousMultipartForm`.
    SuspiciousMultipart,
    /// 500: `RecursionError` escapes as-parse errors are `ValueError` subclasses
    /// only; a nested body hits Python's recursion guard instead.
    Recursion,
    /// 500: the instance failed while rendering a parsed value. A lone
    /// surrogate parses fine but `json.dumps` rejects it, and Django answers
    /// with its HTML 500 page rather than a DRF envelope.
    RenderFailed,
}

/// Django's HTML page for a `SuspiciousOperation` (400). Verified against the
/// live Django 5.2 response: one leading newline, no `Allow` header.
pub const HTML_400: &str = "\n<!doctype html>\n<html lang=\"en\">\n<head>\n  <title>Bad Request (400)</title>\n</head>\n<body>\n  <h1>Bad Request (400)</h1><p></p>\n</body>\n</html>\n";

/// Django's HTML page for an uncaught exception (500), which is what a lone
/// surrogate produces while the renderer encodes the response.
pub const HTML_500: &str = "\n<!doctype html>\n<html lang=\"en\">\n<head>\n  <title>Server Error (500)</title>\n</head>\n<body>\n  <h1>Server Error (500)</h1><p></p>\n</body>\n</html>\n";

impl ParserError {
    /// HTTP status DRF ends up returning.
    #[must_use]
    pub fn status(&self) -> u16 {
        match self {
            Self::UnsupportedMediaType(_) => 415,
            Self::Recursion | Self::RenderFailed | Self::UnsupportedCodec(_) => 500,
            _ => 400,
        }
    }

    /// `true` when Django answers with its HTML error page instead of DRF's
    /// JSON envelope.
    ///
    /// Django renders `SuspiciousOperation` (the multipart limits) and
    /// uncaught 500s itself; only exceptions DRF catches become JSON.
    #[must_use]
    pub fn is_html(&self) -> bool {
        matches!(
            self,
            Self::RequestDataTooBig
                | Self::TooManyFields
                | Self::TooManyFiles
                | Self::SuspiciousMultipart
                | Self::Recursion
                | Self::RenderFailed
                | Self::UnsupportedCodec(_)
        )
    }

    /// The exact HTML body Django writes, or `None` for DRF's JSON envelope.
    #[must_use]
    pub fn html_body(&self) -> Option<&'static str> {
        if !self.is_html() {
            return None;
        }
        Some(if self.status() == 400 {
            HTML_400
        } else {
            HTML_500
        })
    }

    /// The `detail` string, before JSON encoding.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::UnsupportedMediaType(raw) => {
                format!("Unsupported media type \"{raw}\" in request.")
            }
            Self::UnsupportedCharset(name) => {
                format!("Unsupported charset \"{name}\" in request Content-Type header.")
            }
            Self::UnsupportedCodec(name) => {
                format!("Unsupported charset \"{name}\" in request Content-Type header.")
            }
            Self::Json(message) => format!("JSON parse error - {message}"),
            Self::Multipart(message) => format!("Multipart form parse error - {message}"),
            Self::RequestDataTooBig => {
                "Request body exceeded settings.DATA_UPLOAD_MAX_MEMORY_SIZE.".to_string()
            }
            Self::TooManyFields => {
                "The number of GET/POST parameters exceeded settings.DATA_UPLOAD_MAX_NUMBER_FIELDS."
                    .to_string()
            }
            Self::TooManyFiles => {
                "The number of files exceeded settings.DATA_UPLOAD_MAX_NUMBER_FILES.".to_string()
            }
            Self::SuspiciousMultipart => {
                "The multipart parser got stuck, which shouldn't happen with normal uploaded files. Check for malicious upload activity; if there is none, report this to the Django developers."
                    .to_string()
            }
            Self::Recursion => "maximum recursion depth exceeded".to_string(),
            Self::RenderFailed => "maximum recursion depth exceeded".to_string(),
        }
    }
}

impl From<JsonError> for ParserError {
    fn from(error: JsonError) -> Self {
        Self::Json(error.to_string())
    }
}

/// Parse a request body exactly as DRF does for its default parsers.
///
/// `content_type` is the raw header value (`None` when the header is absent,
/// which DRF reports as `""` in the 415 message). `content_length` is the raw
/// `Content-Length` header, if any; DRF re-parses it here rather than trusting
/// the transport.
///
/// # Errors
/// Returns [`ParserError`] carrying DRF's status and message.
pub fn parse_request_body(
    content_type: Option<&str>,
    content_length: Option<&str>,
    body: &[u8],
    limits: ParserLimits,
) -> Result<ParsedBody, ParserError> {
    let raw_media_type = content_type.unwrap_or("");
    // DRF's `Request._load_stream` calls `int()` on the header; a value it
    // cannot parse (or a missing header) becomes 0. `int()` is not Rust's
    // `parse::<i64>()`: it strips 25 of Python's 29 whitespace codepoints,
    // allows `_` between digits, accepts any Unicode `Nd` digit, and caps the
    // digit count at 4300.
    let effective_length = content_length.and_then(python_int).unwrap_or(0);

    if effective_length == 0 {
        return Ok(empty_body(raw_media_type));
    }

    if !raw_media_type.is_empty() && raw_media_type.len() > MAX_MEDIA_TYPE_LENGTH {
        return Err(ParserError::UnsupportedMediaType(
            raw_media_type.to_string(),
        ));
    }

    let parsed = match content_type {
        Some(value) => parse_header_parameters(value),
        None => ParsedContentType::empty(),
    };

    let parser = match parser_for(&parsed) {
        Some(parser) => parser,
        None => {
            return Err(ParserError::UnsupportedMediaType(
                raw_media_type.to_string(),
            ));
        }
    };

    // Django reads `request.body` lazily and rejects anything larger than
    // `DATA_UPLOAD_MAX_MEMORY_SIZE` before DRF sees it, for JSON and form
    // bodies alike. Multipart is exempt: its fields are budgeted while
    // parsing instead (and files never count).
    // A negative `Content-Length` reaches `LimitedStream` with a negative
    // limit, so DRF's `request.body` reads zero bytes: JSON then fails with
    // "Expecting value" and a form body parses as empty. Multipart is
    // different because DRF hands it the raw stream, and Django rejects the
    // negative length outright.
    let body_limit = usize::try_from(effective_length).ok();
    let body = if matches!(parser, Parser::Json | Parser::Form) {
        match body_limit {
            Some(limit) => {
                if limit > limits.max_memory {
                    return Err(ParserError::RequestDataTooBig);
                }
                &body[..limit.min(body.len())]
            }
            None => &body[..0],
        }
    } else {
        body
    };

    let charset_name = parsed.params.get("charset").map(String::as_str);
    match parser {
        Parser::Json => {
            let charset = json_charset(charset_name)?;
            let text = charset
                .decode_strict(body)
                .map_err(|error| ParserError::Json(error.message))?;
            match parse_json(&text) {
                Ok(value) => Ok(ParsedBody::Json(value)),
                Err(ParseFailure::Decode(error)) => Err(ParserError::Json(error.message)),
                Err(ParseFailure::Recursion) => Err(ParserError::Recursion),
            }
        }
        Parser::Form => {
            let charset = form_charset(charset_name)?;
            let fields = parse_urlencoded(body, charset, limits.max_fields)?;
            Ok(ParsedBody::Form {
                fields,
                files: Vec::new(),
            })
        }
        Parser::Multipart => {
            let charset = form_charset(charset_name)?;
            let parsed = multipart::parse(raw_media_type, effective_length, body, charset, limits)?;
            Ok(ParsedBody::Form {
                fields: parsed.fields,
                files: parsed.files,
            })
        }
    }
}

fn empty_body(raw_media_type: &str) -> ParsedBody {
    if is_form_media_type(raw_media_type) {
        ParsedBody::Form {
            fields: FormFields::new(),
            files: Vec::new(),
        }
    } else {
        ParsedBody::Json(JsonValue::Object(Vec::new()))
    }
}

/// CPython's `int(text)` for base 10, which is what DRF uses on the raw
/// `Content-Length` header.
///
/// The observable rules (CPython 3.12, `PyLong_FromString` plus the
/// `_PyUnicode_TransformDecimalAndSpaceToASCII` pre-pass):
///
/// * Leading and trailing whitespace is removed. Only 25 of Python's 29
///   `str.isspace()` codepoints count: `U+001C..U+001F` are *not* stripped and
///   make the value invalid.
/// * One optional `+`/`-` sign.
/// * Digits may be separated by single underscores, never leading, trailing,
///   or doubled.
/// * Any Unicode decimal digit (`Nd`) is accepted and folded to ASCII.
/// * More than 4300 digits raises `ValueError`, which DRF swallows into 0.
///
/// The return value saturates at `i64` because only two things use it: a
/// zero test and a slice bound. A length larger than `i64::MAX` can never be
/// smaller than any real body.
fn python_int(text: &str) -> Option<i64> {
    const MAX_DIGITS: usize = 4300;

    let text = text.trim_matches(is_python_int_space);
    let (negative, digits) = match text.as_bytes().first() {
        Some(b'+') => (false, &text[1..]),
        Some(b'-') => (true, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() {
        return None;
    }

    let mut value: i64 = 0;
    let mut count = 0usize;
    let mut previous_underscore = false;
    let mut seen_digit = false;
    for ch in digits.chars() {
        if ch == '_' {
            if !seen_digit || previous_underscore {
                return None;
            }
            previous_underscore = true;
            continue;
        }
        let digit = decimal_digit(ch)?;
        if previous_underscore {
            previous_underscore = false;
        }
        count += 1;
        if count > MAX_DIGITS {
            return None;
        }
        seen_digit = true;
        value = value.saturating_mul(10).saturating_add(i64::from(digit));
    }
    if previous_underscore || !seen_digit {
        return None;
    }
    Some(if negative { -value } else { value })
}

/// The whitespace `int()` strips: `str.isspace()` minus `U+001C..U+001F`.
fn is_python_int_space(ch: char) -> bool {
    matches!(
        ch,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// `unicodedata.decimal(ch)`, i.e. any Unicode `Nd` digit.
///
/// The 64 ranges are the `Nd` blocks in Unicode 15; CPython ships the same
/// set. `char::is_ascii_digit` alone would reject `１` (FULLWIDTH ONE), which
/// Python accepts.
fn decimal_digit(ch: char) -> Option<u8> {
    const ND_RANGES: [(u32, u32); 64] = [
        (0x30, 0x39),
        (0x660, 0x669),
        (0x6f0, 0x6f9),
        (0x7c0, 0x7c9),
        (0x966, 0x96f),
        (0x9e6, 0x9ef),
        (0xa66, 0xa6f),
        (0xae6, 0xaef),
        (0xb66, 0xb6f),
        (0xbe6, 0xbef),
        (0xc66, 0xc6f),
        (0xce6, 0xcef),
        (0xd66, 0xd6f),
        (0xde6, 0xdef),
        (0xe50, 0xe59),
        (0xed0, 0xed9),
        (0xf20, 0xf29),
        (0x1040, 0x1049),
        (0x1090, 0x1099),
        (0x17e0, 0x17e9),
        (0x1810, 0x1819),
        (0x1946, 0x194f),
        (0x19d0, 0x19d9),
        (0x1a80, 0x1a89),
        (0x1a90, 0x1a99),
        (0x1b50, 0x1b59),
        (0x1bb0, 0x1bb9),
        (0x1c40, 0x1c49),
        (0x1c50, 0x1c59),
        (0xa620, 0xa629),
        (0xa8d0, 0xa8d9),
        (0xa900, 0xa909),
        (0xa9d0, 0xa9d9),
        (0xa9f0, 0xa9f9),
        (0xaa50, 0xaa59),
        (0xabf0, 0xabf9),
        (0xff10, 0xff19),
        (0x104a0, 0x104a9),
        (0x10d30, 0x10d39),
        (0x11066, 0x1106f),
        (0x110f0, 0x110f9),
        (0x11136, 0x1113f),
        (0x111d0, 0x111d9),
        (0x112f0, 0x112f9),
        (0x11450, 0x11459),
        (0x114d0, 0x114d9),
        (0x11650, 0x11659),
        (0x116c0, 0x116c9),
        (0x11730, 0x11739),
        (0x118e0, 0x118e9),
        (0x11950, 0x11959),
        (0x11c50, 0x11c59),
        (0x11d50, 0x11d59),
        (0x11da0, 0x11da9),
        (0x11f50, 0x11f59),
        (0x16a60, 0x16a69),
        (0x16ac0, 0x16ac9),
        (0x16b50, 0x16b59),
        (0x1d7ce, 0x1d7ff),
        (0x1e140, 0x1e149),
        (0x1e2f0, 0x1e2f9),
        (0x1e4f0, 0x1e4f9),
        (0x1e950, 0x1e959),
        (0x1fbf0, 0x1fbf9),
    ];
    let code = u32::from(ch);
    let index = ND_RANGES.partition_point(|(start, _)| *start <= code);
    let (start, end) = *ND_RANGES.get(index.checked_sub(1)?)?;
    (code <= end).then_some((code - start) as u8)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parser {
    Json,
    Form,
    Multipart,
}

const PARSER_MEDIA_TYPES: [(Parser, &str); 3] = [
    (Parser::Json, "application/json"),
    (Parser::Form, "application/x-www-form-urlencoded"),
    (Parser::Multipart, "multipart/form-data"),
];

fn parser_for(content_type: &ParsedContentType) -> Option<Parser> {
    PARSER_MEDIA_TYPES
        .iter()
        .find(|(_, media_type)| media_type_matches(media_type, content_type))
        .map(|(parser, _)| *parser)
}

/// Charset for `JSONParser`, following Django's `_set_content_type_params`.
///
/// Django records the header charset only when `codecs.lookup` resolves it; an
/// unknown name therefore falls back to `DEFAULT_CHARSET` (UTF-8) instead of
/// failing. A name that resolves but that DRF's `''.encode` rejects becomes a
/// 400 before the body is decoded.
fn json_charset(name: Option<&str>) -> Result<Charset, ParserError> {
    let Some(name) = name else {
        return Ok(Charset::Utf8);
    };
    match resolve_charset(name) {
        CharsetResolution::Use(charset) => Ok(charset),
        CharsetResolution::Unknown => Ok(Charset::Utf8),
        CharsetResolution::NonText => Err(ParserError::UnsupportedCharset(name.to_string())),
        CharsetResolution::UnsupportedText => Err(ParserError::UnsupportedCodec(name.to_string())),
    }
}

/// Charset for the two form parsers. The check is the same one DRF applies;
/// both parsers call `get_encoding`, whose error text names the raw value.
fn form_charset(name: Option<&str>) -> Result<Charset, ParserError> {
    json_charset(name)
}
