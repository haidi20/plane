// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! JSON logging: the wire format, the request-id middleware, and redaction.
//!
//! Ported from `apps/api/plane/middleware/logger.py` plus the `LOGGING` block in
//! `apps/api/plane/settings/{local,production}.py`. The golden file
//! `contracts/golden/logging-cases.json` is recorded by
//! `contracts/tools/capture_logging.py` from Plane's own source and from the
//! real `python-json-logger==4.0.0`, so every field name, field order, and
//! escape below is measured rather than guessed.
//!
//! What Plane's logger actually writes (and therefore what this must write):
//!
//! * a flat object whose keys are, in this order, the five `fmt` fields
//!   (`levelname`, `asctime`, `module`, `name`, `message`) followed by every
//!   `extra` key in insertion order;
//! * `asctime` as `%Y-%m-%d %H:%M:%S,mmm` in UTC (`TIME_ZONE` is pinned to UTC
//!   in `common.py`, and the process never calls `time.tzset`);
//! * `ensure_ascii=True`, so non-ASCII scalars are `\uXXXX` escapes — the
//!   `JsonResponse` style, not the DRF style;
//! * `json.dumps` default separators, `", "` and `": "`, because
//!   `python-json-logger` does not pass `separators=`; the line is therefore
//!   *not* compact, unlike a DRF body;
//! * `user_id: null` is kept, because `None` is a real value.
//!
//! Redaction and truncation follow `APITokenLogMiddleware`: the header list is
//! rendered as a Python `dict` repr with the sensitive names masked, and a body
//! longer than 10 KiB (in characters, after decoding) is cut and suffixed.

use std::fmt;

use chrono::{DateTime, Utc};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

/// Longest request or response body, in characters, kept in an API activity log.
pub const MAX_LOGGED_BODY_LENGTH: usize = 10 * 1024;

/// Suffix appended to a body that was cut at [`MAX_LOGGED_BODY_LENGTH`].
pub const TRUNCATION_MARKER: &str = "... [truncated]";

/// Headers whose values must never be persisted in plaintext logs.
///
/// Sorted, lower-case: `_redacted_headers` compares `key.lower()`.
pub const SENSITIVE_HEADERS: [&str; 3] = ["authorization", "cookie", "x-api-key"];

/// Value written instead of a sensitive header value.
pub const REDACTED: &str = "[REDACTED]";

/// `_should_log_route`: Django skips exactly one case, `GET /`.
///
/// The comparison is case-sensitive and matches on the *exact* path, so
/// `GET /` is the only skipped request; `HEAD /` and `GET /?x=1` are logged.
#[must_use]
pub fn should_log_route(path: &str, method: &str) -> bool {
    !(path == "/" && method == "GET")
}

/// `get_client_ip`: the first `X-Forwarded-For` entry, else `REMOTE_ADDR`.
///
/// Plane does not trim and does not validate; `" 1.2.3.4 "` stays as written,
/// because the value only ever reaches a log line. An empty
/// `X-Forwarded-For` falls through to `REMOTE_ADDR` (Python truthiness).
#[must_use]
pub fn client_ip(forwarded_for: Option<&str>, remote_addr: Option<&str>) -> Option<String> {
    match forwarded_for {
        Some(value) if !value.is_empty() => {
            Some(value.split(',').next().unwrap_or(value).to_string())
        }
        _ => remote_addr.map(str::to_string),
    }
}

/// `_safe_decode_body`: decode a body for the activity log, or say why not.
///
/// Order matters and is measured: the binary sniff runs on the *bytes* before
/// decoding, `utf-8` is strict (no `errors="replace"`), and truncation counts
/// **characters**, not bytes — a 12 000-byte body of `é` is 6 000 characters and
/// therefore is not truncated.
#[must_use]
pub fn safe_decode_body(content: Option<&[u8]>) -> Option<String> {
    let content = content?;
    if content.is_empty() {
        return None;
    }
    if content.starts_with(b"\x89PNG")
        || content.starts_with(b"\xff\xd8\xff")
        || content.starts_with(b"%PDF")
    {
        return Some("[Binary Content]".to_string());
    }
    let decoded = match std::str::from_utf8(content) {
        Ok(text) => text,
        Err(_) => return Some("[Could not decode content]".to_string()),
    };
    let characters = decoded.chars().count();
    if characters > MAX_LOGGED_BODY_LENGTH {
        let mut cut: String = decoded.chars().take(MAX_LOGGED_BODY_LENGTH).collect();
        cut.push_str(TRUNCATION_MARKER);
        return Some(cut);
    }
    Some(decoded.to_string())
}

