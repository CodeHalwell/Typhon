//! `|>` in every expression position (W7-09).
//!
//! The pipe binds looser than every other expression operator: its left
//! operand is the whole expression to its left, up to the edge of the
//! expression *slot* it sits in, and its right operand is a call (or a bare
//! callable). `not 0 |> add(0)` is `add(not 0, 0)` and `1 < 2 |> f()` is
//! `f(1 < 2)`, as the statement-level pass always lowered them.
//!
//! What was missing was the slot edges. The statement pass found pipes only
//! at bracket depth 0 and in round-bracket groups, and treated a whole group
//! as one expression, so a pipe in a list, set or dict display, a
//! comprehension, a subscript, an f-string field or after a comma was a
//! `tyc::parse` error — or, inside parentheses, swallowed everything back to
//! the opening bracket (`g(a, b |> f())` lowered to `g(f(a, b))`). A slot
//! now ends where Python's grammar ends an expression:
//!
//! - a bracket (`(`, `[`, `{`) and its match;
//! - a top-level `,` (call arguments, tuple / list / set elements);
//! - a top-level `:` (dict key / value, slice bound, lambda head);
//! - a keyword-argument or parameter-default `=`;
//! - after a top-level `for`, the comprehension keywords `for`, `in`, `if`
//!   (a conditional expression *before* the first `for` stays one slot);
//! - at statement level, the `return` / `yield` / assignment / `lambda`
//!   prefix and an `if` / `elif` / `while` / `for … in` / `assert` header;
//! - in an f-string, the replacement field, up to its `!` conversion, `:`
//!   format spec or `=` debug marker.

use crate::lexmask::{ByteKind, LexMask};

use super::{apply_pipe_call, find_top_level_pipes, split_pipe_prefix};

/// Rewrite every `|>` in one logical line's code (comment removed).
/// `None` when a pipe at statement level could not be rewritten — the caller
/// then emits the line unchanged, so the parser reports at the `|>`.
pub(super) fn rewrite_pipes_in_statement(code: &str) -> Option<String> {
    let text = rewrite_groups(&rewrite_fstring_fields(code));
    if find_top_level_pipes(&text).is_empty() {
        return Some(text);
    }
    let indent_len = text
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(text.len());
    let (indent, body) = text.split_at(indent_len);
    let body = body.trim_end();
    let (prefix, rest, suffix) = split_statement(body);
    let mut out = String::with_capacity(text.len());
    out.push_str(indent);
    out.push_str(prefix);
    for (slot, sep) in split_slots(rest, SlotContext::Statement) {
        out.push_str(&rewrite_slot(slot)?);
        out.push_str(sep);
    }
    out.push_str(suffix);
    Some(out)
}

/// `(prefix, expression part, suffix)` of a statement holding a pipe.
fn split_statement(body: &str) -> (&str, &str, &str) {
    let (prefix, rest) = split_pipe_prefix(body);
    if !prefix.is_empty() {
        return (prefix, rest, "");
    }
    let header_colon = |s: &str| -> (usize, usize) {
        // The expression runs up to a trailing `:` of a compound header.
        if s.ends_with(':') {
            (s.len() - 1, s.len() - 1)
        } else {
            (s.len(), s.len())
        }
    };
    for kw in ["if ", "elif ", "while "] {
        if body.starts_with(kw) {
            let (end, suffix_at) = header_colon(body);
            return (&body[..kw.len()], &body[kw.len()..end], &body[suffix_at..]);
        }
    }
    if let Some(rest) = body.strip_prefix("assert ") {
        return (&body[..body.len() - rest.len()], rest, "");
    }
    if body.starts_with("for ") {
        if let Some(in_at) = find_keyword(body, "in", 0) {
            let iter_start = in_at + "in".len();
            let (end, suffix_at) = header_colon(body);
            if iter_start <= end {
                return (
                    &body[..iter_start],
                    &body[iter_start..end],
                    &body[suffix_at..],
                );
            }
        }
    }
    ("", body, "")
}

