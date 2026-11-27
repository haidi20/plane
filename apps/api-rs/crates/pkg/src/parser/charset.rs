// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Charset resolution and decoding for request bodies.
//!
//! Django resolves the `charset` parameter with `codecs.lookup()` and DRF then
//! checks it with `''.encode(encoding)`, which rejects bytes-to-bytes codecs
//! such as `bz2`. The two steps are visible on the wire:
//!
//! * an unknown name (`charset=nope`) fails `lookup`, so Django keeps its
//!   default and the request succeeds as UTF-8;
//! * a real but non-text codec (`charset=bz2`) passes `lookup` and is rejected
//!   by DRF with a 400.
//!
//! Decoders below cover the charsets Plane receives in practice. A codec that
//! Python knows but this module cannot decode is reported as unsupported
//! instead of being silently decoded with the wrong table (see
//! `docs/rust-api.md` §8.7).

mod table;

/// A charset this module can decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charset {
    /// UTF-8, including the `utf8` and `u8` aliases.
    Utf8,
    /// UTF-8 with an optional leading BOM removed (`utf-8-sig`).
    Utf8Sig,
    /// UTF-16 with BOM detection; native order (little endian) without a BOM.
    Utf16,
    /// Little-endian UTF-16 without BOM handling.
    Utf16Le,
    /// Big-endian UTF-16 without BOM handling.
    Utf16Be,
    /// ASCII.
    Ascii,
    /// ISO-8859-1, byte to code point.
    Latin1,
    /// Windows-1252.
    Cp1252,
}

/// What `codecs.lookup()` plus `''.encode()` would decide for a charset name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharsetResolution {
    /// A charset this module decodes.
    Use(Charset),
    /// `''.encode()` rejects the codec: DRF answers 400.
    NonText,
    /// `codecs.lookup()` succeeds but this module has no decoder: DRF would
    /// decode it, so the caller must treat the request as unsupported rather
    /// than guess.
    UnsupportedText,
    /// `codecs.lookup()` fails: Django falls back to `DEFAULT_CHARSET`.
    Unknown,
}

/// Normalise an encoding name the way CPython does before the registry sees
/// it: lowercase, keep ASCII letters/digits and `.`, collapse any other run to
/// a single `_`, and drop leading/trailing `_`.
///
/// Non-ASCII characters are dropped without producing a separator, which is why
/// `utf\u{2011}8` and `utf8` resolve to the same codec. This mirrors
/// `_Py_normalize_encoding` (C) plus `encodings.normalize_encoding` (Python):
/// the C layer lowercases and collapses, the Python layer drops non-ASCII.
#[must_use]
pub fn normalize_name(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    let mut pending_separator = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' {
            if pending_separator && !output.is_empty() {
                output.push('_');
            }
            pending_separator = false;
            output.push(ch.to_ascii_lowercase());
        } else {
            pending_separator = true;
        }
    }
    output
}

/// Look a name up in `encodings.aliases.aliases`, including the dotted
/// fallback CPython applies (`iso.8859.1` -> `iso_8859_1`).
fn lookup_alias(name: &str) -> Option<&'static str> {
    let find = |key: &str| {
        table::CODEC_ALIASES
            .binary_search_by_key(&key, |(candidate, _)| *candidate)
            .ok()
            .map(|index| table::CODEC_ALIASES[index].1)
    };
    find(name).or_else(|| {
        let underscored = name.replace('.', "_");
        if underscored == name {
            None
        } else {
            find(&underscored)
        }
    })
}

/// Resolve a `charset` parameter the way `codecs.lookup()` plus DRF's
/// `''.encode(name)` check decide it.
///
/// CPython imports the alias target first and the normalised name second, and
/// skips any module name that still contains a dot. Reproducing the skip
/// matters: `utf.8` is **unknown** even though `utf_8` is a real module.
#[must_use]
pub fn resolve(name: &str) -> CharsetResolution {
    let normalised = normalize_name(name);
    let alias = lookup_alias(&normalised);
    let candidates: [Option<&str>; 2] = match alias {
        Some(target) => [Some(target), Some(normalised.as_str())],
        None => [Some(normalised.as_str()), None],
    };
    for candidate in candidates.into_iter().flatten() {
        if candidate.is_empty() || candidate.contains('.') {
            continue;
        }
        let Ok(index) =
            table::CODEC_MODULES.binary_search_by_key(&candidate, |(module, _)| *module)
        else {
            continue;
        };
        return match &table::CODEC_MODULES[index].1 {
            table::CodecClass::Supported(charset) => CharsetResolution::Use(*charset),
            table::CodecClass::Text => CharsetResolution::UnsupportedText,
            table::CodecClass::NonText => CharsetResolution::NonText,
        };
    }
    CharsetResolution::Unknown
}