/// `_redacted_headers`: a Python `dict` repr with sensitive values masked.
///
/// The repr is part of the contract because it is stored in
/// `api_activity_logs.headers` as text, so `{'X-Api-Key': '[REDACTED]'}` is
/// reproduced including the single quotes and the `, ` separator. Insertion
/// order is preserved, and the match on the header name is case-insensitive.
#[must_use]
pub fn redacted_headers<'a, I>(headers: I) -> String
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut out = String::from("{");
    for (index, (key, value)) in headers.into_iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str(&py_repr_str(key));
        out.push_str(": ");
        let masked = SENSITIVE_HEADERS
            .iter()
            .any(|name| key.eq_ignore_ascii_case(name));
        out.push_str(&py_repr_str(if masked { REDACTED } else { value }));
    }
    out.push('}');
    out
}

/// `repr(str)` for the subset that can appear in a header.
///
/// CPython picks `'` unless the text contains a single quote and no double
/// quote; then it switches to `"`. Backslashes, `\n`, `\r`, `\t` and other
/// non-printable characters are escaped, which keeps the repr on one line.
fn py_repr_str(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(value.len() + 2);
    out.push(quote);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch == quote => {
                out.push('\\');
                out.push(ch);
            }
            ch if (ch as u32) < 0x20 || ch as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out.push(quote);
    out
}

/// One request-log line, the fields `RequestLoggerMiddleware` emits.
///
/// The field names and their order are the contract: `python-json-logger`
/// writes the `fmt` fields first and then `extra` in the order it was attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestLog {
    /// `"GET /api/v1/workspaces/ 200"`, Django's `f"{method} {full_path} {status}"`.
    pub message: String,
    /// `request.path` (no query string).
    pub path: String,
    /// `request.method`.
    pub method: String,
    /// `response.status_code`.
    pub status_code: u16,
    /// `int(duration * 1000)`, truncated like Python's `int()`.
    pub duration_ms: u64,
    /// `get_client_ip(request)`.
    pub remote_addr: Option<String>,
    /// `request.META["HTTP_USER_AGENT"]`, `""` when absent.
    pub user_agent: String,
    /// `request.user.id` when authenticated, `None` otherwise.
    pub user_id: Option<i64>,
}

/// Inputs the middleware collects before the request runs.
///
/// A struct rather than nine positional arguments: `remote_addr` and
/// `user_agent` are easy to swap, and a silent swap would corrupt every line.
#[derive(Debug, Clone, Copy)]
pub struct RequestParts<'a> {
    /// `request.method`.
    pub method: &'a str,
    /// `request.path`, without the query string.
    pub path: &'a str,
    /// `request.META["QUERY_STRING"]`, when non-empty.
    pub query: Option<&'a str>,
    /// `response.status_code`.
    pub status_code: u16,
    /// Wall time the request took; `duration_ms` is `int(duration * 1000)`.
    pub duration: std::time::Duration,
    /// `HTTP_X_FORWARDED_FOR`, verbatim: the first entry wins, untrimmed.
    pub forwarded_for: Option<&'a str>,
    /// `REMOTE_ADDR`, used when the proxy header is absent.
    pub remote_addr: Option<&'a str>,
    /// `HTTP_USER_AGENT`; absent becomes `""`, not `null`.
    pub user_agent: Option<&'a str>,
    /// `request.user.id` when authenticated.
    pub user_id: Option<i64>,
}

impl RequestLog {
    /// Build a log line from the parts the middleware has.
    ///
    /// `full_path` is what Django's `get_full_path()` returns: the path plus
    /// `?query` when there is a query string. Django's `path` field stays bare.
    #[must_use]
    pub fn new(parts: &RequestParts<'_>) -> Self {
        let full_path = match parts.query {
            Some(query) if !query.is_empty() => format!("{}?{query}", parts.path),
            _ => parts.path.to_string(),
        };
        Self {
            message: format!("{} {full_path} {}", parts.method, parts.status_code),
            path: parts.path.to_string(),
            method: parts.method.to_string(),
            status_code: parts.status_code,
            // `int(duration * 1000)`; `as_millis` truncates the same way.
            duration_ms: u64::try_from(parts.duration.as_millis()).unwrap_or(u64::MAX),
            remote_addr: client_ip(parts.forwarded_for, parts.remote_addr),
            user_agent: parts.user_agent.unwrap_or_default().to_string(),
            user_id: parts.user_id,
        }
    }

