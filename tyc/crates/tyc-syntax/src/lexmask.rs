//! The one lexical mask the line-oriented preprocessor passes share.
//!
//! `preprocess.rs` is a text rewriter: almost every pass walks the buffer a
//! physical line at a time and has to answer the same three questions before
//! it may touch anything — *is this byte inside a string literal or a
//! comment?*, *how deep are we in brackets here?*, and *does a new logical
//! line start on this physical line?*. Historically each pass answered them
//! with its own hand-rolled state machine, and the copies drifted: some
//! forgot to advance the string state on an early `continue`, some tested a
//! block boundary before consulting it, and some never tracked strings at
//! all. Every one of those drifts showed up as a rewrite fired *inside* a
//! string literal — a silent mutation of a program constant that survives
//! `tyc check`, `tyc build`, and the VM/CPython cross-check alike, because
//! the corruption happens upstream of the AST that both consume.
//!
//! This module holds a single scanner, [`scan_line_core`], and two views of
//! it:
//!
//! * [`scan_line`] — the per-line view. Threads an
//!   `Option<StringMode>` across physical lines and reports where the code
//!   portion of the line ends plus its net bracket delta. The line-level
//!   helpers in `preprocess.rs` (`scan_line_code_end`,
//!   `scan_line_delta_and_code_end`, `bracket_delta_outside_strings`) are
//!   thin wrappers over it, so all ~30 of their call sites now share one
//!   state machine.
//! * [`LexMask`] — the whole-buffer view. Computed once, it answers the
//!   byte-level questions ([`LexMask::is_code`],
//!   [`LexMask::is_structural_code`], [`LexMask::bracket_depth`]) and the
//!   line-level ones ([`LexMask::line_starts_in_string`],
//!   [`LexMask::line_entry_depth`], [`LexMask::is_logical_line_start`],
//!   [`LexMask::line_code_end`]).
//!
//! ## Scanning rules
//!
//! * Single- and double-quoted strings cannot span a physical line: an
//!   unterminated one is reset at end-of-line (it is a Python syntax error
//!   anyway, and resetting stops one bad line from swallowing the rest of
//!   the file). Triple-quoted strings persist across lines.
//! * A backslash escapes the next character inside **every** string mode.
//!   Raw-string prefixes are not modelled — the escape is honoured either
//!   way, which matches what the majority of the previous copies did.
//! * A `#` outside any string starts a comment that runs to the end of the
//!   physical line. A quote inside that comment is inert: the scan is
//!   deliberately *one* pass, because computing a string mask first and a
//!   comment mask second lets an apostrophe in prose (`# each field's
//!   shape`) open a phantom string.
//! * f-string replacement fields (`f"…{HERE}…"`) are lexically code, not
//!   string text: nested string literals inside them open and close
//!   normally, `{{` / `}}` are literal braces, and a `{` nested inside a
//!   field (a dict display, or a `:{width}` format spec) does not close it.
//!   [`ByteKind::FStringExpr`] keeps them distinguishable from top-level
//!   code for the passes that must not rewrite inside a literal at all.

/// Active string-literal mode while scanning. `Single` and `Double` cannot
/// span a newline (per Python's grammar) and are reset at end-of-line;
/// triple-quoted forms persist across lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StringMode {
    Single,
    Double,
    TripleSingle,
    TripleDouble,
}

impl StringMode {
    fn parts(self) -> (u8, bool) {
        match self {
            StringMode::Single => (b'\'', false),
            StringMode::Double => (b'"', false),
            StringMode::TripleSingle => (b'\'', true),
            StringMode::TripleDouble => (b'"', true),
        }
    }

    fn open(quote: u8, triple: bool) -> StringMode {
        match (quote == b'\'', triple) {
            (true, false) => StringMode::Single,
            (false, false) => StringMode::Double,
            (true, true) => StringMode::TripleSingle,
            (false, true) => StringMode::TripleDouble,
        }
    }