/// Why a strict decode failed, with CPython's message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError {
    /// Message as `UnicodeDecodeError.__str__` renders it.
    pub message: String,
}

/// Python's UTF-8 decoder, including the offsets CPython reports.
///
/// CPython treats a truncated multi-byte sequence at the end of the input as
/// `unexpected end of data` covering the bytes that are present, but only when
/// the continuation bytes it did see are valid; otherwise the error covers the
/// bytes up to the last valid continuation.
fn decode_utf8_named(bytes: &[u8], name: &str) -> Result<String, DecodeError> {
    let mut text = String::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let first = bytes[index];
        if first < 0x80 {
            text.push(char::from(first));
            index += 1;
            continue;
        }
        let width = match first {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => {
                return Err(DecodeError {
                    message: format!(
                        "'{name}' codec can't decode byte 0x{first:02x} in position {index}: invalid start byte"
                    ),
                });
            }
        };
        if index + width > bytes.len() {
            let mut last = index;
            for offset in 1..(bytes.len() - index) {
                let byte = bytes[index + offset];
                if !(0x80..=0xbf).contains(&byte) {
                    break;
                }
                last = index + offset;
            }
            let subject = if last == index {
                format!("byte 0x{:02x} in position {index}", bytes[index])
            } else {
                format!("bytes in position {index}-{last}")
            };
            return Err(DecodeError {
                message: format!("'{name}' codec can't decode {subject}: unexpected end of data"),
            });
        }
        let second = bytes[index + 1];
        let second_ok = match width {
            2 => (0x80..=0xbf).contains(&second),
            3 => match first {
                0xe0 => (0xa0..=0xbf).contains(&second),
                0xed => (0x80..=0x9f).contains(&second),
                _ => (0x80..=0xbf).contains(&second),
            },
            _ => match first {
                0xf0 => (0x90..=0xbf).contains(&second),
                0xf4 => (0x80..=0x8f).contains(&second),
                _ => (0x80..=0xbf).contains(&second),
            },
        };
        if !second_ok {
            return Err(DecodeError {
                message: format!(
                    "'{name}' codec can't decode byte 0x{first:02x} in position {index}: invalid continuation byte"
                ),
            });
        }
        let mut valid = index + 2;
        while valid < index + width {
            if !(0x80..=0xbf).contains(&bytes[valid]) {
                return Err(DecodeError {
                    message: format!(
                        "'{name}' codec can't decode bytes in position {index}-{}: invalid continuation byte",
                        valid - 1
                    ),
                });
            }
            valid += 1;
        }
        let decoded = std::str::from_utf8(&bytes[index..index + width])
            .map_err(|_| DecodeError {
                message: format!(
                    "'{name}' codec can't decode byte 0x{first:02x} in position {index}: invalid continuation byte"
                ),
            })?;
        text.push_str(decoded);
        index += width;
    }
    Ok(text)
}

/// `errors="replace"` for UTF-8, matching CPython's substitution boundaries.
fn decode_utf8_lossy(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let first = bytes[index];
        if first < 0x80 {
            text.push(char::from(first));
            index += 1;
            continue;
        }
        let width = match first {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => {
                text.push('\u{fffd}');
                index += 1;
                continue;
            }
        };
        let available = (bytes.len() - index).min(width);
        let mut valid = 1;
        while valid < available && (0x80..=0xbf).contains(&bytes[index + valid]) {
            valid += 1;
        }
        let boundary_ok = match width {
            2 => valid == 2,
            3 => {
                valid == 3
                    && match first {
                        0xe0 => bytes[index + 1] >= 0xa0,
                        0xed => bytes[index + 1] <= 0x9f,
                        _ => true,
                    }
            }
            _ => {
                valid == 4
                    && match first {
                        0xf0 => bytes[index + 1] >= 0x90,
                        0xf4 => bytes[index + 1] <= 0x8f,
                        _ => true,
                    }
            }
        };
        if boundary_ok {
            match std::str::from_utf8(&bytes[index..index + width]) {
                Ok(decoded) => {
                    text.push_str(decoded);
                    index += width;
                }
                Err(_) => {
                    text.push('\u{fffd}');
                    index += 1;
                }
            }
        } else {
            text.push('\u{fffd}');
            index += if valid > 1 { valid } else { 1 };
        }
    }
    text
}