    /// Flatten the line into the `extra` fields, in Django's order.
    ///
    /// The order is the contract: `python-json-logger` writes `extra` in
    /// insertion order, and the fields after `message` are always these seven.
    #[must_use]
    pub fn extra_fields(&self) -> Vec<(&'static str, JsonScalar)> {
        vec![
            ("path", JsonScalar::Str(self.path.clone())),
            ("method", JsonScalar::Str(self.method.clone())),
            ("status_code", JsonScalar::Int(i64::from(self.status_code))),
            (
                "duration_ms",
                JsonScalar::Int(i64::try_from(self.duration_ms).unwrap_or(i64::MAX)),
            ),
            (
                "remote_addr",
                self.remote_addr
                    .clone()
                    .map_or(JsonScalar::Null, JsonScalar::Str),
            ),
            ("user_agent", JsonScalar::Str(self.user_agent.clone())),
            (
                "user_id",
                self.user_id.map_or(JsonScalar::Null, JsonScalar::Int),
            ),
        ]
    }
}

/// Render one log line exactly as `python-json-logger` would.
///
/// `created` is the wall clock the record was made at; `module` is the Python
/// module name Django would report (the Rust caller passes its own component
/// name). Only fields that are set are written, matching `extra`.
#[must_use]
pub fn render_line(
    level: &str,
    created: DateTime<Utc>,
    module: &str,
    name: &str,
    message: &str,
    extra: &[(&str, JsonScalar)],
) -> String {
    let mut out = String::with_capacity(160 + message.len());
    out.push('{');
    write_field(
        &mut out,
        "levelname",
        &JsonScalar::Str(level.to_string()),
        true,
    );
    write_field(
        &mut out,
        "asctime",
        &JsonScalar::Str(asctime(created)),
        false,
    );
    write_field(
        &mut out,
        "module",
        &JsonScalar::Str(module.to_string()),
        false,
    );
    write_field(&mut out, "name", &JsonScalar::Str(name.to_string()), false);
    write_field(
        &mut out,
        "message",
        &JsonScalar::Str(message.to_string()),
        false,
    );
    for (key, value) in extra {
        write_field(&mut out, key, value, false);
    }
    out.push('}');
    out
}

/// A scalar that can land in a log line.
///
/// `python-json-logger` serialises whatever `extra` holds; Plane only ever
/// attaches these shapes, and an unrepresentable value becomes `repr()` text
/// rather than a failed log line.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonScalar {
    /// A JSON string.
    Str(String),
    /// A JSON integer.
    Int(i64),
    /// A JSON float, written with Python's `repr` spelling.
    Float(f64),
    /// A JSON boolean.
    Bool(bool),
    /// JSON `null`.
    Null,
}

impl From<&str> for JsonScalar {
    fn from(value: &str) -> Self {
        Self::Str(value.to_string())
    }
}