    fn is_triple(self) -> bool {
        matches!(self, StringMode::TripleSingle | StringMode::TripleDouble)
    }
}

/// What one source byte lexically is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ByteKind {
    /// Structural code — the only place a rewrite may safely fire.
    Code,
    /// Inside a string literal (opening / closing quotes included).
    StringText,
    /// Inside an f-string replacement field. Lexically code, but nested
    /// inside a literal, so a pass that lifts or splices whole statements
    /// must leave it alone even though an identifier rename may not.
    FStringExpr,
    /// Inside a `#` comment, the `#` included. A line terminator is never
    /// part of the comment.
    Comment,
}

/// Result of scanning one physical line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LineScan {
    /// Byte index where the code portion of the line ends — the offset of a
    /// trailing `#` comment, or the length of the input when there is none.
    pub(crate) code_end: usize,
    /// Net bracket delta contributed by the code portion of the line.
    pub(crate) bracket_delta: i32,
}

/// One string frame. f-strings push a frame per nested literal so a quote
/// inside a replacement field opens an ordinary string without ending the
/// enclosing f-string.
#[derive(Clone, Copy)]
struct Frame {
    mode: StringMode,
    is_fstring: bool,
    /// `true` once inside `{ … }`. While set, bytes are scanned as code.
    in_field: bool,
    /// Nesting of `{` *within* an open replacement field (dict displays,
    /// nested format specs), so the first inner `}` doesn't close the field.
    field_braces: u32,
    /// `true` once past the field's top-level `:` — the format-spec portion,
    /// where a quote is a literal spec character (the fill char in
    /// `f"{v:'>8}"`), not a string opener, and a bracket is spec text, not
    /// structure. Only a nested `{…}` field inside the spec is code again.
    in_spec: bool,
    /// Bracket depth at field entry. The spec-introducing `:` is only the
    /// one at this depth with no `{` nesting, so the slice colon in
    /// `f"{a[1:2]}"` does not start a spec.
    field_entry_depth: i32,
}

/// Length in bytes of the UTF-8 character starting at `bytes[i]`.
fn char_len_at(bytes: &[u8], i: usize) -> usize {
    let b = bytes[i];
    // `b < 0xC0` covers both ASCII and a stray continuation byte; either way
    // one byte is the right step (the latter only to make progress).
    let n = if b < 0xC0 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    };
    n.min(bytes.len() - i)
}

/// `true` when the quote byte at `bytes[i]` carries a string prefix
/// containing `f` / `F` (and not `b` / `B`, which makes it a bytes literal).
fn has_fstring_prefix(bytes: &[u8], quote_idx: usize) -> bool {
    let mut j = quote_idx;
    let mut has_f = false;
    let mut has_b = false;
    while j > 0 {
        match bytes[j - 1] {
            b'f' | b'F' => {
                has_f = true;
                j -= 1;
            }
            b'r' | b'R' | b'u' | b'U' => j -= 1,
            b'b' | b'B' => {
                has_b = true;
                j -= 1;
            }
            _ => break,
        }
    }
    has_f && !has_b
}

