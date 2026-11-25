// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Byte comparison against the DRF capture in `contracts/golden/parser-*`.
//!
//! Every directory replays the recorded `request.body` through
//! [`plane_pkg::parser::parse_request_body`] and rebuilds the JSON that the
//! capture views returned: `RawView` echoes the parsed value, `BodyView` runs
//! the `Payload` serializer. The response status, header set, and body are
//! compared byte-for-byte. `contracts/golden/parser-json-errors.json` freezes
//! CPython's decoder messages separately.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use plane_pkg::JsonError;
use plane_pkg::compact_json;
use plane_pkg::parser::{
    FormFields, HTML_500, JsonValue, ParsedBody, ParserError, ParserLimits, UploadedFile,
    parse_json, parse_request_body,
};
use serde::{Deserialize, Serialize};

const JSON_HEADERS: [(&str, &str); 2] = [
    ("allow", "POST, OPTIONS"),
    ("content-type", "application/json"),
];
const HTML_HEADERS: [(&str, &str); 1] = [("content-type", "text/html; charset=utf-8")];

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../contracts/golden")
}

#[derive(Debug)]
struct RecordedRequest {
    path: String,
    content_type: Option<String>,
    body: Vec<u8>,
}

#[derive(Debug)]
struct RecordedResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_request(directory: &Path) -> RecordedRequest {
    let path = directory.join("request.http");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut lines = text.split('\n');
    let request_line = lines.next().expect("request line");
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("{}: malformed request line", path.display()))
        .to_string();
    let mut content_type = None;
    for line in lines {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(value) = line.strip_prefix("Content-Type: ") {
            content_type = Some(value.to_string());
        }
    }
    let body_path = directory.join("request.body");
    let body = std::fs::read(&body_path)
        .unwrap_or_else(|error| panic!("{}: {error}", body_path.display()));
    RecordedRequest {
        path: target,
        content_type,
        body,
    }
}

fn read_response(directory: &Path) -> RecordedResponse {
    let path = directory.join("response.http");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut lines = text.split('\n');
    let status_line = lines.next().expect("status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("{}: malformed status line", path.display()))
        .parse()
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut headers = BTreeMap::new();
    for line in lines {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .unwrap_or_else(|| panic!("{}: malformed header {line:?}", path.display()));
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }
    let body_path = directory.join("body.raw");
    let body = std::fs::read(&body_path)
        .unwrap_or_else(|error| panic!("{}: {error}", body_path.display()));
    RecordedResponse {
        status,
        headers,
        body,
    }
}

#[derive(Serialize)]
struct FileEcho<'a> {
    name: &'a str,
    filename: &'a str,
    content_type: &'a str,
    size: usize,
}

#[derive(Serialize)]
struct JsonEcho<'a> {
    kind: &'static str,
    data: &'a JsonValue,
    files: Vec<FileEcho<'a>>,
}

#[derive(Serialize)]
struct FormEcho<'a> {
    kind: &'static str,
    data: &'a FormFields,
    files: Vec<FileEcho<'a>>,
}

#[derive(Serialize)]
struct Accepted<'a> {
    accepted: AcceptedName<'a>,
}

#[derive(Serialize)]
struct AcceptedName<'a> {
    name: &'a str,
}

#[derive(Serialize)]
struct Detail<'a> {
    detail: &'a str,
}

fn is_form_request(request: &RecordedRequest) -> bool {
    // The capture view uses the *raw* header with `str.startswith`, before
    // Django lowercases anything.
    request.content_type.as_deref().is_some_and(|value| {
        value.starts_with("application/x-www-form-urlencoded")
            || value.starts_with("multipart/form-data")
    })
}