impl From<String> for JsonScalar {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<i64> for JsonScalar {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<u16> for JsonScalar {
    fn from(value: u16) -> Self {
        Self::Int(i64::from(value))
    }
}

impl From<u64> for JsonScalar {
    fn from(value: u64) -> Self {
        Self::Int(i64::try_from(value).unwrap_or(i64::MAX))
    }
}

impl From<bool> for JsonScalar {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<f64> for JsonScalar {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl<T: Into<JsonScalar>> From<Option<T>> for JsonScalar {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

/// `logging.Formatter.formatTime` with the default format, in UTC.
///
/// Django pins `TIME_ZONE = "UTC"` and never calls `time.tzset()`, so the
/// process timezone the stdlib would use is UTC anyway. The millisecond field
/// is `int(msecs)`, truncated — `1700000000.999` renders `,999`.
#[must_use]
pub fn asctime(created: DateTime<Utc>) -> String {
    let millis = created.timestamp_subsec_millis();
    format!("{},{:03}", created.format("%Y-%m-%d %H:%M:%S"), millis)
}

fn write_field(out: &mut String, key: &str, value: &JsonScalar, first: bool) {
    if !first {
        out.push_str(", ");
    }
    write_json_string(out, key);
    out.push_str(": ");
    match value {
        JsonScalar::Str(text) => write_json_string(out, text),
        JsonScalar::Int(number) => out.push_str(&number.to_string()),
        JsonScalar::Float(number) => out.push_str(&crate::python_repr::repr_float(*number)),
        JsonScalar::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        JsonScalar::Null => out.push_str("null"),
    }
}

/// `json.dumps(..., ensure_ascii=True)` for one string.
///
/// Same escapes as Django's `JsonResponse`: short escapes for the five control
/// characters Python spells out, `\uXXXX` for every other control character and
/// for everything above `~`.
fn write_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            ch => {
                let code = ch as u32;
                if !(0x20..=0x7e).contains(&code) {
                    if code <= 0xffff {
                        out.push_str(&format!("\\u{code:04x}"));
                    } else {
                        let adjusted = code - 0x1_0000;
                        let high = 0xd800 + (adjusted >> 10);
                        let low = 0xdc00 + (adjusted & 0x3ff);
                        out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
                    }
                } else {
                    out.push(ch);
                }
            }
        }
    }
    out.push('"');
}

/// Install the JSON subscriber, with Plane's per-logger levels as the default.
///
/// Django's `LOGGING` sets a level per logger and `RUST_LOG` has no equivalent
/// there; the filter below reproduces the seven `plane.*` loggers from
/// `production.py` for the matching Rust targets, and `RUST_LOG` still wins
/// when an operator sets it. The output format is the Plane one, not
/// `tracing-subscriber`'s own JSON: field names and `asctime` differ.
pub fn init() {
    init_with_default(&default_filter(false));
}

/// Same as [`init`], with the level defaults chosen for `DEBUG`.
pub fn init_for(debug: bool) {
    init_with_default(&default_filter(debug));
}

fn init_with_default(default: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .event_format(PlaneFormat)
                // Django's `StreamHandler` writes to stderr, and an aggregator
                // parses the line as JSON: ANSI escapes would break both.
                .with_writer(std::io::stderr)
                .with_ansi(false),
        )
        .try_init();
}

/// The seven `plane.*` loggers from `plane/settings/production.py`, by name.
///
/// `production.py` sets `DEBUG` for six of them and `ERROR` for
/// `plane.exception`; every one of them is `DEBUG` when `DEBUG=1`. The Rust
/// binaries reuse the same names as `tracing` targets, so an operator can
/// override a single logger with `RUST_LOG=plane.api=debug`.
pub const PRODUCTION_LOGGERS: [&str; 7] = [
    "plane.api",
    "plane.api.request",
    "plane.worker",
    "plane.exception",
    "plane.external",
    "plane.authentication",
    "plane.migrations",
];

/// The level `production.py` assigns to `logger`, for a given `DEBUG`.
///
/// * `plane.exception` is `ERROR` unless `DEBUG`.
/// * `plane.external` is always `INFO`.
/// * the remaining five are `DEBUG` when `DEBUG`, else `INFO`.
#[must_use]
pub fn production_level(logger: &str, debug: bool) -> &'static str {
    match logger {
        "plane.exception" => {
            if debug {
                "DEBUG"
            } else {
                "ERROR"
            }
        }
        "plane.external" => "INFO",
        _ => {
            if debug {
                "DEBUG"
            } else {
                "INFO"
            }
        }
    }
}

/// `RUST_LOG`-style default built from Django's per-logger levels.
///
/// Everything else stays at `info`, matching the `console` handler in both
/// settings modules. Written in `RUST_LOG` syntax: `info` first, then the
/// seven `target=level` pairs that override it.
#[must_use]
pub fn default_filter(debug: bool) -> String {
    let mut filter = String::from("info");
    for logger in PRODUCTION_LOGGERS {
        filter.push(',');
        filter.push_str(logger);
        filter.push('=');
        filter.push_str(
            production_level(logger, debug)
                .to_ascii_lowercase()
                .as_str(),
        );
    }
    filter
}

/// `tracing` event formatter that writes Plane's line instead of `tracing`'s.
struct PlaneFormat;