fn decode_ascii(bytes: &[u8]) -> Result<String, DecodeError> {
    if let Some((index, byte)) = bytes.iter().enumerate().find(|(_, byte)| !byte.is_ascii()) {
        return Err(DecodeError {
            message: format!(
                "'ascii' codec can't decode byte 0x{byte:02x} in position {index}: ordinal not in range(128)"
            ),
        });
    }
    Ok(bytes.iter().map(|byte| char::from(*byte)).collect())
}

fn decode_cp1252(bytes: &[u8]) -> Result<String, DecodeError> {
    let mut text = String::with_capacity(bytes.len());
    for (index, byte) in bytes.iter().enumerate() {
        let ch = match byte {
            0x80 => '\u{20ac}',
            0x82 => '\u{201a}',
            0x83 => '\u{0192}',
            0x84 => '\u{201e}',
            0x85 => '\u{2026}',
            0x86 => '\u{2020}',
            0x87 => '\u{2021}',
            0x88 => '\u{02c6}',
            0x89 => '\u{2030}',
            0x8a => '\u{0160}',
            0x8b => '\u{2039}',
            0x8c => '\u{0152}',
            0x8e => '\u{017d}',
            0x91 => '\u{2018}',
            0x92 => '\u{2019}',
            0x93 => '\u{201c}',
            0x94 => '\u{201d}',
            0x95 => '\u{2022}',
            0x96 => '\u{2013}',
            0x97 => '\u{2014}',
            0x98 => '\u{02dc}',
            0x99 => '\u{2122}',
            0x9a => '\u{0161}',
            0x9b => '\u{203a}',
            0x9c => '\u{0153}',
            0x9e => '\u{017e}',
            0x9f => '\u{0178}',
            0x81 | 0x8d | 0x8f | 0x90 | 0x9d => {
                return Err(DecodeError {
                    message: format!(
                        "'charmap' codec can't decode byte 0x{byte:02x} in position {index}: character maps to <undefined>"
                    ),
                });
            }
            other => char::from(*other),
        };
        text.push(ch);
    }
    Ok(text)
}

/// Windows-1252 with U+FFFD for the five undefined bytes.
fn decode_cp1252_lossy(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| match decode_cp1252(std::slice::from_ref(byte)) {
            Ok(text) => text.chars().next().unwrap_or('\u{fffd}'),
            Err(_) => '\u{fffd}',
        })
        .collect()
}

/// `order`: `None` detects a BOM and otherwise uses native (little) order,
/// `Some(true)` is big endian, `Some(false)` is little endian.
fn utf16_layout(order: Option<bool>, bytes: &[u8]) -> (bool, usize) {
    match (order, bytes) {
        (Some(big), _) => (big, 0),
        (None, [0xfe, 0xff, ..]) => (true, 2),
        (None, [0xff, 0xfe, ..]) => (false, 2),
        (None, _) => (false, 0),
    }
}

fn utf16_codec_name(big_endian: bool) -> &'static str {
    if big_endian { "utf-16-be" } else { "utf-16-le" }
}

fn utf16_unit(bytes: &[u8], index: usize, big_endian: bool) -> u16 {
    let pair = [bytes[index], bytes[index + 1]];
    if big_endian {
        u16::from_be_bytes(pair)
    } else {
        u16::from_le_bytes(pair)
    }
}

