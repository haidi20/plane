// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! The two JSON renderings Django uses, kept byte-compatible.
//!
//! * `JsonResponse` (used by `plane/web/views.py` and the 404 handler) writes
//!   `{"key": "value"}` with a space after `:` and `,`, and escapes every
//!   non-ASCII scalar (`ensure_ascii=True`).
//! * DRF (`JSONRenderer`) writes compact JSON, keeps non-ASCII as raw UTF-8, and
//!   still escapes `U+2028`/`U+2029` for JavaScript safety.
//!
//! `serde_json` is deliberately not used for encoding. Three behaviours differ:
//!
//! | Input    | Python               | `serde_json` |
//! |----------|----------------------|--------------|
//! | `1e-5`   | `1e-05`              | `0.00001`    |
//! | `NaN`    | `ValueError` -> 500  | `null`       |
//! | `U+2028` | `\u2028`             | raw          |
//!
//! So this module walks `serde::Serialize` once and writes the bytes itself. The
//! float text comes from [`crate::python_repr::repr_float`], checked against 218
//! frozen `repr()` outputs.

use std::fmt;
use std::io;

use serde::Serialize;
use serde::ser::{self, Serializer};

use crate::python_repr::repr_float;

/// Which `json.dumps` defaults the body must reproduce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    /// DRF `JSONRenderer`: compact, raw UTF-8, `U+2028`/`U+2029` escaped.
    Drf,
    /// Django `JsonResponse`: one space after `:`/`,`, everything above `~`
    /// escaped as `\uXXXX`.
    Django,
}

/// Why a value could not be rendered.
#[derive(Debug)]
pub enum JsonError {
    /// `json.dumps(..., allow_nan=False)` rejects `NaN`/`Infinity`; DRF surfaces
    /// that as a 500 instead of writing `null`.
    NonFinite(f64),
    /// `json.dumps` cannot represent a map whose key is not a string.
    NonStringKey,
    /// The destination rejected the bytes. Only reachable for a failing writer;
    /// the public API writes into memory.
    Io(io::Error),
    /// [`RawNumber::new`] was given text that is not a JSON number. Only
    /// reachable from a bug in the caller that builds a `RawNumber`.
    InvalidRawNumber,
    /// The value contains a lone UTF-16 surrogate. Python parses such a string
    /// but fails while rendering it, so the request becomes a 500; there is no
    /// JSON bytes to write.
    LoneSurrogate,
}

impl fmt::Display for JsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite(value) => write!(
                formatter,
                "Out of range float values are not JSON compliant: {value}"
            ),
            Self::NonStringKey => {
                formatter.write_str("keys must be str, int, float, bool or None, not object")
            }
            Self::Io(error) => write!(formatter, "failed to write JSON: {error}"),
            Self::InvalidRawNumber => formatter.write_str("invalid raw JSON number"),
            Self::LoneSurrogate => formatter.write_str("lone UTF-16 surrogate is not encodable"),
        }
    }
}

impl std::error::Error for JsonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for JsonError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl ser::Error for JsonError {
    fn custom<T: fmt::Display>(_: T) -> Self {
        // Serde's own message would name Rust types, which never reach the wire.
        Self::NonStringKey
    }
}

/// Sentinel name used by [`RawNumber`]; the encoder recognises it and writes
/// the digits without quotes.
const RAW_NUMBER: &str = "$plane.raw_number";

/// Sentinel name used by parsed Python strings that hold a lone surrogate.
/// The encoder turns it into [`JsonError::LoneSurrogate`] instead of writing
/// replacement characters; Python raises `UnicodeEncodeError` there.
pub(crate) const LONE_SURROGATE: &str = "$plane.lone_surrogate";

/// A number whose text must reach the wire unchanged.
///
/// Python integers are unbounded, so a 30-digit id cannot travel through `i64`
/// or `f64` without changing. `serde` has no "write these bytes as a number"
/// method, so this type rides `serialize_newtype_struct` with a sentinel name
/// that this module's encoder understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawNumber<'a>(std::borrow::Cow<'a, str>);

impl<'a> RawNumber<'a> {
    /// Wrap already-validated JSON number text.
    #[must_use]
    pub fn new(text: &'a str) -> Self {
        Self(std::borrow::Cow::Borrowed(text))
    }

    /// Wrap number text that outlives the buffer it was parsed from.
    #[must_use]
    pub fn owned(text: String) -> Self {
        Self(std::borrow::Cow::Owned(text))
    }