/// The single string / comment / bracket state machine.
///
/// Scans one physical line (with or without its trailing newline), threading
/// triple-quoted-string state through `in_string` and bracket depth through
/// `depth`. `emit` is called once per byte with `(index, kind, depth_before)`
/// so a caller that wants a byte mask can build one without a second scan;
/// callers that only need the summary pass a no-op.
fn scan_line_core<F>(
    line: &str,
    in_string: &mut Option<StringMode>,
    depth: &mut i32,
    mut emit: F,
) -> LineScan
where
    F: FnMut(usize, ByteKind, i32),
{
    let bytes = line.as_bytes();
    let start_depth = *depth;
    let mut code_end = line.len();

    // Frame stack. Seeded from the carried-over `in_string` so a triple-quoted
    // string opened on an earlier line resumes correctly. A carried-over
    // string is never mid-replacement-field: a `{` may not span lines in the
    // forms this preprocessor handles.
    let mut stack: Vec<Frame> = Vec::new();
    if let Some(mode) = *in_string {
        stack.push(Frame {
            mode,
            is_fstring: false,
            in_field: false,
            field_braces: 0,
            in_spec: false,
            field_entry_depth: 0,
        });
    }

    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];

        if let Some(top) = stack.last().copied() {
            if !top.in_field {
                // ── Literal portion of a string ──────────────────────────
                emit(i, ByteKind::StringText, *depth);
                if b == b'\\' && i + 1 < bytes.len() {
                    let n = char_len_at(bytes, i + 1);
                    for k in 0..n {
                        emit(i + 1 + k, ByteKind::StringText, *depth);
                    }
                    i += 1 + n;
                    continue;
                }
                let (q, triple) = top.mode.parts();
                if b == q {
                    if triple {
                        if i + 2 < bytes.len() && bytes[i + 1] == q && bytes[i + 2] == q {
                            emit(i + 1, ByteKind::StringText, *depth);
                            emit(i + 2, ByteKind::StringText, *depth);
                            stack.pop();
                            i += 3;
                            continue;
                        }
                    } else {
                        stack.pop();
                        i += 1;
                        continue;
                    }
                }
                if top.is_fstring && b == b'{' {
                    if bytes.get(i + 1) == Some(&b'{') {
                        // `{{` is a literal brace.
                        emit(i + 1, ByteKind::StringText, *depth);
                        i += 2;
                        continue;
                    }
                    if let Some(last) = stack.last_mut() {
                        last.in_field = true;
                        last.field_braces = 0;
                        last.in_spec = false;
                        last.field_entry_depth = *depth;
                    }
                    i += 1;
                    continue;
                }
                if top.is_fstring && b == b'}' && bytes.get(i + 1) == Some(&b'}') {
                    emit(i + 1, ByteKind::StringText, *depth);
                    i += 2;
                    continue;
                }
                i += char_len_at(bytes, i);
                continue;
            }

            // ── Inside an f-string replacement field: bytes are code ─────
            // …except in the format-spec portion (after the field's
            // top-level `:`), where everything but a nested `{…}` field is
            // literal spec text: `f"{v:'>8}"` pads with a quote and
            // `f"{v:(>8}"` with a paren — neither opens a string or a
            // bracket. Pushing a frame for such a quote opened a *phantom*
            // string that masked the rest of the line (and, triple-shaped,
            // the following lines), silently dropping later rewrites.
            if top.in_spec && top.field_braces == 0 && b != b'{' && b != b'}' {
                let n = char_len_at(bytes, i);
                for k in 0..n {
                    emit(i + k, ByteKind::FStringExpr, *depth);
                }
                i += n;
                continue;
            }
            match b {
                b'}' if top.field_braces == 0 => {
                    emit(i, ByteKind::StringText, *depth);
                    if let Some(last) = stack.last_mut() {
                        last.in_field = false;
                        last.in_spec = false;
                    }
                    i += 1;
                    continue;
                }
                b'}' => {
                    *depth -= 1;
                    emit(i, ByteKind::FStringExpr, *depth);
                    if let Some(last) = stack.last_mut() {
                        last.field_braces -= 1;
                    }
                    i += 1;
                    continue;
                }
                b'{' => {
                    emit(i, ByteKind::FStringExpr, *depth);
                    *depth += 1;
                    if let Some(last) = stack.last_mut() {
                        last.field_braces += 1;
                    }
                    i += 1;
                    continue;
                }
                b':' if top.field_braces == 0 && *depth == top.field_entry_depth => {
                    // The field's top-level `:` starts the format spec. A
                    // colon nested in brackets (`f"{a[1:2]}"`) or braces
                    // (`f"{ {'k': 1} }"`) is expression code, not a spec.
                    emit(i, ByteKind::FStringExpr, *depth);
                    if let Some(last) = stack.last_mut() {
                        last.in_spec = true;
                    }
                    i += 1;
                    continue;
                }
                b'"' | b'\'' => {
                    let triple = i + 2 < bytes.len() && bytes[i + 1] == b && bytes[i + 2] == b;
                    emit(i, ByteKind::StringText, *depth);
                    if triple {
                        emit(i + 1, ByteKind::StringText, *depth);
                        emit(i + 2, ByteKind::StringText, *depth);
                    }
                    stack.push(Frame {
                        mode: StringMode::open(b, triple),
                        is_fstring: has_fstring_prefix(bytes, i),
                        in_field: false,
                        field_braces: 0,
                        in_spec: false,
                        field_entry_depth: 0,
                    });
                    i += if triple { 3 } else { 1 };
                    continue;
                }
                b'(' | b'[' => {
                    emit(i, ByteKind::FStringExpr, *depth);
                    *depth += 1;
                    i += 1;
                    continue;
                }
                b')' | b']' => {
                    *depth -= 1;
                    emit(i, ByteKind::FStringExpr, *depth);
                    i += 1;
                    continue;
                }
                _ => {
                    // `#` is *not* a comment inside a replacement field —
                    // Python forbids comments there, and treating it as one
                    // would desynchronise the string state for the rest of
                    // the line.
                    let n = char_len_at(bytes, i);
                    for k in 0..n {
                        emit(i + k, ByteKind::FStringExpr, *depth);
                    }
                    i += n;
                    continue;
                }
            }
        }

        // ── Outside every string ───────────────────────────────────────
        if b == b'#' {
            code_end = i;
            // The comment runs to the end of the physical line; the line
            // terminator itself stays structural so a whole-buffer mask can
            // still use it as a line boundary.
            let mut j = i;
            while j < bytes.len() && bytes[j] != b'\n' && bytes[j] != b'\r' {
                emit(j, ByteKind::Comment, *depth);
                j += 1;
            }
            while j < bytes.len() {
                emit(j, ByteKind::Code, *depth);
                j += 1;
            }
            break;
        }

        if b == b'"' || b == b'\'' {
            let triple = i + 2 < bytes.len() && bytes[i + 1] == b && bytes[i + 2] == b;
            emit(i, ByteKind::StringText, *depth);
            if triple {
                emit(i + 1, ByteKind::StringText, *depth);
                emit(i + 2, ByteKind::StringText, *depth);
            }
            stack.push(Frame {
                mode: StringMode::open(b, triple),
                is_fstring: has_fstring_prefix(bytes, i),
                in_field: false,
                field_braces: 0,
                in_spec: false,
                field_entry_depth: 0,
            });
            i += if triple { 3 } else { 1 };
            continue;
        }

        match b {
            b'(' | b'[' | b'{' => {
                emit(i, ByteKind::Code, *depth);
                *depth += 1;
            }
            b')' | b']' | b'}' => {
                *depth -= 1;
                emit(i, ByteKind::Code, *depth);
            }
            _ => {
                let n = char_len_at(bytes, i);
                for k in 0..n {
                    emit(i + k, ByteKind::Code, *depth);
                }
                i += n;
                continue;
            }
        }
        i += 1;
    }

    // Carry only a triple-quoted string across the line boundary. An
    // unterminated single/double-quoted string is a Python syntax error;
    // resetting keeps one bad line from swallowing the rest of the file.
    *in_string = stack
        .first()
        .map(|f| f.mode)
        .filter(|mode| mode.is_triple());

    LineScan {
        code_end,
        bracket_delta: *depth - start_depth,
    }
}

