//! `str.encode` / `bytes.decode`: the codecs the VM models (utf-8 with and
//! without BOM, ascii, latin-1, utf-16 and utf-32 in every endianness) with
//! CPython's error handlers (`strict`, `ignore`, `replace`,
//! `backslashreplace`, `xmlcharrefreplace`) and its exact error messages.
//! Lone surrogates cannot exist in a VM string, so `surrogateescape` /
//! `surrogatepass` behave as `strict`.

use crate::error::{Unwind, VmException};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Codec {
    Utf8,
    Utf8Sig,
    Ascii,
    Latin1,
    Utf16,
    Utf16Le,
    Utf16Be,
    Utf32,
    Utf32Le,
    Utf32Be,
    Cp1252,
}

impl Codec {
    /// The name CPython prints in the codec's error messages.
    fn label(self) -> &'static str {
        match self {
            Codec::Utf8 | Codec::Utf8Sig => "utf-8",
            Codec::Ascii => "ascii",
            Codec::Latin1 => "latin-1",
            Codec::Utf16 | Codec::Utf16Le => "utf-16-le",
            Codec::Utf16Be => "utf-16-be",
            Codec::Utf32 | Codec::Utf32Le => "utf-32-le",
            Codec::Utf32Be => "utf-32-be",
            // Every charmap codec answers to the same name in CPython's
            // messages, whichever table it carries.
            Codec::Cp1252 => "charmap",
        }
    }
}