    /// The wrapped text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for RawNumber<'_> {
    fn serialize<S: ser::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(RAW_NUMBER, &*self.0)
    }
}

/// Encoder state: destination plus the separator style.
///
/// `W` is any `io::Write`; the public API writes into a `String` through a
/// `Vec<u8>` so the bytes are provably UTF-8. `serde`'s compound traits need
/// `&mut Encoder` (not `Encoder`), which is why the writer is generic instead of
/// borrowed from the parent value.
struct Encoder<W> {
    writer: W,
    style: Style,
}

impl<W: io::Write> Encoder<W> {
    fn separator(&mut self, first: bool) -> io::Result<()> {
        if first {
            return Ok(());
        }
        self.writer.write_all(match self.style {
            Style::Drf => b",",
            Style::Django => b", ",
        })
    }

    fn key_value_separator(&mut self) -> io::Result<()> {
        self.writer.write_all(match self.style {
            Style::Drf => b":",
            Style::Django => b": ",
        })
    }

    fn write_key(&mut self, key: &str) -> io::Result<()> {
        write_string(&mut self.writer, key, self.style)
    }
}

impl<'a, W: io::Write> Serializer for &'a mut Encoder<W> {
    type Ok = ();
    type Error = JsonError;
    type SerializeSeq = SeqEncoder<'a, W>;
    type SerializeTuple = SeqEncoder<'a, W>;
    type SerializeTupleStruct = SeqEncoder<'a, W>;
    type SerializeTupleVariant = SeqEncoder<'a, W>;
    type SerializeMap = MapEncoder<'a, W>;
    type SerializeStruct = MapEncoder<'a, W>;
    type SerializeStructVariant = MapEncoder<'a, W>;

    fn serialize_bool(self, value: bool) -> Result<(), Self::Error> {
        self.writer
            .write_all(if value { b"true" } else { b"false" })
            .map_err(JsonError::Io)
    }

    fn serialize_i8(self, value: i8) -> Result<(), Self::Error> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i16(self, value: i16) -> Result<(), Self::Error> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i32(self, value: i32) -> Result<(), Self::Error> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i64(self, value: i64) -> Result<(), Self::Error> {
        self.writer
            .write_all(value.to_string().as_bytes())
            .map_err(JsonError::Io)
    }

    fn serialize_i128(self, value: i128) -> Result<(), Self::Error> {
        self.writer
            .write_all(value.to_string().as_bytes())
            .map_err(JsonError::Io)
    }

    fn serialize_u8(self, value: u8) -> Result<(), Self::Error> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u16(self, value: u16) -> Result<(), Self::Error> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u32(self, value: u32) -> Result<(), Self::Error> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u64(self, value: u64) -> Result<(), Self::Error> {
        self.writer
            .write_all(value.to_string().as_bytes())
            .map_err(JsonError::Io)
    }

    fn serialize_u128(self, value: u128) -> Result<(), Self::Error> {
        self.writer
            .write_all(value.to_string().as_bytes())
            .map_err(JsonError::Io)
    }

    fn serialize_f32(self, value: f32) -> Result<(), Self::Error> {
        self.serialize_f64(f64::from(value))
    }

    fn serialize_f64(self, value: f64) -> Result<(), Self::Error> {
        if !value.is_finite() {
            return Err(JsonError::NonFinite(value));
        }
        self.writer
            .write_all(repr_float(value).as_bytes())
            .map_err(JsonError::Io)
    }

    fn serialize_char(self, value: char) -> Result<(), Self::Error> {
        self.serialize_str(value.encode_utf8(&mut [0; 4]))
    }