/// Scan one physical line, threading triple-quoted-string state through
/// `in_string`. See [`scan_line_core`] for the rules.
pub(crate) fn scan_line(line: &str, in_string: &mut Option<StringMode>) -> LineScan {
    let mut depth = 0i32;
    scan_line_core(line, in_string, &mut depth, |_, _, _| {})
}

/// Scan one physical line and return the [`ByteKind`] of every byte, threading
/// triple-quoted-string state through `in_string` (so a string opened on an
/// earlier line resumes correctly). This is the per-byte view a line-oriented
/// rewrite pass needs to tell structural code from string / f-string-field
/// content without maintaining its own — necessarily PEP 701-incomplete —
/// scanner. Positions not written by the scan default to [`ByteKind::Code`].
pub(crate) fn scan_line_kinds(line: &str, in_string: &mut Option<StringMode>) -> Vec<ByteKind> {
    let mut depth = 0i32;
    let mut kinds = vec![ByteKind::Code; line.len()];
    scan_line_core(line, in_string, &mut depth, |i, kind, _d| {
        if i < kinds.len() {
            kinds[i] = kind;
        }
    });
    kinds
}

/// A whole-buffer lexical mask: byte kinds, bracket depths, and the derived
/// per-line facts every block-structure pass needs.
pub(crate) struct LexMask {
    kinds: Vec<ByteKind>,
    depths: Vec<i32>,
    line_start: Vec<usize>,
    line_entry_string: Vec<Option<StringMode>>,
    line_entry_depth: Vec<i32>,
    line_bracket_delta: Vec<i32>,
    line_code_end: Vec<usize>,
    line_continues: Vec<bool>,
}

