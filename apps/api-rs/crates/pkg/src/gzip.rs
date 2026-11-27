// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `django.middleware.gzip.GZipMiddleware`, in the same order.
//!
//! `process_response` short-circuits **before** `patch_vary_headers` when the
//! body is shorter than 200 bytes or already has a `Content-Encoding`. Every
//! other branch adds `Vary: Accept-Encoding`. The golden case `gzip-short-body`
//! (16-byte health body, `Vary: origin` only) and `body-too-large` (111-byte
//! 413, `Vary: origin` only) exist to keep that ordering honest.
//!
//! Measured facts (`contracts/golden/middleware-cases.json`):
//!
//! * `Accept-Encoding` is matched with `\bgzip\b`: `gzip;q=0` **is** compressed,
//!   `GZIP` and `xgzip` are not;
//! * the compressed body is used only when it is strictly shorter;
//! * the gzip header has `mtime = 0` and `FNAME` set, followed by a random run
//!   of `a` and a NUL, because `compress_string(..., max_random_bytes=100)`
//!   makes the filename random. The random run is allowed to differ, the shape
//!   is not.

use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::Write;

/// Responses shorter than this are returned untouched, `Vary` included.
pub const MIN_LENGTH: usize = 200;

/// `max_random_bytes` in `GZipMiddleware`.
pub const MAX_RANDOM_BYTES: usize = 100;

/// `Content-Encoding` value the middleware writes.
pub const CONTENT_ENCODING: &str = "gzip";

/// Step 1: is this response even a candidate?
///
/// Mirrors the two early returns in `process_response`.
#[must_use]
pub fn should_consider(body_len: usize, has_content_encoding: bool) -> bool {
    body_len >= MIN_LENGTH && !has_content_encoding
}

/// Step 2: does `Accept-Encoding` contain the word `gzip`?
///
/// `re.search(r"\bgzip\b", header)` - case-sensitive, so `GZIP` fails while
/// `gzip;q=0` passes (the `;` is a word boundary).
#[must_use]
pub fn accepts_gzip(accept_encoding: &str) -> bool {
    let bytes = accept_encoding.as_bytes();
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut index = 0;
    while let Some(found) = accept_encoding[index..].find("gzip") {
        let start = index + found;
        let end = start + 4;
        let before = start.checked_sub(1).map(|pos| bytes[pos]);
        let after = bytes.get(end).copied();
        if before.is_none_or(|byte| !is_word(byte)) && after.is_none_or(|byte| !is_word(byte)) {
            return true;
        }
        index = end;
    }
    false
}

/// `compress_string(body, max_random_bytes=100)`.
///
/// `flate2` writes `mtime = 0` already; byte 3 is OR-ed with `FNAME`, then the
/// filename (`"a" * n` plus NUL) is inserted after the fixed 10-byte header.
#[must_use]
pub fn compress(body: &[u8], random_name: usize) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(body).expect("in-memory write");
    let compressed = encoder.finish().expect("in-memory finish");

    let mut out = Vec::with_capacity(compressed.len() + random_name + 1);
    out.extend_from_slice(&compressed[..3]);
    out.push(compressed[3] | 0b0000_1000);
    out.extend_from_slice(&compressed[4..10]);
    // CPython's zlib writes OS = 10 (Unix) even on Windows; flate2 writes
    // 255. The compressed stream itself still differs (zlib vs miniz_oxide),
    // which docs 8.7 normalises; the header bytes are cheap to match.
    out[9] = 10;
    out.extend(std::iter::repeat_n(b'a', random_name));
    out.push(0);
    out.extend_from_slice(&compressed[10..]);
    out
}

/// Step 3: compress, but keep the original when the result is not shorter.
///
/// Returns `None` when the original must be kept (the caller still keeps the
/// `Vary` it already added).
#[must_use]
pub fn compress_if_shorter(body: &[u8]) -> Option<Vec<u8>> {
    let compressed = compress(body, 0);
    (compressed.len() < body.len()).then_some(compressed)
}

/// Weak ETag rule from RFC 9110 8.8.1: a strong ETag becomes `W/"..."`.
#[must_use]
pub fn weaken_etag(etag: &str) -> String {
    if etag.starts_with('"') {
        format!("W/{etag}")
    } else {
        etag.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_gzip_matches_python_word_boundary() {
        assert!(accepts_gzip("gzip"));
        assert!(accepts_gzip("gzip, deflate"));
        assert!(accepts_gzip("gzip;q=0"));
        assert!(accepts_gzip("deflate, gzip;q=1.0"));
        assert!(!accepts_gzip("GZIP"));
        assert!(!accepts_gzip("xgzip"));
        assert!(!accepts_gzip("gzipx"));
        assert!(!accepts_gzip("br"));
        assert!(!accepts_gzip(""));
    }

    #[test]
    fn short_bodies_and_existing_encoding_are_skipped_before_vary() {
        assert!(!should_consider(199, false));
        assert!(should_consider(200, false));
        assert!(!should_consider(500, true));
    }

    #[test]
    fn incompressible_bodies_stay_uncompressed() {
        // Deterministic pseudo-random bytes: deflate cannot shrink them, so
        // Django keeps the original and only `Vary` survives.
        let mut state = 0x9E37_79B9_u32;
        let body: Vec<u8> = (0..1000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xff) as u8
            })
            .collect();
        assert_eq!(compress_if_shorter(&body), None);
    }

    #[test]
    fn compressible_body_round_trips_and_carries_fname() {
        let body = vec![b'a'; 500];
        let out = compress_if_shorter(&body).expect("compressed");
        assert!(out.len() < body.len());
        assert_eq!(out[3] & 0b0000_1000, 0b0000_1000, "FNAME flag");
        assert_eq!(&out[4..8], &[0, 0, 0, 0], "mtime zero");
        let mut decoder = flate2::read::GzDecoder::new(out.as_slice());
        let mut sink = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut sink).expect("gunzip");
        assert_eq!(sink, body);
    }

    #[test]
    fn strong_etags_are_weakened_once() {
        assert_eq!(weaken_etag("\"abc\""), "W/\"abc\"");
        assert_eq!(weaken_etag("W/\"abc\""), "W/\"abc\"");
        assert_eq!(weaken_etag("abc"), "abc");
    }
}