    fn serialize_str(self, value: &str) -> Result<(), Self::Error> {
        write_string(&mut self.writer, value, self.style).map_err(JsonError::Io)
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<(), Self::Error> {
        self.writer.write_all(b"[").map_err(JsonError::Io)?;
        for (index, byte) in value.iter().enumerate() {
            if index > 0 {
                self.separator(false)?;
            }
            self.writer
                .write_all(u64::from(*byte).to_string().as_bytes())
                .map_err(JsonError::Io)?;
        }
        self.writer.write_all(b"]").map_err(JsonError::Io)
    }

    fn serialize_none(self) -> Result<(), Self::Error> {
        self.writer.write_all(b"null").map_err(JsonError::Io)
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), Self::Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), Self::Error> {
        self.writer.write_all(b"null").map_err(JsonError::Io)
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Self::Error> {
        self.serialize_unit()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<(), Self::Error> {
        self.serialize_str(variant)
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        if name == LONE_SURROGATE {
            return Err(JsonError::LoneSurrogate);
        }
        if name != RAW_NUMBER {
            return value.serialize(self);
        }
        // Render with our own string rules, then drop the quotes. The wrapper
        // already restricted the text to number characters, so no escape can
        // appear inside.
        let mut scratch = Vec::new();
        let mut inner = Encoder {
            writer: &mut scratch,
            style: self.style,
        };
        value.serialize(&mut inner)?;
        let text = std::str::from_utf8(&scratch).map_err(|_| JsonError::InvalidRawNumber)?;
        let digits = text
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .ok_or(JsonError::InvalidRawNumber)?;
        if !digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E'))
        {
            return Err(JsonError::InvalidRawNumber);
        }
        self.writer
            .write_all(digits.as_bytes())
            .map_err(JsonError::Io)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.writer.write_all(b"{").map_err(JsonError::Io)?;
        self.write_key(variant)?;
        self.key_value_separator()?;
        value.serialize(&mut *self)?;
        self.writer.write_all(b"}").map_err(JsonError::Io)
    }

    fn serialize_seq(self, _length: Option<usize>) -> Result<SeqEncoder<'a, W>, Self::Error> {
        self.writer.write_all(b"[").map_err(JsonError::Io)?;
        Ok(SeqEncoder {
            serializer: self,
            first: true,
            closing: b"]",
        })
    }

    fn serialize_tuple(self, length: usize) -> Result<SeqEncoder<'a, W>, Self::Error> {
        self.serialize_seq(Some(length))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        length: usize,
    ) -> Result<SeqEncoder<'a, W>, Self::Error> {
        self.serialize_seq(Some(length))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        length: usize,
    ) -> Result<SeqEncoder<'a, W>, Self::Error> {
        self.writer.write_all(b"{").map_err(JsonError::Io)?;
        self.write_key(variant)?;
        self.key_value_separator()?;
        self.writer.write_all(b"[").map_err(JsonError::Io)?;
        let _ = length;
        Ok(SeqEncoder {
            serializer: self,
            first: true,
            closing: b"]}",
        })
    }

    fn serialize_map(self, _length: Option<usize>) -> Result<MapEncoder<'a, W>, Self::Error> {
        self.writer.write_all(b"{").map_err(JsonError::Io)?;
        Ok(MapEncoder {
            serializer: self,
            first: true,
            closing: b"}",
        })
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        length: usize,
    ) -> Result<MapEncoder<'a, W>, Self::Error> {
        self.serialize_map(Some(length))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _length: usize,
    ) -> Result<MapEncoder<'a, W>, Self::Error> {
        self.writer.write_all(b"{").map_err(JsonError::Io)?;
        self.write_key(variant)?;
        self.key_value_separator()?;
        self.writer.write_all(b"{").map_err(JsonError::Io)?;
        Ok(MapEncoder {
            serializer: self,
            first: true,
            closing: b"}}",
        })
    }

    fn collect_str<T: ?Sized + fmt::Display>(self, value: &T) -> Result<(), Self::Error> {
        self.serialize_str(&value.to_string())
    }
}

/// Array-like helpers; one struct for seq, tuple, tuple struct and tuple variant.
struct SeqEncoder<'a, W> {
    serializer: &'a mut Encoder<W>,
    first: bool,
    /// `b"]"` for an array, `b"]}"` when the array is the payload of a tuple
    /// variant and the wrapper object is still open.
    closing: &'static [u8],
}

impl<W: io::Write> SeqEncoder<'_, W> {
    fn element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), JsonError> {
        self.serializer
            .separator(self.first)
            .map_err(JsonError::Io)?;
        self.first = false;
        value.serialize(&mut *self.serializer)
    }

    fn finish(self) -> Result<(), JsonError> {
        self.serializer
            .writer
            .write_all(self.closing)
            .map_err(JsonError::Io)
    }
}

impl<W: io::Write> ser::SerializeSeq for SeqEncoder<'_, W> {
    type Ok = ();
    type Error = JsonError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.element(value)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

impl<W: io::Write> ser::SerializeTuple for SeqEncoder<'_, W> {
    type Ok = ();
    type Error = JsonError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.element(value)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