impl LexMask {
    /// Scan `source` once. Lines are indexed the way the preprocessor
    /// indexes them everywhere else — `source.split_inclusive('\n')`.
    pub(crate) fn new(source: &str) -> LexMask {
        let mut kinds = vec![ByteKind::Code; source.len()];
        let mut depths = vec![0i32; source.len()];
        let mut line_start = Vec::new();
        let mut line_entry_string = Vec::new();
        let mut line_entry_depth = Vec::new();
        let mut line_bracket_delta = Vec::new();
        let mut line_code_end = Vec::new();
        let mut line_continues = Vec::new();

        let mut in_string: Option<StringMode> = None;
        let mut depth = 0i32;
        let mut base = 0usize;

        for line in source.split_inclusive('\n') {
            line_start.push(base);
            line_entry_string.push(in_string);
            line_entry_depth.push(depth);

            let scan = scan_line_core(line, &mut in_string, &mut depth, |i, kind, d| {
                kinds[base + i] = kind;
                depths[base + i] = d;
            });
            line_bracket_delta.push(scan.bracket_delta);

            // `code_end` is relative to the line and may include the line
            // terminator when there is no comment; normalise it to the end of
            // the line's *content*.
            let content_len = line.trim_end_matches(['\n', '\r']).len();
            line_code_end.push(scan.code_end.min(content_len));

            // A trailing backslash on the code portion continues the logical
            // line onto the next physical one.
            let code = &line[..scan.code_end.min(content_len)];
            line_continues.push(code.ends_with('\\'));

            base += line.len();
        }

        LexMask {
            kinds,
            depths,
            line_start,
            line_entry_string,
            line_entry_depth,
            line_bracket_delta,
            line_code_end,
            line_continues,
        }
    }

    /// Kind of the byte at `i`. Out-of-range indices read as code so callers
    /// scanning a derived buffer degrade to the previous, mask-free
    /// behaviour instead of panicking.
    pub(crate) fn kind(&self, i: usize) -> ByteKind {
        self.kinds.get(i).copied().unwrap_or(ByteKind::Code)
    }

    /// `true` when byte `i` is code in the sense that matters for an
    /// identifier-level rewrite: structural code, or the inside of an
    /// f-string replacement field (which is a real expression).
    pub(crate) fn is_code(&self, i: usize) -> bool {
        matches!(self.kind(i), ByteKind::Code | ByteKind::FStringExpr)
    }