impl<S, N> FormatEvent<S, N> for PlaneFormat
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> fmt::Result {
        let metadata = event.metadata();
        let mut visitor = JsonVisitor::default();
        event.record(&mut visitor);

        // Structured records (the request line) arrive pre-rendered under
        // `plane_line`, because `tracing` field order is declaration order and
        // its fields cannot be `null`. Writing the object verbatim keeps the
        // exact bytes the golden was recorded with.
        if let Some(line) = visitor.pre_rendered {
            return writeln!(writer, "{line}");
        }

        let level = match *metadata.level() {
            tracing::Level::ERROR => "ERROR",
            tracing::Level::WARN => "WARNING",
            tracing::Level::INFO => "INFO",
            tracing::Level::DEBUG => "DEBUG",
            tracing::Level::TRACE => "DEBUG",
        };
        let message = visitor.message.unwrap_or_default();
        let mut extra: Vec<(String, JsonScalar)> = visitor.fields;
        // `python-json-logger` inserts the `request_id` field right before the
        // `extra` keys, since the middleware adds it in `__call__`.
        if let Some(request_id) = visitor.request_id {
            extra.insert(0, ("request_id".to_string(), JsonScalar::Str(request_id)));
        }
        let owned: Vec<(&str, JsonScalar)> = extra
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect();

        let line = render_line(
            level,
            Utc::now(),
            visitor.module.as_deref().unwrap_or("plane_api"),
            metadata.target(),
            &message,
            &owned,
        );
        writeln!(writer, "{line}")
    }
}

/// Collects `tracing` fields into the JSON shapes a log line can hold.
#[derive(Default)]
struct JsonVisitor {
    message: Option<String>,
    module: Option<String>,
    request_id: Option<String>,
    /// Already-rendered JSON object, written as the whole line.
    pre_rendered: Option<String>,
    fields: Vec<(String, JsonScalar)>,
}