impl<W: io::Write> ser::SerializeTupleStruct for SeqEncoder<'_, W> {
    type Ok = ();
    type Error = JsonError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.element(value)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

impl<W: io::Write> ser::SerializeTupleVariant for SeqEncoder<'_, W> {
    type Ok = ();
    type Error = JsonError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.element(value)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

/// Object-like helpers; one struct for map, struct and struct variant.
struct MapEncoder<'a, W> {
    serializer: &'a mut Encoder<W>,
    first: bool,
    /// `b"}"` for a map, `b"}}"` when the object is the payload of a struct
    /// variant and the wrapper object is still open.
    closing: &'static [u8],
}

impl<W: io::Write> MapEncoder<'_, W> {
    fn entry<T: ?Sized + Serialize, V: ?Sized + Serialize>(
        &mut self,
        key: &T,
        value: &V,
    ) -> Result<(), JsonError> {
        self.serializer
            .separator(self.first)
            .map_err(JsonError::Io)?;
        self.first = false;
        key.serialize(StringOnly {
            writer: &mut self.serializer.writer,
            style: self.serializer.style,
        })?;
        self.serializer
            .key_value_separator()
            .map_err(JsonError::Io)?;
        value.serialize(&mut *self.serializer)
    }

    fn finish(self) -> Result<(), JsonError> {
        self.serializer
            .writer
            .write_all(self.closing)
            .map_err(JsonError::Io)
    }
}

impl<W: io::Write> ser::SerializeMap for MapEncoder<'_, W> {
    type Ok = ();
    type Error = JsonError;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), Self::Error> {
        self.serializer
            .separator(self.first)
            .map_err(JsonError::Io)?;
        self.first = false;
        key.serialize(StringOnly {
            writer: &mut self.serializer.writer,
            style: self.serializer.style,
        })
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.serializer
            .key_value_separator()
            .map_err(JsonError::Io)?;
        value.serialize(&mut *self.serializer)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

impl<W: io::Write> ser::SerializeStruct for MapEncoder<'_, W> {
    type Ok = ();
    type Error = JsonError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.entry(&key, value)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

impl<W: io::Write> ser::SerializeStructVariant for MapEncoder<'_, W> {
    type Ok = ();
    type Error = JsonError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.entry(&key, value)
    }

    fn end(self) -> Result<(), Self::Error> {
        self.finish()
    }
}

/// Serializer that only accepts strings; used for object keys.
struct StringOnly<'w, W> {
    writer: &'w mut W,
    style: Style,
}

macro_rules! reject_key {
    ($($method:ident($ty:ty)),* $(,)?) => {
        $(
            fn $method(self, _value: $ty) -> Result<(), JsonError> {
                Err(JsonError::NonStringKey)
            }
        )*
    };
}