fn echo_raw(parsed: &ParsedBody, form_request: bool) -> Result<String, JsonError> {
    match (form_request, parsed) {
        (true, ParsedBody::Form { fields, files }) => {
            let mut ordered: Vec<&UploadedFile> = files.iter().collect();
            // `RawView` sorts `request.FILES` keys; repeats keep arrival order.
            ordered.sort_by(|left, right| left.field.cmp(&right.field));
            let files = ordered
                .into_iter()
                .map(|file| FileEcho {
                    name: &file.field,
                    filename: &file.filename,
                    content_type: &file.content_type,
                    size: file.size,
                })
                .collect();
            compact_json(&FormEcho {
                kind: "form",
                data: fields,
                files,
            })
        }
        (false, ParsedBody::Json(value)) => compact_json(&JsonEcho {
            kind: "json",
            data: value,
            files: Vec::new(),
        }),
        (form_request, parsed) => {
            panic!("capture view kind mismatch: form_request={form_request}, parsed={parsed:?}")
        }
    }
}

fn name_of(parsed: &ParsedBody) -> Option<String> {
    match parsed {
        ParsedBody::Json(JsonValue::Object(pairs)) => {
            for (key, value) in pairs {
                if key.as_str().as_deref() == Some("name") {
                    if let JsonValue::String(text) = value {
                        return text.as_str();
                    }
                }
            }
            None
        }
        // `request.data` merged fields over files; no body-view file case.
        ParsedBody::Form { fields, .. } => {
            fields.get("name").and_then(|values| values.last().cloned())
        }
        _ => None,
    }
}

fn echo_body_view(parsed: &ParsedBody) -> String {
    let name = name_of(parsed).expect("golden body-view cases carry a string `name`");
    compact_json(&Accepted {
        accepted: AcceptedName { name: &name },
    })
    .expect("encode")
}

fn rendered(request: &RecordedRequest) -> (u16, BTreeMap<String, String>, Vec<u8>) {
    let length = request.body.len().to_string();
    let parsed = match parse_request_body(
        request.content_type.as_deref(),
        Some(&length),
        &request.body,
        ParserLimits::default(),
    ) {
        Ok(parsed) => parsed,
        Err(error) => return rendered_error(&error),
    };
    let is_body_view = request.path == "/api/parser-body/";
    let encoded = if is_body_view {
        Ok(echo_body_view(&parsed))
    } else {
        echo_raw(&parsed, is_form_request(request))
    };
    match encoded {
        Ok(body) => (
            200,
            JSON_HEADERS
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            body.into_bytes(),
        ),
        // A lone surrogate parses, then fails while rendering; Django answers
        // with its HTML 500 page and no `Allow` header.
        Err(JsonError::LoneSurrogate) => (
            500,
            HTML_HEADERS
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            HTML_500.as_bytes().to_vec(),
        ),
        Err(error) => panic!("unexpected rendering failure: {error}"),
    }
}

fn rendered_error(error: &ParserError) -> (u16, BTreeMap<String, String>, Vec<u8>) {
    if let Some(html) = error.html_body() {
        return (
            error.status(),
            HTML_HEADERS
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect(),
            html.as_bytes().to_vec(),
        );
    }
    let detail = error.detail();
    let body = compact_json(&Detail { detail: &detail }).expect("encode");
    (
        error.status(),
        JSON_HEADERS
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect(),
        body.into_bytes(),
    )
}

fn replay(directory: &Path) {
    let name = directory
        .file_name()
        .expect("case name")
        .to_string_lossy()
        .into_owned();
    let request = read_request(directory);
    let expected = read_response(directory);
    let (status, headers, body) = rendered(&request);
    assert_eq!(status, expected.status, "{name}: status");
    assert_eq!(headers, expected.headers, "{name}: headers");
    assert_eq!(
        String::from_utf8_lossy(&body),
        String::from_utf8_lossy(&expected.body),
        "{name}: body"
    );
    assert_eq!(body, expected.body, "{name}: body bytes");
}

#[test]
fn every_recorded_parser_case_matches() {
    let directory = golden_dir();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("parser-"))
        })
        .collect();
    cases.sort();
    assert_eq!(cases.len(), 48, "golden parser case count changed");
    for case in cases {
        replay(&case);
    }
}