    /// `true` when byte `i` is structural code — outside every string
    /// literal, including f-string replacement fields. This is the
    /// predicate for a pass that splices or lifts whole statements, which
    /// must not reach inside a literal even where the literal contains an
    /// expression.
    pub(crate) fn is_structural_code(&self, i: usize) -> bool {
        matches!(self.kind(i), ByteKind::Code)
    }

    /// Bracket depth *before* the byte at `i` (a `(` reads as the depth
    /// outside it; its matching `)` reads the same).
    pub(crate) fn bracket_depth(&self, i: usize) -> i32 {
        self.depths.get(i).copied().unwrap_or(0)
    }

    /// String state on entry to line `line`, i.e. before its first byte.
    pub(crate) fn line_entry_string(&self, line: usize) -> Option<StringMode> {
        self.line_entry_string.get(line).copied().flatten()
    }

    /// `true` when line `line` *begins* inside a triple-quoted string, so its
    /// leading whitespace is string content rather than indentation. Such a
    /// line is never a block boundary and must never be re-indented.
    pub(crate) fn line_starts_in_string(&self, line: usize) -> bool {
        self.line_entry_string(line).is_some()
    }

    /// Bracket depth on entry to line `line`.
    pub(crate) fn line_entry_depth(&self, line: usize) -> i32 {
        self.line_entry_depth.get(line).copied().unwrap_or(0)
    }

    /// Net bracket delta of line `line`.
    pub(crate) fn line_bracket_delta(&self, line: usize) -> i32 {
        self.line_bracket_delta.get(line).copied().unwrap_or(0)
    }

    /// Byte offset, relative to the start of line `line`, at which its code
    /// portion ends — the offset of a trailing `#` comment, or the length of
    /// the line's content (excluding the line terminator) when there is none.
    pub(crate) fn line_code_end(&self, line: usize) -> usize {
        self.line_code_end.get(line).copied().unwrap_or(0)
    }