impl<W: io::Write> Serializer for StringOnly<'_, W> {
    type Ok = ();
    type Error = JsonError;
    type SerializeSeq = ser::Impossible<(), JsonError>;
    type SerializeTuple = ser::Impossible<(), JsonError>;
    type SerializeTupleStruct = ser::Impossible<(), JsonError>;
    type SerializeTupleVariant = ser::Impossible<(), JsonError>;
    type SerializeMap = ser::Impossible<(), JsonError>;
    type SerializeStruct = ser::Impossible<(), JsonError>;
    type SerializeStructVariant = ser::Impossible<(), JsonError>;

    fn serialize_str(self, value: &str) -> Result<(), Self::Error> {
        write_string(self.writer, value, self.style).map_err(JsonError::Io)
    }

    fn serialize_char(self, value: char) -> Result<(), Self::Error> {
        self.serialize_str(value.encode_utf8(&mut [0; 4]))
    }

    reject_key!(
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128),
        serialize_f32(f32),
        serialize_f64(f64),
        serialize_bytes(&[u8]),
    );

    fn serialize_none(self) -> Result<(), Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_some<T: ?Sized + Serialize>(self, _value: &T) -> Result<(), Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_unit(self) -> Result<(), Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
    ) -> Result<(), Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        _value: &T,
    ) -> Result<(), Self::Error> {
        if name == LONE_SURROGATE {
            Err(JsonError::LoneSurrogate)
        } else {
            Err(JsonError::NonStringKey)
        }
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<(), Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_seq(self, _length: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_tuple(self, _length: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_map(self, _length: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(JsonError::NonStringKey)
    }

    fn collect_str<T: ?Sized + fmt::Display>(self, value: &T) -> Result<(), Self::Error> {
        self.serialize_str(&value.to_string())
    }
}

/// Python's string escaping for a JSON body.
///
/// Short escapes match `json.dumps` (`\b`, `\t`, `\n`, `\f`, `\r`); every other
/// control character becomes `\u00XX`. Beyond that the two renderers disagree:
/// Django escapes everything above `~` (including `0x7f`), DRF keeps raw UTF-8
/// but escapes the JavaScript line separators.
fn write_string<W: io::Write>(out: &mut W, value: &str, style: Style) -> io::Result<()> {
    let mut buffer = String::with_capacity(value.len() + 2);
    buffer.push('"');
    for ch in value.chars() {
        match ch {
            '"' => buffer.push_str("\\\""),
            '\\' => buffer.push_str("\\\\"),
            '\u{8}' => buffer.push_str("\\b"),
            '\t' => buffer.push_str("\\t"),
            '\n' => buffer.push_str("\\n"),
            '\u{c}' => buffer.push_str("\\f"),
            '\r' => buffer.push_str("\\r"),
            '\u{2028}' => buffer.push_str("\\u2028"),
            '\u{2029}' => buffer.push_str("\\u2029"),
            ch if (ch as u32) < 0x20 => buffer.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => {
                let code = ch as u32;
                if style == Style::Django && code > 0x7e {
                    if code <= 0xffff {
                        buffer.push_str(&format!("\\u{code:04x}"));
                    } else {
                        let adjusted = code - 0x1_0000;
                        let high = 0xd800 + (adjusted >> 10);
                        let low = 0xdc00 + (adjusted & 0x3ff);
                        buffer.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
                    }
                } else if style == Style::Django && code == 0x7f {
                    buffer.push_str("\\u007f");
                } else {
                    buffer.push(ch);
                }
            }
        }
    }
    buffer.push('"');
    out.write_all(buffer.as_bytes())
}

fn render<T: Serialize + ?Sized>(value: &T, style: Style) -> Result<String, JsonError> {
    let mut bytes = Vec::new();
    let mut encoder = Encoder {
        writer: &mut bytes,
        style,
    };
    value.serialize(&mut encoder)?;
    // The encoder only writes UTF-8 (the input `str`s and ASCII escapes).
    String::from_utf8(bytes).map_err(|error| {
        JsonError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            error.to_string(),
        ))
    })
}

/// Compact rendering, matching DRF's `JSONRenderer`.
///
/// # Errors
/// Returns [`JsonError::NonFinite`] for `NaN`/`Infinity`, which DRF rejects
/// instead of writing `null`.
pub fn compact_json<T: Serialize + ?Sized>(value: &T) -> Result<String, JsonError> {
    render(value, Style::Drf)
}

/// `JsonResponse`-style rendering: one space after `:` and after `,`, ASCII-only.
///
/// `json.dumps` defaults to `ensure_ascii=True`, so Django writes `"Ren\u00e9"`
/// while DRF writes raw UTF-8. Both details are part of the wire contract, so
/// they live here rather than at each call site.
///
/// # Errors
/// Returns [`JsonError::NonFinite`] for `NaN`/`Infinity`.
pub fn spaced_json<T: Serialize + ?Sized>(value: &T) -> Result<String, JsonError> {
    render(value, Style::Django)
}

/// A rendered body plus the `Content-Type` it must carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonResponse {
    /// Already-serialized body.
    pub body: String,
    /// Value for the `Content-Type` header.
    pub content_type: &'static str,
}

impl JsonResponse {
    /// `JsonResponse`-style body (`application/json`).
    ///
    /// # Errors
    /// Returns [`JsonError`] when the value cannot be encoded.
    pub fn django<T: Serialize + ?Sized>(value: &T) -> Result<Self, JsonError> {
        Ok(Self {
            body: spaced_json(value)?,
            content_type: "application/json",
        })
    }