impl tracing::field::Visit for JsonVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "plane_line" => self.pre_rendered = Some(value.to_string()),
            "message" => self.message = Some(value.to_string()),
            "module" => self.module = Some(value.to_string()),
            "request_id" => self.request_id = Some(value.to_string()),
            other => self
                .fields
                .push((other.to_string(), JsonScalar::Str(value.to_string()))),
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields
            .push((field.name().to_string(), JsonScalar::Int(value)));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.push((
            field.name().to_string(),
            JsonScalar::Int(i64::try_from(value).unwrap_or(i64::MAX)),
        ));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.fields
            .push((field.name().to_string(), JsonScalar::Float(value)));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields
            .push((field.name().to_string(), JsonScalar::Bool(value)));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        let text = format!("{value:?}");
        match field.name() {
            "plane_line" => self.pre_rendered = Some(text),
            "message" => self.message = Some(text),
            "module" => self.module = Some(text),
            "request_id" => self.request_id = Some(text),
            other => self.fields.push((other.to_string(), JsonScalar::Str(text))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const LIMIT: usize = MAX_LOGGED_BODY_LENGTH;

    #[test]
    fn only_get_on_the_root_is_skipped() {
        assert!(!should_log_route("/", "GET"));
        assert!(should_log_route("/", "POST"));
        assert!(should_log_route("/", "HEAD"));
        assert!(should_log_route("/", "get"));
        assert!(should_log_route("/api/v1/", "GET"));
        assert!(should_log_route("/robots.txt", "GET"));
    }

    #[test]
    fn client_ip_matches_django_truthiness() {
        assert_eq!(
            client_ip(Some("1.2.3.4, 5.6.7.8"), Some("10.0.0.1")).as_deref(),
            Some("1.2.3.4")
        );
        // No trimming: the value is logged as the proxy wrote it.
        assert_eq!(
            client_ip(Some(" 1.2.3.4 "), Some("10.0.0.1")).as_deref(),
            Some(" 1.2.3.4 ")
        );
        assert_eq!(
            client_ip(Some(""), Some("10.0.0.1")).as_deref(),
            Some("10.0.0.1")
        );
        assert_eq!(
            client_ip(None, Some("10.0.0.1")).as_deref(),
            Some("10.0.0.1")
        );
        assert_eq!(client_ip(None, None), None);
    }

    #[test]
    fn body_sniffing_runs_before_decoding() {
        assert_eq!(safe_decode_body(None), None);
        assert_eq!(safe_decode_body(Some(b"")), None);
        assert_eq!(
            safe_decode_body(Some(b"{\"a\": 1}")).as_deref(),
            Some("{\"a\": 1}")
        );
        assert_eq!(
            safe_decode_body(Some(b"\x89PNG\r\n\x1a\n")).as_deref(),
            Some("[Binary Content]")
        );
        assert_eq!(
            safe_decode_body(Some(b"\xff\xd8\xff\xe0rest")).as_deref(),
            Some("[Binary Content]")
        );
        assert_eq!(
            safe_decode_body(Some(b"%PDF-1.4\n")).as_deref(),
            Some("[Binary Content]")
        );
        // A two-byte prefix that is not a full signature stays undecodable.
        assert_eq!(
            safe_decode_body(Some(b"\x89P")).as_deref(),
            Some("[Could not decode content]")
        );
        assert_eq!(
            safe_decode_body(Some(b"\xff\xfe\x00")).as_deref(),
            Some("[Could not decode content]")
        );
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(
            safe_decode_body(Some(&vec![b'x'; LIMIT]))
                .expect("decoded")
                .len(),
            LIMIT
        );

        let over = safe_decode_body(Some(&vec![b'y'; LIMIT + 1])).expect("decoded");
        assert!(over.ends_with(TRUNCATION_MARKER));
        assert_eq!(
            over.chars().count(),
            LIMIT + TRUNCATION_MARKER.chars().count()
        );

        // 6 000 two-byte characters is 12 000 bytes, over the 10 KiB *byte*
        // length, yet only 6 000 characters: Python counts characters, so the
        // body survives untouched.
        let wide = "\u{e9}".repeat(6000);
        let decoded = safe_decode_body(Some(wide.as_bytes())).expect("decoded");
        assert_eq!(decoded.chars().count(), 6000);
        assert!(!decoded.ends_with(TRUNCATION_MARKER));

        // One character past the limit is truncated, and the cut is counted in
        // characters as well.
        let wide_over = "\u{e9}".repeat(LIMIT + 1);
        let decoded = safe_decode_body(Some(wide_over.as_bytes())).expect("decoded");
        assert!(decoded.ends_with(TRUNCATION_MARKER));
        assert_eq!(
            decoded.chars().count(),
            LIMIT + TRUNCATION_MARKER.chars().count()
        );
    }

    #[test]
    fn redacted_headers_render_a_python_dict() {
        assert_eq!(
            redacted_headers([
                ("X-Api-Key", "secret"),
                ("Authorization", "Bearer t"),
                ("Cookie", "s=1"),
            ]),
            "{'X-Api-Key': '[REDACTED]', 'Authorization': '[REDACTED]', 'Cookie': '[REDACTED]'}"
        );
        assert_eq!(
            redacted_headers([("content-type", "application/json"), ("X-Api-Key", "k")]),
            "{'content-type': 'application/json', 'X-Api-Key': '[REDACTED]'}"
        );
        assert_eq!(
            redacted_headers([("AUTHORIZATION", "Basic x")]),
            "{'AUTHORIZATION': '[REDACTED]'}"
        );
        assert_eq!(
            redacted_headers([("x-forwarded-for", "1.2.3.4")]),
            "{'x-forwarded-for': '1.2.3.4'}"
        );
        assert_eq!(redacted_headers([]), "{}");
    }

    #[test]
    fn python_repr_picks_the_quote_cpython_picks() {
        assert_eq!(py_repr_str("plain"), "'plain'");
        assert_eq!(py_repr_str("it's"), "\"it's\"");
        assert_eq!(py_repr_str("it's \"quoted\""), "'it\\'s \"quoted\"'");
        assert_eq!(py_repr_str("a\nb"), "'a\\nb'");
        assert_eq!(py_repr_str("tab\there"), "'tab\\there'");
        assert_eq!(py_repr_str("back\\slash"), "'back\\\\slash'");
    }

    #[test]
    fn asctime_is_utc_with_truncated_millis() {
        let instant = Utc.with_ymd_and_hms(2023, 11, 14, 22, 13, 20).unwrap()
            + chrono::Duration::milliseconds(123);
        assert_eq!(asctime(instant), "2023-11-14 22:13:20,123");

        let instant = Utc.with_ymd_and_hms(2023, 11, 14, 22, 13, 20).unwrap()
            + chrono::Duration::microseconds(999_999);
        assert_eq!(asctime(instant), "2023-11-14 22:13:20,999");
    }

    #[test]
    fn rendered_line_matches_the_django_shape() {
        let created = Utc.with_ymd_and_hms(2023, 11, 14, 22, 13, 20).unwrap()
            + chrono::Duration::milliseconds(123);
        let line = render_line(
            "INFO",
            created,
            "logger",
            "plane.api.request",
            "GET /api/v1/workspaces/ 200",
            &[
                ("path", JsonScalar::Str("/api/v1/workspaces/".into())),
                ("method", JsonScalar::Str("GET".into())),
                ("status_code", JsonScalar::Int(200)),
                ("duration_ms", JsonScalar::Int(7)),
                ("remote_addr", JsonScalar::Str("203.0.113.9".into())),
                ("user_agent", JsonScalar::Str("Mozilla/5.0".into())),
                ("user_id", JsonScalar::Null),
            ],
        );
        assert_eq!(
            line,
            concat!(
                r#"{"levelname": "INFO", "asctime": "2023-11-14 22:13:20,123", "#,
                r#""module": "logger", "name": "plane.api.request", "#,
                r#""message": "GET /api/v1/workspaces/ 200", "#,
                r#""path": "/api/v1/workspaces/", "method": "GET", "status_code": 200, "#,
                r#""duration_ms": 7, "remote_addr": "203.0.113.9", "#,
                r#""user_agent": "Mozilla/5.0", "user_id": null}"#
            )
        );
    }

    #[test]
    fn non_ascii_and_quotes_are_escaped_like_json_dumps() {
        let created = Utc.with_ymd_and_hms(2023, 11, 14, 22, 13, 23).unwrap()
            + chrono::Duration::milliseconds(500);
        let line = render_line(
            "INFO",
            created,
            "logger",
            "plane.api.request",
            "GET /api/v1/projects/Ren\u{e9}/ 200",
            &[
                ("user_agent", JsonScalar::Str("caf\u{e9}/1.0".into())),
                ("duration_ms", JsonScalar::Int(12)),
            ],
        );
        assert!(
            line.contains(r#""message": "GET /api/v1/projects/Ren\u00e9/ 200""#),
            "{line}"
        );
        assert!(line.contains(r#""user_agent": "caf\u00e9/1.0""#), "{line}");
        assert!(!line.contains('\u{e9}'), "raw non-ASCII leaked: {line}");
    }

    #[test]
    fn no_request_log_field_can_carry_a_credential() {
        // `RequestLog` has no header field at all, so the only credential-bearing
        // inputs are the ones Django itself writes: the user agent, the proxy
        // address, and the path. All three are logged as the client sent them,
        // exactly like Django, and a secret is only ever exposed if a caller
        // puts it there. This test pins that no *new* field appears that would
        // widen the surface without review.
        let log = RequestLog::new(&RequestParts {
            method: "GET",
            path: "/api/v1/workspaces/",
            query: Some("api_key=secret"),
            status_code: 200,
            duration: std::time::Duration::from_millis(1),
            forwarded_for: None,
            remote_addr: Some("10.0.0.1"),
            user_agent: Some("curl/8"),
            user_id: None,
        });
        let fields: Vec<&str> = log.extra_fields().iter().map(|(key, _)| *key).collect();
        assert_eq!(
            fields,
            vec![
                "path",
                "method",
                "status_code",
                "duration_ms",
                "remote_addr",
                "user_agent",
                "user_id",
            ]
        );
        // The query string appears in `message` (Django's `get_full_path()`),
        // which is why `APITokenLogMiddleware` is the only writer allowed to
        // store a query string in a persisted field: it redacts the headers.
        assert!(log.message.contains("api_key=secret"));
        assert!(!log.path.contains("api_key"));
    }

    #[test]
    fn request_log_builds_djangos_message() {
        let log = RequestLog::new(&RequestParts {
            method: "POST",
            path: "/api/v1/issues/",
            query: Some("q=1"),
            status_code: 201,
            duration: std::time::Duration::from_micros(1500),
            forwarded_for: Some("1.2.3.4, 5.6.7.8"),
            remote_addr: Some("10.0.0.1"),
            user_agent: None,
            user_id: Some(42),
        });
        assert_eq!(log.message, "POST /api/v1/issues/?q=1 201");
        assert_eq!(log.path, "/api/v1/issues/");
        assert_eq!(log.duration_ms, 1);
        assert_eq!(log.remote_addr.as_deref(), Some("1.2.3.4"));
        assert_eq!(log.user_agent, "");
        assert_eq!(log.user_id, Some(42));
    }
}