#[derive(Deserialize)]
struct ErrorRow {
    input: String,
    error: Option<String>,
    kind: String,
}

#[test]
fn json_error_table_matches_cpython() {
    let path = golden_dir().join("parser-json-errors.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let rows: Vec<ErrorRow> = serde_json::from_str(&text).expect("error table");
    assert_eq!(rows.len(), 55, "error table inputs changed");
    for row in rows {
        match (&row.error, parse_json(&row.input)) {
            (Some(expected), Err(plane_pkg::parser::ParseFailure::Decode(error))) => {
                assert_eq!(&error.message, expected, "input {:?}", row.input);
                assert_eq!(row.kind, "JSONDecodeError", "input {:?}", row.input);
            }
            (None, Ok(_)) => assert_eq!(row.kind, "ok", "input {:?}", row.input),
            (expected, actual) => panic!(
                "input {:?}: expected error {expected:?}, got {actual:?}",
                row.input
            ),
        }
    }
}

fn parse_with(
    content_type: Option<&str>,
    content_length: Option<&str>,
    body: &[u8],
) -> Result<ParsedBody, ParserError> {
    parse_request_body(content_type, content_length, body, ParserLimits::default())
}

fn with_length(content_type: Option<&str>, body: &[u8]) -> Result<ParsedBody, ParserError> {
    let length = body.len().to_string();
    parse_with(content_type, Some(&length), body)
}

fn multipart(body: &[u8]) -> Result<ParsedBody, ParserError> {
    with_length(Some("multipart/form-data; boundary=bnd"), body)
}

fn fields(parsed: ParsedBody) -> FormFields {
    match parsed {
        ParsedBody::Form { fields, files } => {
            assert!(files.is_empty(), "unexpected files: {files:?}");
            fields
        }
        other => panic!("expected a form body, got {other:?}"),
    }
}

fn expect_error(result: Result<ParsedBody, ParserError>) -> ParserError {
    match result {
        Ok(parsed) => panic!("expected a parser error, got {parsed:?}"),
        Err(error) => error,
    }
}

#[test]
fn multipart_edge_cases_match_the_oracle() {
    // No closing boundary: the last field keeps the CRLF that follows it.
    let body = b"--bnd\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nx\r\n";
    assert_eq!(fields(multipart(body).expect("no close")), {
        let mut expected = FormFields::new();
        expected.append("a".to_string(), "x\r\n".to_string());
        expected
    });

    // The trailing file is discarded because it never completed.
    let body = b"--bnd\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nx\r\n--bnd\r\nContent-Disposition: form-data; name=\"u\"; filename=\"a.txt\"\r\n\r\nhello\r\n";
    match multipart(body).expect("field then file") {
        ParsedBody::Form { fields, files } => {
            assert_eq!(fields.get("a"), Some(&["x".to_string()][..]));
            assert!(files.is_empty(), "incomplete file is dropped: {files:?}");
        }
        other => panic!("expected form, got {other:?}"),
    }

    // A preamble before the first boundary is ignored.
    let body =
        b"preamble\r\n--bnd\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nx\r\n--bnd--\r\n";
    assert_eq!(
        fields(multipart(body).expect("preamble")).get("a"),
        Some(&["x".to_string()][..])
    );

    // LF-only delimiters do not form a boundary, so nothing is parsed.
    let body = b"--bnd\nContent-Disposition: form-data; name=\"a\"\n\nx\n--bnd--\n";
    assert!(fields(multipart(body).expect("lf only")).is_empty());

    // The header block must end within Django's 1024-byte window.
    let mut body = b"--bnd\r\nContent-Disposition: form-data; name=\"a\"\r\nX-Filler: ".to_vec();
    body.extend_from_slice(&vec![b'x'; 1200]);
    body.extend_from_slice(b"\r\n\r\nx\r\n--bnd--\r\n");
    assert_eq!(
        expect_error(multipart(&body)),
        ParserError::Multipart("Request max total header size exceeded.".to_string())
    );

    // A partial base64 field falls back to its raw bytes; a file is an error.
    let body = b"--bnd\r\nContent-Disposition: form-data; name=\"a\"\r\nContent-Transfer-Encoding: base64\r\n\r\nQQ\r\n--bnd--\r\n";
    assert_eq!(
        fields(multipart(body).expect("base64 field")).get("a"),
        Some(&["QQ".to_string()][..])
    );
    let body = b"--bnd\r\nContent-Disposition: form-data; name=\"u\"; filename=\"a.txt\"\r\nContent-Transfer-Encoding: base64\r\n\r\nQQ\r\n--bnd--\r\n";
    assert_eq!(
        expect_error(multipart(body)),
        ParserError::Multipart("Could not decode base64 data.".to_string())
    );

    // `filename=""` is a field; `filename=" "` is a file named a space.
    let body = b"--bnd\r\nContent-Disposition: form-data; name=\"u\"; filename=\"\"\r\n\r\nx\r\n--bnd--\r\n";
    assert_eq!(
        fields(multipart(body).expect("empty filename")).get("u"),
        Some(&["x".to_string()][..])
    );
    let body = b"--bnd\r\nContent-Disposition: form-data; name=\"u\"; filename=\" \"\r\n\r\nx\r\n--bnd--\r\n";
    match multipart(body).expect("space filename") {
        ParsedBody::Form { fields, files } => {
            assert!(fields.is_empty(), "{fields:?}");
            assert_eq!(files.len(), 1, "{files:?}");
            assert_eq!(files[0].field, "u");
            assert_eq!(files[0].filename, " ");
            assert_eq!(files[0].content_type, "");
            assert_eq!(files[0].size, 1);
        }
        other => panic!("expected form, got {other:?}"),
    }

    // A header line that is not UTF-8 is skipped, so the part has no
    // disposition and contributes neither a field nor a file.
    let body = b"--bnd\r\nContent-Disposition: form-data; name=\"a\xff\"\r\n\r\nx\r\n--bnd--\r\n";
    assert!(fields(multipart(body).expect("bad header")).is_empty());
}

#[test]
fn content_length_edges_match_drf() {
    // A negative length reads zero bytes for JSON and form, but multipart
    // rejects it outright.
    assert_eq!(
        expect_error(parse_with(
            Some("application/json"),
            Some("-1"),
            b"{\"a\":1}"
        )),
        ParserError::Json("Expecting value: line 1 column 1 (char 0)".to_string())
    );
    assert!(
        fields(
            parse_with(
                Some("application/x-www-form-urlencoded"),
                Some("-1"),
                b"a=1"
            )
            .expect("negative form length")
        )
        .is_empty()
    );
    assert_eq!(
        expect_error(parse_with(
            Some("multipart/form-data; boundary=bnd"),
            Some("-1"),
            b"--bnd--\r\n"
        )),
        ParserError::Multipart("Invalid content length: -1".to_string())
    );

    // An unparseable length becomes 0, which short-circuits to an empty body
    // before content negotiation.
    match parse_with(Some("application/json"), Some("abc"), b"{\"a\":1}").expect("bad length") {
        ParsedBody::Json(JsonValue::Object(pairs)) => assert!(pairs.is_empty()),
        other => panic!("expected an empty JSON object, got {other:?}"),
    }

    // `int()` accepts surrounding whitespace and underscores; the body is
    // still read up to the requested length, which the slice clamps.
    assert!(matches!(
        parse_with(Some("application/json"), Some(" 7 "), b"{\"a\":1}"),
        Ok(ParsedBody::Json(JsonValue::Object(_)))
    ));
    assert!(matches!(
        parse_with(Some("application/json"), Some("1_4"), b"{\"a\":1}"),
        Ok(ParsedBody::Json(JsonValue::Object(_)))
    ));
}