fn decode_utf16(bytes: &[u8], order: Option<bool>) -> Result<String, DecodeError> {
    let (big_endian, skip) = utf16_layout(order, bytes);
    let rest = &bytes[skip..];
    let codec = utf16_codec_name(big_endian);
    let mut text = String::with_capacity(rest.len() / 2);
    let mut index = 0;
    while index < rest.len() {
        if rest.len() - index < 2 {
            // A lone trailing byte is only reported once every complete unit
            // before it decoded; CPython raises this from the decoder's tail
            // handling, not up front.
            let position = skip + index;
            return Err(DecodeError {
                message: format!(
                    "'{codec}' codec can't decode byte 0x{:02x} in position {position}: truncated data",
                    bytes[position]
                ),
            });
        }
        let unit = utf16_unit(rest, index, big_endian);
        if (0xd800..=0xdbff).contains(&unit) {
            if rest.len() - index < 4 {
                // A high surrogate without a full low-surrogate unit is
                // "unexpected end of data". The range CPython reports covers
                // the bytes that are actually present.
                let start = skip + index;
                let end = (start + 2).min(bytes.len() - 1);
                return Err(DecodeError {
                    message: format!(
                        "'{codec}' codec can't decode bytes in position {start}-{end}: unexpected end of data"
                    ),
                });
            }
            let low = utf16_unit(rest, index + 2, big_endian);
            if !(0xdc00..=0xdfff).contains(&low) {
                let start = skip + index;
                return Err(DecodeError {
                    message: format!(
                        "'{codec}' codec can't decode bytes in position {start}-{}: illegal UTF-16 surrogate",
                        start + 1
                    ),
                });
            }
            let code = 0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(low) - 0xdc00);
            text.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            index += 4;
        } else if (0xdc00..=0xdfff).contains(&unit) {
            let start = skip + index;
            return Err(DecodeError {
                message: format!(
                    "'{codec}' codec can't decode bytes in position {start}-{}: illegal encoding",
                    start + 1
                ),
            });
        } else {
            text.push(char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}'));
            index += 2;
        }
    }
    Ok(text)
}