    /// `true` when a new logical line starts at line `line`: it does not
    /// begin inside a string, no bracket is open across its left edge, and
    /// the previous physical line did not end in a backslash continuation.
    pub(crate) fn is_logical_line_start(&self, line: usize) -> bool {
        if line >= self.line_start.len() {
            return false;
        }
        if self.line_starts_in_string(line) {
            return false;
        }
        if self.line_entry_depth(line) > 0 {
            return false;
        }
        line == 0 || !self.line_continues[line - 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<ByteKind> {
        let mask = LexMask::new(src);
        (0..src.len()).map(|i| mask.kind(i)).collect()
    }

    #[test]
    fn plain_code_is_code() {
        let src = "let x: int = 1\n";
        assert!(kinds(src).iter().all(|k| *k == ByteKind::Code));
    }

    #[test]
    fn string_body_is_string_text() {
        let src = "x = \"a?b\"\n";
        let mask = LexMask::new(src);
        let q = src.find('"').unwrap();
        assert!(mask.is_structural_code(q - 1));
        for i in q..src.rfind('"').unwrap() + 1 {
            assert!(!mask.is_code(i), "byte {i} should be string text");
        }
    }

    #[test]
    fn quote_inside_comment_does_not_open_a_string() {
        // The v0.15.2 regression: a comment-blind string pass turned the
        // apostrophe into a string opener and swallowed the next line.
        let src = "# each field's shape\nlet x: int = 1\n";
        let mask = LexMask::new(src);
        let second = src.find("let").unwrap();
        assert!(mask.is_structural_code(second));
        assert!(!mask.line_starts_in_string(1));
    }

    #[test]
    fn hash_inside_string_is_not_a_comment() {
        let src = "x = \"a # b\"  # real\n";
        let mask = LexMask::new(src);
        assert_eq!(mask.line_code_end(0), src.find("# real").unwrap());
    }

    #[test]
    fn triple_quoted_string_spans_lines() {
        let src = "x = \"\"\"\nRED\nGREEN\n\"\"\"\ny = 1\n";
        let mask = LexMask::new(src);
        assert!(!mask.line_starts_in_string(0));
        assert!(mask.line_starts_in_string(1));
        assert!(mask.line_starts_in_string(2));
        assert!(mask.line_starts_in_string(3));
        assert!(!mask.line_starts_in_string(4));
    }

    #[test]
    fn bracket_depth_tracks_continuations() {
        let src = "f(\n  1,\n  2,\n)\ng()\n";
        let mask = LexMask::new(src);
        assert_eq!(mask.line_entry_depth(0), 0);
        assert_eq!(mask.line_entry_depth(1), 1);
        assert_eq!(mask.line_entry_depth(2), 1);
        assert_eq!(mask.line_entry_depth(3), 1);
        assert_eq!(mask.line_entry_depth(4), 0);
        assert!(mask.is_logical_line_start(0));
        assert!(!mask.is_logical_line_start(1));
        assert!(!mask.is_logical_line_start(3));
        assert!(mask.is_logical_line_start(4));
    }

    #[test]
    fn brackets_inside_strings_and_comments_do_not_count() {
        let src = "x = \"((( \"  # )))\ny = 1\n";
        let mask = LexMask::new(src);
        assert_eq!(mask.line_entry_depth(1), 0);
        assert!(mask.is_logical_line_start(1));
    }

    #[test]
    fn backslash_continuation_is_not_a_logical_line_start() {
        let src = "x = 1 + \\\n    2\ny = 3\n";
        let mask = LexMask::new(src);
        assert!(mask.is_logical_line_start(0));
        assert!(!mask.is_logical_line_start(1));
        assert!(mask.is_logical_line_start(2));
    }

    #[test]
    fn fstring_replacement_field_is_expression_code() {
        let src = "x = f\"{err.field} literal\"\n";
        let mask = LexMask::new(src);
        let e = src.find("err").unwrap();
        assert!(mask.is_code(e));
        assert!(!mask.is_structural_code(e));
        let lit = src.find("literal").unwrap();
        assert!(!mask.is_code(lit));
    }

    #[test]
    fn fstring_nested_braces_do_not_close_the_field_early() {
        let src = "x = f\"{ {'a': err} } tail\"\n";
        let mask = LexMask::new(src);
        let e = src.find("err").unwrap();
        assert!(mask.is_code(e));
        let tail = src.find("tail").unwrap();
        assert!(!mask.is_code(tail));
    }

    #[test]
    fn fstring_double_braces_are_literal() {
        let src = "x = f\"{{err}} tail\"\n";
        let mask = LexMask::new(src);
        let e = src.find("err").unwrap();
        assert!(!mask.is_code(e));
    }

    #[test]
    fn quote_fill_char_in_format_spec_is_not_a_string_opener() {
        // `f"{v:'>8}"` pads with a literal `'` (valid CPython). Treating it
        // as a string opener pushed a phantom frame that masked the rest of
        // the line, so later rewrites (`?`, `as!`) on the same line were
        // silently skipped and the emitted Python failed to parse.
        let src = "let s: str = f\"{v:'>8}\" + name()\n";
        let mask = LexMask::new(src);
        let plus = src.find('+').unwrap();
        assert!(
            mask.is_structural_code(plus),
            "code after the f-string must not be masked"
        );
        let name = src.find("name").unwrap();
        assert!(mask.is_structural_code(name));
        // The spec text itself is inside a literal, not structural code.
        let fill = src.find("'>").unwrap();
        assert!(!mask.is_structural_code(fill));
    }

    #[test]
    fn triple_quote_shaped_format_spec_does_not_open_a_phantom_string() {
        // `f"{v:'''}"` — three quote fill/spec chars must not open a
        // phantom triple-quoted string that swallows the rest of the line
        // (and the f-string's own terminator with it).
        let src = "x = f\"{v:'''}\" + tail\ny = 1\n";
        let mask = LexMask::new(src);
        let plus = src.find('+').unwrap();
        assert!(
            mask.is_structural_code(plus),
            "code after the f-string must not be masked"
        );
        assert!(!mask.line_starts_in_string(1));
        assert!(mask.is_structural_code(src.find("y = 1").unwrap()));
    }

    #[test]
    fn bracket_fill_char_in_format_spec_does_not_desync_depth() {
        // `(` as the fill char is spec text, not an opening bracket;
        // counting it would leave a bracket open across the line boundary
        // and stop the next line from reading as a logical line start.
        let src = "x = f\"{v:(>8}\"\ny = 1\n";
        let mask = LexMask::new(src);
        assert_eq!(mask.line_entry_depth(1), 0);
        assert!(mask.is_logical_line_start(1));
    }

    #[test]
    fn spec_rule_does_not_bleed_into_the_expression_portion() {
        // A quote in the *expression* portion still opens a real string,
        // and a slice colon inside brackets does not start the spec.
        let src = "x = f\"{d['a:b'][1:2]:>8}\" + tail\n";
        let mask = LexMask::new(src);
        let key = src.find("a:b").unwrap();
        assert!(!mask.is_code(key), "the subscript key is string text");
        let spec = src.find(">8").unwrap();
        assert!(!mask.is_structural_code(spec));
        let plus = src.find('+').unwrap();
        assert!(mask.is_structural_code(plus));
    }

    #[test]
    fn nested_field_inside_a_format_spec_is_still_code() {
        // `f"{v:{d['k']}>8}"` — the nested `{…}` inside the spec is a real
        // replacement field: its quotes open strings and its expression is
        // code, while the surrounding spec text stays inert.
        let src = "x = f\"{v:{d['k']}>8}\" + tail\n";
        let mask = LexMask::new(src);
        let d = src.find("d[").unwrap();
        assert!(mask.is_code(d));
        let k = src.find('k').unwrap();
        assert!(!mask.is_code(k), "the nested key is string text");
        let plus = src.find('+').unwrap();
        assert!(mask.is_structural_code(plus));
    }

    #[test]
    fn nested_quote_inside_replacement_field() {
        let src = "x = f\"{d['k']} tail\"\ny = 1\n";
        let mask = LexMask::new(src);
        assert!(!mask.line_starts_in_string(1));
        let d = src.find("d[").unwrap();
        assert!(mask.is_code(d));
        let k = src.find('k').unwrap();
        assert!(!mask.is_code(k));
    }

    #[test]
    fn unterminated_single_quote_resets_at_end_of_line() {
        let src = "x = 'oops\ny = 1\n";
        let mask = LexMask::new(src);
        assert!(!mask.line_starts_in_string(1));
    }

    #[test]
    fn escaped_quote_does_not_close_a_string() {
        let src = "x = \"a\\\"b\"  # c\ny = 1\n";
        let mask = LexMask::new(src);
        assert_eq!(mask.line_code_end(0), src.find("# c").unwrap());
        assert!(!mask.line_starts_in_string(1));
    }

    #[test]
    fn scan_line_matches_mask_for_a_line_sequence() {
        let src = "def f() -> None:\n    \"\"\"doc\n    RED\n    \"\"\"\n    return None\n";
        let mask = LexMask::new(src);
        let mut state: Option<StringMode> = None;
        for (i, line) in src.split_inclusive('\n').enumerate() {
            assert_eq!(
                state.is_some(),
                mask.line_starts_in_string(i),
                "line {i} entry state"
            );
            scan_line(line, &mut state);
        }
    }

    #[test]
    fn multibyte_content_does_not_shift_offsets() {
        let src = "x = \"em—dash\"  # naïve\ny = 1\n";
        let mask = LexMask::new(src);
        assert_eq!(mask.line_code_end(0), src.find("# na").unwrap());
        assert!(!mask.line_starts_in_string(1));
    }
}