/// Rewrite the pipes of one slot. A slot without a top-level pipe comes back
/// unchanged; `None` when a pipe's right operand is not a call.
fn rewrite_slot(slot: &str) -> Option<String> {
    let pipes = find_top_level_pipes(slot);
    if pipes.is_empty() {
        return Some(slot.to_owned());
    }
    let lead_len = slot.len() - slot.trim_start().len();
    let trail_start = slot.trim_end().len();
    let mut segments = Vec::with_capacity(pipes.len() + 1);
    let mut last = lead_len;
    for &p in &pipes {
        segments.push(&slot[last..p]);
        last = p + 2;
    }
    segments.push(&slot[last..trail_start]);
    let mut acc = segments[0].trim().to_owned();
    if acc.is_empty() {
        return None;
    }
    for rhs in &segments[1..] {
        acc = apply_pipe_call(&acc, rhs.trim())?;
    }
    Some(format!(
        "{}{}{}",
        &slot[..lead_len],
        acc,
        &slot[trail_start..]
    ))
}

/// Rewrite every bracket group in `text`, innermost first, slot by slot.
fn rewrite_groups(text: &str) -> String {
    if !text.contains("|>") {
        return text.to_owned();
    }
    let code = code_mask(text);
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut copied = 0;
    while i < bytes.len() {
        if code[i] && matches!(bytes[i], b'(' | b'[' | b'{') {
            if let Some(close) = matching_close(bytes, &code, i) {
                out.push_str(&text[copied..=i]);
                let inner = rewrite_groups(&text[i + 1..close]);
                for (slot, sep) in split_slots(&inner, SlotContext::Group) {
                    out.push_str(&rewrite_slot(slot).unwrap_or_else(|| slot.to_owned()));
                    out.push_str(sep);
                }
                out.push(bytes[close] as char);
                i = close + 1;
                copied = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&text[copied..]);
    out
}

/// Byte index of the bracket closing the one at `open`, counting only
/// structural-code brackets.
fn matching_close(bytes: &[u8], code: &[bool], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (k, &b) in bytes.iter().enumerate().skip(open) {
        if !code[k] {
            continue;
        }
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotContext {
    /// The expression part of a statement: only commas separate slots.
    Statement,
    /// The inside of a bracket group.
    Group,
}

/// Split `text` into `(slot, separator)` pairs at its top-level slot edges.
/// Concatenating every slot and separator gives `text` back.
fn split_slots(text: &str, ctx: SlotContext) -> Vec<(&str, &str)> {
    let code = code_mask(text);
    let bytes = text.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    let mut in_comprehension = false;
    // After a comprehension `for`, the next top-level `in` ends its target.
    let mut awaiting_in = false;
    let mut i = 0;
    while i < bytes.len() {
        if !code[i] {
            i += 1;
            continue;
        }
        let b = bytes[i];
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        if depth != 0 || matches!(b, b'(' | b'[' | b'{') {
            i += 1;
            continue;
        }
        let prev = i.checked_sub(1).map(|k| bytes[k]);
        let next = bytes.get(i + 1).copied();
        let sep_len = match b {
            b',' => 1,
            b':' if ctx == SlotContext::Group && next != Some(b'=') => 1,
            b'=' if ctx == SlotContext::Group
                && next != Some(b'=')
                && !matches!(prev, Some(b'=' | b'!' | b'<' | b'>' | b':')) =>
            {
                1
            }
            _ if ctx == SlotContext::Group && is_word_start(bytes, i) => {
                let word = word_at(text, i);
                let ends_slot = match word {
                    "for" => {
                        in_comprehension = true;
                        awaiting_in = true;
                        true
                    }
                    "in" if in_comprehension && awaiting_in => {
                        awaiting_in = false;
                        true
                    }
                    "if" | "async" if in_comprehension => true,
                    _ => false,
                };
                if ends_slot {
                    word.len()
                } else {
                    // Skip the rest of the identifier.
                    i += word.len().max(1);
                    continue;
                }
            }
            _ => 0,
        };
        if sep_len > 0 {
            parts.push((&text[start..i], &text[i..i + sep_len]));
            i += sep_len;
            start = i;
        } else {
            i += 1;
        }
    }
    parts.push((&text[start..], ""));
    parts
}

/// Whether byte `i` starts an identifier-like word (the previous byte is not
/// part of one).
fn is_word_start(bytes: &[u8], i: usize) -> bool {
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    is_ident(bytes[i]) && !i.checked_sub(1).is_some_and(|k| is_ident(bytes[k]))
}

/// The identifier-like word starting at byte `i`.
fn word_at(text: &str, i: usize) -> &str {
    let rest = &text[i..];
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Offset of keyword `kw` as a whole word in structural code at bracket
/// depth 0, at or after `from`.
fn find_keyword(text: &str, kw: &str, from: usize) -> Option<usize> {
    let code = code_mask(text);
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    for i in 0..bytes.len() {
        if !code[i] {
            continue;
        }
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        if i >= from && depth == 0 && is_word_start(bytes, i) && word_at(text, i) == kw {
            return Some(i);
        }
    }
    None
}

/// `true` for every byte of `s` that is structural code (not string text, an
/// f-string field or a comment).
fn code_mask(s: &str) -> Vec<bool> {
    let mask = LexMask::new(s);
    (0..s.len())
        .map(|i| matches!(mask.kind(i), ByteKind::Code))
        .collect()
}

// ── f-string replacement fields ──────────────────────────────────────────────

/// Rewrite pipes inside the replacement fields of every f-string in `code`.
fn rewrite_fstring_fields(code: &str) -> String {
    if !code.contains("|>") {
        return code.to_owned();
    }
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len());
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'#' {
            break;
        }
        if b == b'\'' || b == b'"' {
            let prefix = string_prefix(code, i);
            let Some(lit) = Literal::open(code, i) else {
                break;
            };
            if !prefix.to_ascii_lowercase().contains('f') {
                i = lit.end(code).unwrap_or(bytes.len());
                continue;
            }
            // An f-string: rewrite each field's expression in place.
            let mut k = lit.body_start;
            while k < bytes.len() {
                if lit.closes_at(bytes, k) {
                    k += lit.quote_len;
                    break;
                }
                match bytes[k] {
                    b'\\' if !lit.raw => k += 2,
                    b'{' if bytes.get(k + 1) == Some(&b'{') => k += 2,
                    b'{' => {
                        let Some(field_end) = field_close(code, k + 1) else {
                            return out + &code[copied..];
                        };
                        let field = &code[k + 1..field_end];
                        let expr_len = field_expression_len(field);
                        let expr = &field[..expr_len];
                        if expr.contains("|>") {
                            let rewritten = rewrite_expression(expr);
                            out.push_str(&code[copied..=k]);
                            out.push_str(&rewritten);
                            copied = k + 1 + expr_len;
                        }
                        k = field_end + 1;
                    }
                    _ => k += 1,
                }
            }
            i = k;
            continue;
        }
        i += 1;
    }
    out.push_str(&code[copied..]);
    out
}

/// One expression (an f-string field) rewritten as a single slot.
fn rewrite_expression(expr: &str) -> String {
    let inner = rewrite_groups(&rewrite_fstring_fields(expr));
    rewrite_slot(&inner).unwrap_or(inner)
}

/// The string prefix letters (`f`, `rb`, …) immediately before the quote at
/// `quote_at`, or `""`.
fn string_prefix(code: &str, quote_at: usize) -> &str {
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
struct Literal {
    quote: u8,
    quote_len: usize,
    body_start: usize,
    raw: bool,
}

impl Literal {
    fn open(code: &str, quote_at: usize) -> Option<Literal> {
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

    fn closes_at(&self, bytes: &[u8], k: usize) -> bool {
        bytes
            .get(k..k + self.quote_len)
            .is_some_and(|w| w.iter().all(|&c| c == self.quote))
    }

    /// One past the closing quote, skipping escapes (and, for an f-string
    /// nested in a field, its own fields' brackets do not matter: a field
    /// cannot contain this literal's quote unescaped before 3.12, and from
    /// 3.12 it is itself a string we skip whole).
    fn end(&self, code: &str) -> Option<usize> {
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
fn field_close(code: &str, start: usize) -> Option<usize> {
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
fn field_expression_len(field: &str) -> usize {
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