fn decode_utf16_lossy(bytes: &[u8], order: Option<bool>) -> String {
    let (big_endian, skip) = utf16_layout(order, bytes);
    let rest = &bytes[skip..];
    let full = rest.len() - rest.len() % 2;
    let units: Vec<u16> = rest[..full]
        .chunks_exact(2)
        .map(|pair| {
            if big_endian {
                u16::from_be_bytes([pair[0], pair[1]])
            } else {
                u16::from_le_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    let mut text: String = char::decode_utf16(units)
        .map(|decoded| decoded.unwrap_or('\u{fffd}'))
        .collect();
    if rest.len() % 2 != 0 {
        text.push('\u{fffd}');
    }
    text
}

impl Charset {
    /// Decode strictly, returning CPython's error text on failure.
    ///
    /// # Errors
    /// Returns [`DecodeError`] for any byte the charset cannot represent.
    pub fn decode_strict(self, bytes: &[u8]) -> Result<String, DecodeError> {
        match self {
            Self::Utf8 => decode_utf8_named(bytes, "utf-8"),
            Self::Utf8Sig => {
                let body = bytes.strip_prefix(&[0xef, 0xbb, 0xbf][..]).unwrap_or(bytes);
                decode_utf8_named(body, "utf-8")
            }
            Self::Utf16 => decode_utf16(bytes, None),
            Self::Utf16Le => decode_utf16(bytes, Some(false)),
            Self::Utf16Be => decode_utf16(bytes, Some(true)),
            Self::Ascii => decode_ascii(bytes),
            Self::Latin1 => Ok(bytes.iter().map(|byte| char::from(*byte)).collect()),
            Self::Cp1252 => decode_cp1252(bytes),
        }
    }

    /// Decode with `errors="replace"`, matching Python's U+FFFD substitution.
    #[must_use]
    pub fn decode_lossy(self, bytes: &[u8]) -> String {
        match self {
            Self::Utf8 => decode_utf8_lossy(bytes),
            Self::Utf8Sig => {
                let body = bytes.strip_prefix(&[0xef, 0xbb, 0xbf][..]).unwrap_or(bytes);
                decode_utf8_lossy(body)
            }
            Self::Utf16 => decode_utf16_lossy(bytes, None),
            Self::Utf16Le => decode_utf16_lossy(bytes, Some(false)),
            Self::Utf16Be => decode_utf16_lossy(bytes, Some(true)),
            Self::Ascii => bytes
                .iter()
                .map(|byte| {
                    if byte.is_ascii() {
                        char::from(*byte)
                    } else {
                        '\u{fffd}'
                    }
                })
                .collect(),
            Self::Latin1 => bytes.iter().map(|byte| char::from(*byte)).collect(),
            Self::Cp1252 => decode_cp1252_lossy(bytes),
        }
    }
}

/// Decode `name=value` percent escapes with `charset`, `errors="replace"`.
///
/// `urllib.parse.unquote` only percent-decodes runs of ASCII characters; a
/// non-ASCII character splits the input and passes through untouched, even when
/// it lies between two escapes.
#[must_use]
pub fn percent_decode(input: &str, charset: Charset) -> String {
    let mut output = String::with_capacity(input.len());
    let mut run = String::new();
    let flush = |run: &mut String, output: &mut String| {
        if !run.is_empty() {
            output.push_str(&decode_percent_run(run, charset));
            run.clear();
        }
    };
    for ch in input.chars() {
        if ch.is_ascii() {
            run.push(ch);
        } else {
            flush(&mut run, &mut output);
            output.push(ch);
        }
    }
    flush(&mut run, &mut output);
    output
}

fn decode_percent_run(run: &str, charset: Charset) -> String {
    if !run.contains('%') {
        return run.to_string();
    }
    let bytes = run.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = hex_value(bytes[index + 1]);
            let low = hex_value(bytes[index + 2]);
            if let (Some(high), Some(low)) = (high, low) {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    charset.decode_lossy(&decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_non_text_are_distinguished() {
        assert_eq!(resolve("nope"), CharsetResolution::Unknown);
        assert_eq!(resolve("bz2"), CharsetResolution::NonText);
        assert_eq!(resolve("utf 8"), CharsetResolution::Use(Charset::Utf8));
        assert_eq!(
            resolve("ISO-8859-1"),
            CharsetResolution::Use(Charset::Latin1)
        );
    }

    #[test]
    fn resolution_matches_codecs_lookup_edge_cases() {
        // `iso.8859.1` hits the dotted alias fallback; `utf.8` only has a
        // dotted module name and must stay unknown.
        assert_eq!(
            resolve("iso.8859.1"),
            CharsetResolution::Use(Charset::Latin1)
        );
        assert_eq!(resolve("utf.8"), CharsetResolution::Unknown);
        assert_eq!(resolve("utf8"), CharsetResolution::Use(Charset::Utf8));
        assert_eq!(resolve("unicode"), CharsetResolution::Unknown);
        assert_eq!(resolve("unicode-little"), CharsetResolution::Unknown);
        assert_eq!(resolve("utf16le"), CharsetResolution::Unknown);
        // `ANSI` is an alias for `mbcs` on Windows, so CPython resolves it to
        // a text codec this port cannot decode.
        assert_eq!(resolve("ANSI"), CharsetResolution::UnsupportedText);
        assert_eq!(resolve("utf-32"), CharsetResolution::UnsupportedText);
        assert_eq!(resolve("mbcs"), CharsetResolution::UnsupportedText);
        assert_eq!(resolve("hex"), CharsetResolution::NonText);
        assert_eq!(resolve("undefined"), CharsetResolution::NonText);
        assert_eq!(resolve(""), CharsetResolution::Unknown);
    }

    #[test]
    fn utf8_errors_match_cpython_text() {
        let error = Charset::Utf8
            .decode_strict(b"{\"a\":\"\xff\"}")
            .unwrap_err();
        assert_eq!(
            error.message,
            "'utf-8' codec can't decode byte 0xff in position 6: invalid start byte"
        );
        let error = Charset::Utf8.decode_strict(b"\xe4\xbd").unwrap_err();
        assert_eq!(
            error.message,
            "'utf-8' codec can't decode bytes in position 0-1: unexpected end of data"
        );
        let error = Charset::Utf8.decode_strict(b"\xe4\xbdA").unwrap_err();
        assert_eq!(
            error.message,
            "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte"
        );
        let error = Charset::Utf8.decode_strict(b"\xc3").unwrap_err();
        assert_eq!(
            error.message,
            "'utf-8' codec can't decode byte 0xc3 in position 0: unexpected end of data"
        );
        let error = Charset::Utf8Sig
            .decode_strict(b"\xef\xbb\xbf\xc3")
            .unwrap_err();
        assert_eq!(
            error.message,
            "'utf-8' codec can't decode byte 0xc3 in position 0: unexpected end of data"
        );
    }

    #[test]
    fn replacement_matches_cpython() {
        assert_eq!(Charset::Utf8.decode_lossy(b"a\x81b"), "a\u{fffd}b");
        assert_eq!(Charset::Utf8.decode_lossy(b"\xe2\x82"), "\u{fffd}");
        assert_eq!(Charset::Utf8.decode_lossy(b"\xe2\x82("), "\u{fffd}(");
        assert_eq!(Charset::Ascii.decode_lossy(b"a\x81b"), "a\u{fffd}b");
        assert_eq!(Charset::Cp1252.decode_lossy(b"a\x81b"), "a\u{fffd}b");
        assert_eq!(Charset::Cp1252.decode_lossy(b"\x80"), "\u{20ac}");
    }

    #[test]
    fn utf16_detects_bom_and_native_order() {
        let little: Vec<u8> = "abc".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(Charset::Utf16.decode_strict(&little).expect("le"), "abc");
        assert_eq!(
            Charset::Utf16
                .decode_strict(&[0xff, 0xfe, b'a', 0, b'b', 0])
                .expect("bom"),
            "ab"
        );
        assert_eq!(
            Charset::Utf16
                .decode_strict(&[0xfe, 0xff, 0, b'a', 0, b'b'])
                .expect("bom be"),
            "ab"
        );
    }

    #[test]
    fn utf16_errors_match_cpython_text() {
        let cases: &[(&[u8], &str)] = &[
            (
                &[0x00, 0xd8],
                "'utf-16-le' codec can't decode bytes in position 0-1: unexpected end of data",
            ),
            (
                &[0x00, 0xd8, 0x00, 0x00],
                "'utf-16-le' codec can't decode bytes in position 0-1: illegal UTF-16 surrogate",
            ),
            (
                &[0x00, 0xdc],
                "'utf-16-le' codec can't decode bytes in position 0-1: illegal encoding",
            ),
            (
                &[0x61, 0x00, 0xdc],
                "'utf-16-le' codec can't decode byte 0xdc in position 2: truncated data",
            ),
            (
                &[0x61],
                "'utf-16-le' codec can't decode byte 0x61 in position 0: truncated data",
            ),
            (
                &[0x00, 0x61, 0x00, 0xd8, 0x00, 0x62, 0x00],
                "'utf-16-le' codec can't decode bytes in position 2-3: illegal UTF-16 surrogate",
            ),
        ];
        for (bytes, expected) in cases {
            let error = Charset::Utf16Le.decode_strict(bytes).unwrap_err();
            assert_eq!(&error.message, expected, "{bytes:02x?}");
        }
        let error = Charset::Utf16Be
            .decode_strict(&[0x00, 0x61, 0xdc, 0x00])
            .unwrap_err();
        assert_eq!(
            error.message,
            "'utf-16-be' codec can't decode bytes in position 2-3: illegal encoding"
        );
        let error = Charset::Utf16
            .decode_strict(&[0xff, 0xfe, 0x00, 0xd8])
            .unwrap_err();
        assert_eq!(
            error.message,
            "'utf-16-le' codec can't decode bytes in position 2-3: unexpected end of data"
        );
    }

    #[test]
    fn utf16_lossy_matches_cpython() {
        assert_eq!(
            Charset::Utf16Le.decode_lossy(&[0x00, 0xd8, 0x61, 0x00]),
            "\u{fffd}a"
        );
        // CPython's replacement decoder drops a lone trailing byte into a
        // single `�` rather than keeping the ASCII half.
        assert_eq!(Charset::Utf16Le.decode_lossy(b"a"), "\u{fffd}");
        assert_eq!(
            Charset::Utf16Le.decode_lossy(&[0x00, 0xd8, 0x00, 0x00]),
            "\u{fffd}\u{0}"
        );
    }

    #[test]
    fn percent_decoding_respects_ascii_runs() {
        assert_eq!(percent_decode("a%20b", Charset::Utf8), "a b");
        assert_eq!(percent_decode("a%zz", Charset::Utf8), "a%zz");
        assert_eq!(
            percent_decode("%C3%A9\u{e9}", Charset::Latin1),
            "\u{c3}\u{a9}\u{e9}"
        );
        assert_eq!(percent_decode("%", Charset::Utf8), "%");
        assert_eq!(percent_decode("%2", Charset::Utf8), "%2");
        assert_eq!(percent_decode("%FF", Charset::Utf8), "\u{fffd}");
    }

    #[test]
    fn normalization_follows_cpython() {
        assert_eq!(normalize_name("UTF-8"), "utf_8");
        assert_eq!(normalize_name("  -- ;#"), "");
        assert_eq!(normalize_name("utf\u{2011}8"), "utf_8");
        assert_eq!(normalize_name("iso.8859.1"), "iso.8859.1");
        assert_eq!(normalize_name("caf\u{e9}"), "caf");
    }
}