/// `codecs.lookup` normalisation plus the alias table for the codecs here.
fn lookup(name: &str) -> Result<Codec, Unwind> {
    let norm: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c == '_' || c == ' ' { '-' } else { c })
        .collect();
    Ok(match norm.as_str() {
        "utf-8" | "utf8" | "u8" | "utf" | "cp65001" => Codec::Utf8,
        "utf-8-sig" | "utf8-sig" => Codec::Utf8Sig,
        "ascii" | "us-ascii" | "646" | "ansi-x3.4-1968" => Codec::Ascii,
        "latin-1" | "latin1" | "iso-8859-1" | "iso8859-1" | "8859" | "cp819" | "l1" | "latin" => {
            Codec::Latin1
        }
        "utf-16" | "utf16" | "u16" => Codec::Utf16,
        "utf-16-le" | "utf-16le" | "utf16le" => Codec::Utf16Le,
        "utf-16-be" | "utf-16be" | "utf16be" => Codec::Utf16Be,
        "utf-32" | "utf32" | "u32" => Codec::Utf32,
        "utf-32-le" | "utf-32le" | "utf32le" => Codec::Utf32Le,
        "utf-32-be" | "utf-32be" | "utf32be" => Codec::Utf32Be,
        "cp1252" | "windows-1252" | "1252" => Codec::Cp1252,
        _ => {
            return Err(Unwind::Exception(VmException::new(
                "LookupError",
                format!("unknown encoding: {name}"),
            )))
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Handler {
    Strict,
    Ignore,
    Replace,
    BackslashReplace,
    XmlCharRefReplace,
}

/// Resolved only when an error actually occurs — like CPython, an unknown
/// handler name is not an error for input the codec accepts.
fn handler(errors: &str) -> Result<Handler, Unwind> {
    Ok(match errors {
        "strict" | "surrogateescape" | "surrogatepass" => Handler::Strict,
        "ignore" => Handler::Ignore,
        "replace" => Handler::Replace,
        "backslashreplace" => Handler::BackslashReplace,
        "xmlcharrefreplace" => Handler::XmlCharRefReplace,
        _ => {
            return Err(Unwind::Exception(VmException::new(
                "LookupError",
                format!("unknown error handler name '{errors}'"),
            )))
        }
    })
}

/// `'\xe9'` / `'日'` / `'\U0001f600'` — how an encode error names the
/// character.
fn char_escape(c: char) -> String {
    let n = c as u32;
    if n < 0x100 {
        format!("\\x{n:02x}")
    } else if n < 0x10000 {
        format!("\\u{n:04x}")
    } else {
        format!("\\U{n:08x}")
    }
}

fn encode_error(codec: Codec, chars: &[char], start: usize, end: usize, limit: u32) -> Unwind {
    let reason = format!("ordinal not in range({limit})");
    let message = if end - start == 1 {
        format!(
            "'{}' codec can't encode character '{}' in position {}: {}",
            codec.label(),
            char_escape(chars[start]),
            start,
            reason
        )
    } else {
        format!(
            "'{}' codec can't encode characters in position {}-{}: {}",
            codec.label(),
            start,
            end - 1,
            reason
        )
    };
    Unwind::Exception(VmException::new("UnicodeEncodeError", message))
}

/// Windows-1252: Latin-1 with 27 printable characters in place of the C1
/// controls at 0x80-0x9F. `\0` marks the five bytes that stay undefined.
#[rustfmt::skip]
const CP1252_HIGH: [char; 32] = [
    '\u{20AC}', '\0', '\u{201A}', '\u{192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2C6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\0', '\u{17D}', '\0',
    '\0', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2DC}', '\u{2122}', '\u{161}', '\u{203A}', '\u{153}', '\0', '\u{17E}', '\u{178}',
];

/// The cp1252 byte for `c`, if the table has one. Everything below 0x80 and
/// from 0xA0 up is its Latin-1 byte; the C1 block itself is unencodable.
fn cp1252_byte(c: char) -> Option<u8> {
    let n = c as u32;
    if n < 0x80 || (0xa0..0x100).contains(&n) {
        return Some(n as u8);
    }
    CP1252_HIGH
        .iter()
        .position(|&mapped| mapped != '\0' && mapped == c)
        .map(|i| 0x80 + i as u8)
}

/// A charmap codec's encode failure. Unlike the fixed-width codecs it names
/// no ordinal range: the character simply has no entry in the table.
fn charmap_encode_error(codec: Codec, chars: &[char], start: usize, end: usize) -> Unwind {
    let reason = "character maps to <undefined>";
    let message = if end - start == 1 {
        format!(
            "'{}' codec can't encode character '{}' in position {}: {}",
            codec.label(),
            char_escape(chars[start]),
            start,
            reason
        )
    } else {
        format!(
            "'{}' codec can't encode characters in position {}-{}: {}",
            codec.label(),
            start,
            end - 1,
            reason
        )
    };
    Unwind::Exception(VmException::new("UnicodeEncodeError", message))
}

/// `str.encode(encoding, errors)`.
pub fn encode(s: &str, encoding: &str, errors: &str) -> Result<Vec<u8>, Unwind> {
    let codec = lookup(encoding)?;
    match codec {
        Codec::Utf8 => Ok(s.as_bytes().to_vec()),
        Codec::Utf8Sig => {
            let mut out = vec![0xef, 0xbb, 0xbf];
            out.extend_from_slice(s.as_bytes());
            Ok(out)
        }
        Codec::Ascii | Codec::Latin1 => {
            let limit: u32 = if codec == Codec::Ascii { 128 } else { 256 };
            let chars: Vec<char> = s.chars().collect();
            let mut out = Vec::with_capacity(chars.len());
            let mut i = 0;
            while i < chars.len() {
                let n = chars[i] as u32;
                if n < limit {
                    out.push(n as u8);
                    i += 1;
                    continue;
                }
                // The run of consecutive unencodable characters is one error.
                let start = i;
                while i < chars.len() && (chars[i] as u32) >= limit {
                    i += 1;
                }
                match handler(errors)? {
                    Handler::Strict => return Err(encode_error(codec, &chars, start, i, limit)),
                    Handler::Ignore => {}
                    Handler::Replace => out.extend(std::iter::repeat_n(b'?', i - start)),
                    Handler::BackslashReplace => {
                        for &c in &chars[start..i] {
                            out.extend_from_slice(char_escape(c).as_bytes());
                        }
                    }
                    Handler::XmlCharRefReplace => {
                        for &c in &chars[start..i] {
                            out.extend_from_slice(format!("&#{};", c as u32).as_bytes());
                        }
                    }
                }
            }
            Ok(out)
        }
        Codec::Cp1252 => {
            let chars: Vec<char> = s.chars().collect();
            let mut out = Vec::with_capacity(chars.len());
            let mut i = 0;
            while i < chars.len() {
                if let Some(b) = cp1252_byte(chars[i]) {
                    out.push(b);
                    i += 1;
                    continue;
                }
                // A run of consecutive unmappable characters is one error.
                let start = i;
                while i < chars.len() && cp1252_byte(chars[i]).is_none() {
                    i += 1;
                }
                match handler(errors)? {
                    Handler::Strict => return Err(charmap_encode_error(codec, &chars, start, i)),
                    Handler::Ignore => {}
                    Handler::Replace => out.extend(std::iter::repeat_n(b'?', i - start)),
                    Handler::BackslashReplace => {
                        for &c in &chars[start..i] {
                            out.extend_from_slice(char_escape(c).as_bytes());
                        }
                    }
                    Handler::XmlCharRefReplace => {
                        for &c in &chars[start..i] {
                            out.extend_from_slice(format!("&#{};", c as u32).as_bytes());
                        }
                    }
                }
            }
            Ok(out)
        }
        Codec::Utf16 | Codec::Utf16Le | Codec::Utf16Be => {
            let mut out = Vec::with_capacity(s.len() * 2 + 2);
            let big = codec == Codec::Utf16Be;
            if codec == Codec::Utf16 {
                out.extend_from_slice(&[0xff, 0xfe]);
            }
            for unit in s.encode_utf16() {
                let bytes = if big {
                    unit.to_be_bytes()
                } else {
                    unit.to_le_bytes()
                };
                out.extend_from_slice(&bytes);
            }
            Ok(out)
        }
        Codec::Utf32 | Codec::Utf32Le | Codec::Utf32Be => {
            let mut out = Vec::with_capacity(s.len() * 4 + 4);
            let big = codec == Codec::Utf32Be;
            if codec == Codec::Utf32 {
                out.extend_from_slice(&[0xff, 0xfe, 0, 0]);
            }
            for c in s.chars() {
                let bytes = if big {
                    (c as u32).to_be_bytes()
                } else {
                    (c as u32).to_le_bytes()
                };
                out.extend_from_slice(&bytes);
            }
            Ok(out)
        }
    }
}

fn decode_error(codec: Codec, bytes: &[u8], start: usize, end: usize, reason: &str) -> Unwind {
    let message = if end - start == 1 {
        format!(
            "'{}' codec can't decode byte 0x{:02x} in position {}: {}",
            codec.label(),
            bytes[start],
            start,
            reason
        )
    } else {
        format!(
            "'{}' codec can't decode bytes in position {}-{}: {}",
            codec.label(),
            start,
            end - 1,
            reason
        )
    };
    Unwind::Exception(VmException::new("UnicodeDecodeError", message))
}

/// Apply the error handler for the undecodable bytes `bytes[start..end]`.
fn handle_decode_error(
    codec: Codec,
    bytes: &[u8],
    start: usize,
    end: usize,
    reason: &str,
    errors: &str,
    out: &mut String,
) -> Result<(), Unwind> {
    match handler(errors)? {
        Handler::Strict => Err(decode_error(codec, bytes, start, end, reason)),
        Handler::Ignore => Ok(()),
        Handler::Replace => {
            out.push('\u{fffd}');
            Ok(())
        }
        Handler::BackslashReplace => {
            for b in &bytes[start..end] {
                out.push_str(&format!("\\x{b:02x}"));
            }
            Ok(())
        }
        // Not a decoding handler in CPython either ("don't know how to
        // handle UnicodeDecodeError in error callback").
        Handler::XmlCharRefReplace => Err(Unwind::Exception(VmException::new(
            "TypeError",
            "don't know how to handle UnicodeDecodeError in error callback",
        ))),
    }
}

/// CPython's UTF-8 decoder error classification: for the sequence starting
/// at `i`, either the decoded char and its length, or the error span and
/// reason (`invalid start byte`, `invalid continuation byte`, `unexpected
/// end of data`).
fn utf8_step(b: &[u8], i: usize) -> Result<(char, usize), (usize, &'static str)> {
    let c = b[i];
    let (need, lo, hi): (usize, u8, u8) = match c {
        0xc2..=0xdf => (1, 0x80, 0xbf),
        0xe0 => (2, 0xa0, 0xbf),
        0xe1..=0xec | 0xee..=0xef => (2, 0x80, 0xbf),
        0xed => (2, 0x80, 0x9f),
        0xf0 => (3, 0x90, 0xbf),
        0xf1..=0xf3 => (3, 0x80, 0xbf),
        0xf4 => (3, 0x80, 0x8f),
        _ => return Err((1, "invalid start byte")),
    };
    let mut cp: u32 = (c as u32) & (0x7f >> (need + 1));
    for k in 1..=need {
        let Some(&next) = b.get(i + k) else {
            return Err((k, "unexpected end of data"));
        };
        let (l, h) = if k == 1 { (lo, hi) } else { (0x80, 0xbf) };
        if next < l || next > h {
            return Err((k, "invalid continuation byte"));
        }
        cp = (cp << 6) | (next as u32 & 0x3f);
    }
    Ok((char::from_u32(cp).unwrap_or('\u{fffd}'), need + 1))
}

/// `bytes.decode(encoding, errors)`.
pub fn decode(bytes: &[u8], encoding: &str, errors: &str) -> Result<String, Unwind> {
    let codec = lookup(encoding)?;
    match codec {
        Codec::Utf8 | Codec::Utf8Sig => {
            let data = if codec == Codec::Utf8Sig && bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
                &bytes[3..]
            } else {
                bytes
            };
            // Fast path: valid UTF-8 needs no per-byte walk.
            if let Ok(s) = std::str::from_utf8(data) {
                return Ok(s.to_owned());
            }
            let mut out = String::with_capacity(data.len());
            let mut i = 0;
            while i < data.len() {
                if data[i] < 0x80 {
                    out.push(data[i] as char);
                    i += 1;
                    continue;
                }
                match utf8_step(data, i) {
                    Ok((c, len)) => {
                        out.push(c);
                        i += len;
                    }
                    Err((len, reason)) => {
                        handle_decode_error(codec, data, i, i + len, reason, errors, &mut out)?;
                        i += len;
                    }
                }
            }
            Ok(out)
        }
        Codec::Ascii => {
            let mut out = String::with_capacity(bytes.len());
            for (i, &b) in bytes.iter().enumerate() {
                if b < 0x80 {
                    out.push(b as char);
                } else {
                    handle_decode_error(
                        codec,
                        bytes,
                        i,
                        i + 1,
                        "ordinal not in range(128)",
                        errors,
                        &mut out,
                    )?;
                }
            }
            Ok(out)
        }
        Codec::Latin1 => Ok(bytes.iter().map(|&b| b as char).collect()),
        Codec::Cp1252 => {
            let mut out = String::with_capacity(bytes.len());
            for (i, &b) in bytes.iter().enumerate() {
                let mapped = if (0x80..0xa0).contains(&b) {
                    CP1252_HIGH[(b - 0x80) as usize]
                } else {
                    b as char
                };
                if mapped == '\0' && b != 0 {
                    handle_decode_error(
                        codec,
                        bytes,
                        i,
                        i + 1,
                        "character maps to <undefined>",
                        errors,
                        &mut out,
                    )?;
                } else {
                    out.push(mapped);
                }
            }
            Ok(out)
        }
        Codec::Utf16 | Codec::Utf16Le | Codec::Utf16Be => {
            let (data, big, label_codec) = match codec {
                Codec::Utf16 => {
                    if bytes.starts_with(&[0xfe, 0xff]) {
                        (&bytes[2..], true, Codec::Utf16Be)
                    } else if bytes.starts_with(&[0xff, 0xfe]) {
                        (&bytes[2..], false, Codec::Utf16Le)
                    } else {
                        (bytes, false, Codec::Utf16Le)
                    }
                }
                Codec::Utf16Be => (bytes, true, Codec::Utf16Be),
                _ => (bytes, false, Codec::Utf16Le),
            };
            let offset = bytes.len() - data.len();
            let mut units: Vec<u16> = Vec::with_capacity(data.len() / 2);
            let mut out = String::new();
            let mut i = 0;
            while i + 1 < data.len() {
                let pair = [data[i], data[i + 1]];
                units.push(if big {
                    u16::from_be_bytes(pair)
                } else {
                    u16::from_le_bytes(pair)
                });
                i += 2;
            }
            // Decoded unit by unit rather than through `char::decode_utf16`
            // so a lone surrogate carries its byte span: the error handler
            // needs it, and `ignore` / `replace` / `backslashreplace` were
            // raising instead of applying.
            let mut u = 0usize;
            while u < units.len() {
                let unit = units[u];
                let high = (0xD800..0xDC00).contains(&unit);
                let low = (0xDC00..0xE000).contains(&unit);
                if high && u + 1 < units.len() && (0xDC00..0xE000).contains(&units[u + 1]) {
                    let scalar = 0x1_0000
                        + ((u32::from(unit) - 0xD800) << 10)
                        + (u32::from(units[u + 1]) - 0xDC00);
                    if let Some(c) = char::from_u32(scalar) {
                        out.push(c);
                    }
                    u += 2;
                    continue;
                }
                if !high && !low {
                    if let Some(c) = char::from_u32(u32::from(unit)) {
                        out.push(c);
                    }
                    u += 1;
                    continue;
                }
                let start = offset + u * 2;
                handle_decode_error(
                    label_codec,
                    bytes,
                    start,
                    start + 2,
                    "illegal UTF-16 surrogate",
                    errors,
                    &mut out,
                )?;
                u += 1;
            }
            if i < data.len() {
                handle_decode_error(
                    label_codec,
                    bytes,
                    offset + i,
                    offset + i + 1,
                    "truncated data",
                    errors,
                    &mut out,
                )?;
            }
            Ok(out)
        }
        Codec::Utf32 | Codec::Utf32Le | Codec::Utf32Be => {
            let (data, big, label_codec) = match codec {
                Codec::Utf32 => {
                    if bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
                        (&bytes[4..], true, Codec::Utf32Be)
                    } else if bytes.starts_with(&[0xff, 0xfe, 0, 0]) {
                        (&bytes[4..], false, Codec::Utf32Le)
                    } else {
                        (bytes, false, Codec::Utf32Le)
                    }
                }
                Codec::Utf32Be => (bytes, true, Codec::Utf32Be),
                _ => (bytes, false, Codec::Utf32Le),
            };
            let offset = bytes.len() - data.len();
            let mut out = String::new();
            let mut i = 0;
            while i + 3 < data.len() {
                let quad = [data[i], data[i + 1], data[i + 2], data[i + 3]];
                let cp = if big {
                    u32::from_be_bytes(quad)
                } else {
                    u32::from_le_bytes(quad)
                };
                match char::from_u32(cp) {
                    Some(c) => out.push(c),
                    None => {
                        handle_decode_error(
                            label_codec,
                            bytes,
                            offset + i,
                            offset + i + 4,
                            "code point not in range(0x110000)",
                            errors,
                            &mut out,
                        )?;
                    }
                }
                i += 4;
            }
            if i < data.len() {
                handle_decode_error(
                    label_codec,
                    bytes,
                    offset + i,
                    offset + data.len(),
                    "truncated data",
                    errors,
                    &mut out,
                )?;
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(r: Result<impl std::fmt::Debug, Unwind>) -> String {
        match r {
            Err(Unwind::Exception(e)) => format!("{} {}", e.kind, e.message),
            other => panic!("expected an exception, got {other:?}"),
        }
    }

    // Expected values printed by python3.13.
    #[test]
    fn encode_matches_cpython() {
        assert_eq!(encode("héllo", "utf-8", "strict").unwrap(), b"h\xc3\xa9llo");
        assert_eq!(
            err(encode("héllo", "ascii", "strict")),
            "UnicodeEncodeError 'ascii' codec can't encode character '\\xe9' in position 1: ordinal not in range(128)"
        );
        assert_eq!(encode("héllo", "ascii", "ignore").unwrap(), b"hllo");
        assert_eq!(encode("héllo", "ascii", "replace").unwrap(), b"h?llo");
        assert_eq!(
            encode("héllo", "ascii", "backslashreplace").unwrap(),
            b"h\\xe9llo"
        );
        assert_eq!(encode("héllo", "latin-1", "strict").unwrap(), b"h\xe9llo");
        assert_eq!(
            err(encode("日本", "latin-1", "strict")),
            "UnicodeEncodeError 'latin-1' codec can't encode characters in position 0-1: ordinal not in range(256)"
        );
        assert_eq!(
            encode("hi", "utf-16", "strict").unwrap(),
            b"\xff\xfeh\x00i\x00"
        );
        assert_eq!(encode("hi", "utf-16-le", "strict").unwrap(), b"h\x00i\x00");
        assert_eq!(encode("hi", "utf-16be", "strict").unwrap(), b"\x00h\x00i");
        assert_eq!(
            encode("h", "utf-32", "strict").unwrap(),
            b"\xff\xfe\x00\x00h\x00\x00\x00"
        );
        assert_eq!(
            err(encode("x", "nope", "strict")),
            "LookupError unknown encoding: nope"
        );
        assert_eq!(encode("x", "UTF8", "strict").unwrap(), b"x");
        assert_eq!(encode("y", "Utf_8", "strict").unwrap(), b"y");
        assert_eq!(
            encode("t", "utf-8-sig", "strict").unwrap(),
            b"\xef\xbb\xbft"
        );
        assert_eq!(encode("x", "utf-8", "nope").unwrap(), b"x");
    }

    #[test]
    fn decode_matches_cpython() {
        assert_eq!(
            decode(b"\xff\xfeh\x00i\x00", "utf-16", "strict").unwrap(),
            "hi"
        );
        assert_eq!(decode(b"h\xc3\xa9", "utf-8", "strict").unwrap(), "hé");
        assert_eq!(
            err(decode(b"h\xff", "utf-8", "strict")),
            "UnicodeDecodeError 'utf-8' codec can't decode byte 0xff in position 1: invalid start byte"
        );
        assert_eq!(
            err(decode(b"h\xc3", "utf-8", "strict")),
            "UnicodeDecodeError 'utf-8' codec can't decode byte 0xc3 in position 1: unexpected end of data"
        );
        assert_eq!(
            err(decode(b"\xc3\x28", "utf-8", "strict")),
            "UnicodeDecodeError 'utf-8' codec can't decode byte 0xc3 in position 0: invalid continuation byte"
        );
        assert_eq!(decode(b"h\xff", "utf-8", "ignore").unwrap(), "h");
        assert_eq!(decode(b"h\xff", "utf-8", "replace").unwrap(), "h\u{fffd}");
        assert_eq!(
            decode(b"h\xff", "utf-8", "backslashreplace").unwrap(),
            "h\\xff"
        );
        assert_eq!(decode(b"h\xe9", "latin-1", "strict").unwrap(), "hé");
        assert_eq!(
            err(decode(b"h\xe9", "ascii", "strict")),
            "UnicodeDecodeError 'ascii' codec can't decode byte 0xe9 in position 1: ordinal not in range(128)"
        );
        assert_eq!(decode(b"h\xe9", "ascii", "replace").unwrap(), "h\u{fffd}");
        assert_eq!(
            decode(b"\xef\xbb\xbfhi", "utf-8-sig", "strict").unwrap(),
            "hi"
        );
        assert_eq!(
            err(decode(b"\xe2\x82", "utf-8", "strict")),
            "UnicodeDecodeError 'utf-8' codec can't decode bytes in position 0-1: unexpected end of data"
        );
        assert_eq!(
            err(decode(b"\xf0\x9f\x98", "utf-8", "strict")),
            "UnicodeDecodeError 'utf-8' codec can't decode bytes in position 0-2: unexpected end of data"
        );
        assert_eq!(
            err(decode(b"\xed\xa0\x80", "utf-8", "strict")),
            "UnicodeDecodeError 'utf-8' codec can't decode byte 0xed in position 0: invalid continuation byte"
        );
        assert_eq!(
            decode(b"ab\xff\xfecd", "utf-8", "replace").unwrap(),
            "ab\u{fffd}\u{fffd}cd"
        );
        assert_eq!(
            err(decode(b"\xe9", "utf-8", "strict")),
            "UnicodeDecodeError 'utf-8' codec can't decode byte 0xe9 in position 0: unexpected end of data"
        );
        assert_eq!(decode(b"h\xe9", "ascii", "ignore").unwrap(), "h");
        assert_eq!(
            decode(b"h\xe9", "ascii", "backslashreplace").unwrap(),
            "h\\xe9"
        );
        assert_eq!(decode(b"h\x00i\x00", "utf-16-le", "strict").unwrap(), "hi");
        assert_eq!(decode(b"\x00h\x00i", "utf-16-be", "strict").unwrap(), "hi");
        assert_eq!(
            err(decode(b"h\x00i", "utf-16", "strict")),
            "UnicodeDecodeError 'utf-16-le' codec can't decode byte 0x69 in position 2: truncated data"
        );
    }
}
