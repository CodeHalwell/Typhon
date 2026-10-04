//! Replacement fields of f-strings, found by scanning text: the `|>`
//! rewrite (W7-09) and the `?` lift on a continuation line of a
//! triple-quoted f-string (W7-12) both need each field's expression.

use std::ops::Range;

/// Byte ranges of the field expressions in the part of `line` that lies
/// inside a triple-quoted f-string the line *starts inside* (a continuation
/// line; `quote` is the string's quote character), up to where the string
/// closes. A field that does not close on this line ends the scan.
pub(super) fn continued_fstring_field_exprs(line: &str, quote: u8) -> Vec<Range<usize>> {
    let lit = Literal {
        quote,
        quote_len: 3,
        body_start: 0,
        raw: false,
    };
    let bytes = line.as_bytes();
    let mut fields = Vec::new();
    let mut k = 0;
    while k < bytes.len() {
        if lit.closes_at(bytes, k) {
            break;
        }
        match bytes[k] {
            b'\\' => k += 2,
            b'{' if bytes.get(k + 1) == Some(&b'{') => k += 2,
            b'{' => {
                let Some(close) = field_close(line, k + 1) else {
                    break;
                };
                let len = field_expression_len(&line[k + 1..close]);
                fields.push(k + 1..k + 1 + len);
                k = close + 1;
            }
            _ => k += 1,
        }
    }
    fields
}

/// The string prefix letters (`f`, `rb`, …) immediately before the quote at
/// `quote_at`, or `""`.
pub(super) fn string_prefix(code: &str, quote_at: usize) -> &str {
    let head = &code[..quote_at];
    let start = head
        .rfind(|c: char| !c.is_ascii_alphabetic())
        .map_or(0, |p| p + 1);
    let letters = &head[start..];
    if letters.len() <= 3
        && letters
            .chars()
            .all(|c| matches!(c.to_ascii_lowercase(), 'r' | 'b' | 'f' | 'u' | 't'))
    {
        letters
    } else {
        ""
    }
}

/// An open string literal: where its body starts, and how it closes.
pub(super) struct Literal {
    pub(super) quote: u8,
    pub(super) quote_len: usize,
    pub(super) body_start: usize,
    pub(super) raw: bool,
}

impl Literal {
    pub(super) fn open(code: &str, quote_at: usize) -> Option<Literal> {
        let bytes = code.as_bytes();
        let quote = *bytes.get(quote_at)?;
        let triple = bytes.get(quote_at..quote_at + 3) == Some(&[quote, quote, quote][..]);
        let quote_len = if triple { 3 } else { 1 };
        let raw = string_prefix(code, quote_at)
            .to_ascii_lowercase()
            .contains('r');
        Some(Literal {
            quote,
            quote_len,
            body_start: quote_at + quote_len,
            raw,
        })
    }

    pub(super) fn closes_at(&self, bytes: &[u8], k: usize) -> bool {
        bytes
            .get(k..k + self.quote_len)
            .is_some_and(|w| w.iter().all(|&c| c == self.quote))
    }

    /// One past the closing quote, skipping escapes (and, for an f-string
    /// nested in a field, its own fields' brackets do not matter: a field
    /// cannot contain this literal's quote unescaped before 3.12, and from
    /// 3.12 it is itself a string we skip whole).
    pub(super) fn end(&self, code: &str) -> Option<usize> {
        let bytes = code.as_bytes();
        let mut k = self.body_start;
        while k < bytes.len() {
            if self.closes_at(bytes, k) {
                return Some(k + self.quote_len);
            }
            if bytes[k] == b'\\' && !self.raw {
                k += 2;
            } else {
                k += 1;
            }
        }
        None
    }
}

/// Byte index of the `}` closing a replacement field whose expression starts
/// at `start`, skipping brackets and nested string literals.
pub(super) fn field_close(code: &str, start: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut depth = 0i32;
    let mut k = start;
    while k < bytes.len() {
        match bytes[k] {
            b'\'' | b'"' => {
                k = Literal::open(code, k)?.end(code)?;
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' => depth -= 1,
            b'}' if depth == 0 => return Some(k),
            b'}' => depth -= 1,
            _ => {}
        }
        k += 1;
    }
    None
}

/// Length of the expression at the head of a replacement field: up to a
/// top-level `!` conversion, `:` format spec or trailing `=` debug marker.
pub(super) fn field_expression_len(field: &str) -> usize {
    let bytes = field.as_bytes();
    let mut depth = 0i32;
    let mut k = 0;
    let mut end = field.len();
    while k < bytes.len() {
        match bytes[k] {
            b'\'' | b'"' => {
                match Literal::open(field, k).and_then(|l| l.end(field)) {
                    Some(e) => k = e,
                    None => break,
                }
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'!' if depth == 0 && bytes.get(k + 1) != Some(&b'=') => {
                end = k;
                break;
            }
            b':' if depth == 0 => {
                end = k;
                break;
            }
            _ => {}
        }
        k += 1;
    }
    // `{expr=}` / `{expr = }`: the debug marker is not part of the expression.
    let head = field[..end].trim_end();
    if let Some(stripped) = head.strip_suffix('=') {
        if !stripped.ends_with(['=', '!', '<', '>']) {
            return stripped.len();
        }
    }
    end
}