    /// DRF-style compact body (`application/json`).
    ///
    /// # Errors
    /// Returns [`JsonError`] when the value cannot be encoded.
    pub fn drf<T: Serialize + ?Sized>(value: &T) -> Result<Self, JsonError> {
        Ok(Self {
            body: compact_json(value)?,
            content_type: "application/json",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Serialize)]
    struct Raw {
        v: f64,
    }

    #[test]
    fn django_health_matches_golden_bytes() {
        let rendered = spaced_json(&json!({"status": "OK"})).expect("encode");
        assert_eq!(rendered, r#"{"status": "OK"}"#);
    }

    #[test]
    fn drf_stays_compact() {
        let rendered = compact_json(&json!({"status": "OK"})).expect("encode");
        assert_eq!(rendered, r#"{"status":"OK"}"#);
    }

    #[test]
    fn separators_inside_strings_are_untouched() {
        let rendered = spaced_json(&json!({"a": "x:y,z"})).expect("encode");
        assert_eq!(rendered, r#"{"a": "x:y,z"}"#);
    }

    #[test]
    fn django_escapes_non_ascii_but_drf_keeps_utf8() {
        // Measured: JsonResponse -> b'{"name": "Ren\u00e9 \u2014 \u4f60\u597d"}'
        //           JSONRenderer -> b'{"name":"Ren\xc3\xa9 \xe2\x80\x94 \xe4\xbd\xa0\xe5\xa5\xbd"}'
        let value = json!({"name": "Ren\u{e9} \u{2014} \u{4f60}\u{597d}"});
        assert_eq!(
            spaced_json(&value).expect("encode"),
            r#"{"name": "Ren\u00e9 \u2014 \u4f60\u597d"}"#
        );
        assert_eq!(
            compact_json(&value).expect("encode"),
            "{\"name\":\"Ren\u{e9} \u{2014} \u{4f60}\u{597d}\"}"
        );
    }

    #[test]
    fn django_uses_surrogate_pairs_for_astral_characters() {
        // Measured: JsonResponse -> b'{"emoji": "\\ud83d\\ude00"}'
        let rendered = spaced_json(&json!({"emoji": "\u{1f600}"})).expect("encode");
        assert_eq!(rendered, r#"{"emoji": "\ud83d\ude00"}"#);
    }

    #[test]
    fn del_is_escaped_only_on_the_django_path() {
        // Measured: JsonResponse escapes `0x7f`; DRF's C encoder keeps it raw.
        let value = json!({"v": "a\u{7f}b"});
        assert_eq!(spaced_json(&value).expect("encode"), r#"{"v": "a\u007fb"}"#);
        assert_eq!(
            compact_json(&value).expect("encode"),
            "{\"v\":\"a\u{7f}b\"}"
        );
    }

    #[test]
    fn line_and_paragraph_separators_are_escaped_on_both_paths() {
        // DRF post-processes the encoded string for JavaScript safety.
        let value = json!({"v": "a\u{2028}b\u{2029}c"});
        assert_eq!(
            compact_json(&value).expect("encode"),
            r#"{"v":"a\u2028b\u2029c"}"#
        );
        assert_eq!(
            spaced_json(&value).expect("encode"),
            r#"{"v": "a\u2028b\u2029c"}"#
        );
    }

    #[test]
    fn short_escapes_and_other_controls_match_json_dumps() {
        let value = json!({"v": "a\tb\nc\u{8}d\u{c}e\rf\u{1}g"});
        assert_eq!(
            compact_json(&value).expect("encode"),
            r#"{"v":"a\tb\nc\bd\fe\rf\u0001g"}"#
        );
    }

    #[test]
    fn quotes_and_backslashes_are_escaped_once() {
        let value = json!({"v": "a\"b\\c"});
        assert_eq!(compact_json(&value).expect("encode"), r#"{"v":"a\"b\\c"}"#);
    }

    #[test]
    fn floats_use_python_repr_not_serdes_shortest_form() {
        let rendered =
            compact_json(&json!({"tiny": 1e-5, "big": 1e16, "whole": 65535.0})).expect("encode");
        assert_eq!(rendered, r#"{"tiny":1e-05,"big":1e+16,"whole":65535.0}"#);
        let spaced = spaced_json(&json!({"tiny": 1e-5, "big": 1e16})).expect("encode");
        assert_eq!(spaced, r#"{"tiny": 1e-05, "big": 1e+16}"#);
    }

    #[test]
    fn non_finite_floats_fail_instead_of_becoming_null() {
        assert!(matches!(
            compact_json(&Raw { v: f64::NAN }),
            Err(JsonError::NonFinite(_))
        ));
        assert!(matches!(
            compact_json(&Raw { v: f64::INFINITY }),
            Err(JsonError::NonFinite(_))
        ));
        assert!(matches!(
            spaced_json(&Raw {
                v: f64::NEG_INFINITY
            }),
            Err(JsonError::NonFinite(_))
        ));
        assert_eq!(
            compact_json(&Raw { v: 1.0 }).expect("encode"),
            r#"{"v":1.0}"#
        );
    }

    #[test]
    fn nested_containers_keep_the_same_separators() {
        let value = json!({"a": [1, {"b": "c"}], "d": {}});
        assert_eq!(
            compact_json(&value).expect("encode"),
            r#"{"a":[1,{"b":"c"}],"d":{}}"#
        );
        assert_eq!(
            spaced_json(&value).expect("encode"),
            r#"{"a": [1, {"b": "c"}], "d": {}}"#
        );
    }

    #[derive(Serialize)]
    struct RawFields<'a> {
        id: RawNumber<'a>,
        neg: RawNumber<'a>,
        exp: RawNumber<'a>,
    }

    #[test]
    fn raw_numbers_keep_every_digit() {
        // Python ints are unbounded; `f64`/`i64` would round this id.
        let value = RawFields {
            id: RawNumber::new("123456789012345678901234567890"),
            neg: RawNumber::new("-0.0"),
            exp: RawNumber::new("1E+16"),
        };
        assert_eq!(
            compact_json(&value).expect("encode"),
            r#"{"id":123456789012345678901234567890,"neg":-0.0,"exp":1E+16}"#
        );
        assert_eq!(
            spaced_json(&value).expect("encode"),
            r#"{"id": 123456789012345678901234567890, "neg": -0.0, "exp": 1E+16}"#
        );
    }

    #[test]
    fn raw_numbers_reject_non_numeric_text() {
        #[derive(Serialize)]
        struct Bad<'a> {
            id: RawNumber<'a>,
        }
        let value = Bad {
            id: RawNumber::new("12abc"),
        };
        assert!(matches!(
            compact_json(&value),
            Err(JsonError::InvalidRawNumber)
        ));
    }

    #[test]
    fn key_order_follows_insertion_order() {
        let rendered = compact_json(&json!({"z": 1, "a": 2, "m": 3})).expect("encode");
        assert_eq!(rendered, r#"{"z":1,"a":2,"m":3}"#);
    }

    #[test]
    fn scalars_match_python() {
        assert_eq!(compact_json(&true).expect("encode"), "true");
        assert_eq!(compact_json(&false).expect("encode"), "false");
        assert_eq!(compact_json(&()).expect("encode"), "null");
        assert_eq!(compact_json(&Option::<u8>::None).expect("encode"), "null");
        assert_eq!(compact_json(&Some(7_u8)).expect("encode"), "7");
        assert_eq!(compact_json(&-7_i64).expect("encode"), "-7");
        assert_eq!(
            compact_json(&u64::MAX).expect("encode"),
            "18446744073709551615"
        );
        assert_eq!(
            compact_json(&-i64::MAX).expect("encode"),
            "-9223372036854775807"
        );
        assert_eq!(compact_json(&"x").expect("encode"), r#""x""#);
    }

    #[test]
    fn structs_and_tuple_variants_nest_correctly() {
        #[derive(Serialize)]
        enum Shape {
            Circle { radius: f64 },
            Pair(u8, u8),
        }
        #[derive(Serialize)]
        struct Widget {
            name: &'static str,
            shape: Shape,
        }
        let rendered = compact_json(&Widget {
            name: "a",
            shape: Shape::Circle { radius: 1.5 },
        })
        .expect("encode");
        assert_eq!(
            rendered,
            r#"{"name":"a","shape":{"Circle":{"radius":1.5}}}"#
        );
        let rendered = compact_json(&Widget {
            name: "b",
            shape: Shape::Pair(1, 2),
        })
        .expect("encode");
        assert_eq!(rendered, r#"{"name":"b","shape":{"Pair":[1,2]}}"#);
        let spaced = spaced_json(&Widget {
            name: "b",
            shape: Shape::Pair(1, 2),
        })
        .expect("encode");
        assert_eq!(spaced, r#"{"name": "b", "shape": {"Pair": [1, 2]}}"#);
    }

    #[test]
    fn non_string_map_keys_are_rejected() {
        let mut map = std::collections::BTreeMap::new();
        map.insert(1_u8, "a");
        assert!(matches!(compact_json(&map), Err(JsonError::NonStringKey)));
    }
}
