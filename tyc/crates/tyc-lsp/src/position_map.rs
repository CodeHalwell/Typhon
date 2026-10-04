//! Editor positions ↔ offsets into the preprocessed buffer (W4-07).
//!
//! Hover, go-to-definition and completion run the resolver over the
//! *preprocessed* buffer: the sugar chain has expanded `?`, `gather:`,
//! with-chains and friends, inserting lines, and the keyword-stripping pass
//! has shifted columns. Diagnostics were always remapped through the
//! expansion line table (`PreprocessResult::line_map`, preprocessed line →
//! original line), but the request handlers passed the editor's position
//! straight into the preprocessed text. Below the first `?` every hover named
//! the wrong symbol — a desugaring temporary, the next line's binding, or
//! nothing at all.
//!
//! [`PositionMap`] maps both ways through that same table. A line can expand
//! into several preprocessed lines, so the column is recovered textually: the
//! identifier under the cursor is located among the preprocessed lines that
//! came from the cursor's line. When the identifier occurs the same number of
//! times on both sides the occurrences are matched by order, which is exact
//! (expansion moves tokens between lines but keeps their order and never
//! invents user identifiers); otherwise the occurrence nearest the original
//! column wins.

use tower_lsp_server::ls_types::{Position, Range};

/// Bidirectional position mapping between the editor's text (`original`) and
/// the preprocessed buffer the resolver indexed.
pub(crate) struct PositionMap<'a> {
    original: &'a str,
    preprocessed: &'a str,
    /// `line_map[preprocessed_line] = original_line`; empty means identity.
    line_map: &'a [usize],
    orig_starts: Vec<usize>,
    prep_starts: Vec<usize>,
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte index within `line` of the UTF-16 column `col` (clamped to the line).
fn utf16_to_byte(line: &str, col: u32) -> usize {
    let mut units = 0u32;
    for (i, ch) in line.char_indices() {
        if units >= col {
            return i;
        }
        units += ch.len_utf16() as u32;
    }
    line.len()
}

/// UTF-16 column of byte index `byte` within `line`.
fn byte_to_utf16(line: &str, byte: usize) -> u32 {
    let byte = byte.min(line.len());
    let mut byte = byte;
    while !line.is_char_boundary(byte) {
        byte -= 1;
    }
    line[..byte].encode_utf16().count() as u32
}

/// The identifier `byte` touches in `line` — the one containing it, or the one
/// ending exactly at it (a cursor just past a name). `None` off identifiers.
fn word_at(line: &str, byte: usize) -> Option<(usize, usize)> {
    let byte = byte.min(line.len());
    if !line.is_char_boundary(byte) {
        return None;
    }
    let mut start = byte;
    for (i, ch) in line[..byte].char_indices().rev() {
        if !is_ident_char(ch) {
            break;
        }
        start = i;
    }
    let mut end = byte;
    for (i, ch) in line[byte..].char_indices() {
        if !is_ident_char(ch) {
            break;
        }
        end = byte + i + ch.len_utf8();
    }
    (start < end).then_some((start, end))
}

/// Byte columns of every whole-word occurrence of `word` in `line`.
fn occurrences(line: &str, word: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = line[from..].find(word) {
        let at = from + i;
        let end = at + word.len();
        let before_ok = line[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident_char(c));
        let after_ok = line[end..].chars().next().is_none_or(|c| !is_ident_char(c));
        if before_ok && after_ok {
            out.push(at);
        }
        from = at + word.len().max(1);
    }
    out
}

impl<'a> PositionMap<'a> {
    pub(crate) fn new(original: &'a str, preprocessed: &'a str, line_map: &'a [usize]) -> Self {
        Self {
            original,
            preprocessed,
            line_map,
            orig_starts: line_starts(original),
            prep_starts: line_starts(preprocessed),
        }
    }

    fn line(text: &'a str, starts: &[usize], idx: usize) -> &'a str {
        let Some(&start) = starts.get(idx) else {
            return "";
        };
        let end = starts.get(idx + 1).copied().unwrap_or(text.len());
        text[start..end].trim_end_matches(['\n', '\r'])
    }

    fn orig_line(&self, idx: usize) -> &'a str {
        Self::line(self.original, &self.orig_starts, idx)
    }

    fn prep_line(&self, idx: usize) -> &'a str {
        Self::line(self.preprocessed, &self.prep_starts, idx)
    }

    /// The original line preprocessed line `prep` came from.
    fn origin_of(&self, prep: usize) -> usize {
        let line = if self.line_map.is_empty() {
            prep
        } else {
            self.line_map
                .get(prep)
                .copied()
                .unwrap_or_else(|| self.line_map.last().copied().unwrap_or(0))
        };
        line.min(self.orig_starts.len().saturating_sub(1))
    }

    /// Every preprocessed line that came from original line `orig`, in order.
    fn expansions_of(&self, orig: usize) -> Vec<usize> {
        if self.line_map.is_empty() {
            return if orig < self.prep_starts.len() {
                vec![orig]
            } else {
                Vec::new()
            };
        }
        (0..self.prep_starts.len().min(self.line_map.len()))
            .filter(|&p| self.line_map[p] == orig)
            .collect()
    }

    /// Byte offset into the preprocessed buffer for the editor position `pos`.
    pub(crate) fn to_preprocessed(&self, pos: Position) -> usize {
        let orig = pos.line as usize;
        let line = self.orig_line(orig);
        let col = utf16_to_byte(line, pos.character);
        let candidates = self.expansions_of(orig);
        let Some(&first) = candidates.first() else {
            // A line the preprocessed buffer has no counterpart for (past
            // its end): the nearest earlier line, at the end.
            let prep = (0..self.prep_starts.len())
                .rev()
                .find(|&p| self.origin_of(p) <= orig)
                .unwrap_or(0);
            return self.prep_starts[prep] + self.prep_line(prep).len();
        };

        // The identifier under the cursor, located among the expansions.
        if let Some((ws, we)) = word_at(line, col) {
            let word = &line[ws..we];
            if let Some((prep, at)) = self.locate(word, ws, line, &candidates) {
                return self.prep_starts[prep] + at + (col - ws).min(word.len());
            }
        }

        // Off an identifier (after a `.`, on punctuation): match the text
        // just left of the cursor, which is what completion reads.
        let head = line[..col].trim_start();
        for len in [24usize, 12, 6, 3, 1] {
            let mut cut = head.len().saturating_sub(len);
            while !head.is_char_boundary(cut) {
                cut += 1;
            }
            let anchor = &head[cut..];
            if anchor.trim().is_empty() {
                continue;
            }
            let mut best: Option<(usize, usize, usize)> = None; // (dist, prep, end)
            for &prep in &candidates {
                let text = self.prep_line(prep);
                let mut from = 0;
                while let Some(i) = text[from..].find(anchor) {
                    let end = from + i + anchor.len();
                    let dist = end.abs_diff(col);
                    if best.is_none_or(|(d, _, _)| dist < d) {
                        best = Some((dist, prep, end));
                    }
                    from += i + anchor.len().max(1);
                }
            }
            if let Some((_, prep, end)) = best {
                return self.prep_starts[prep] + end;
            }
        }

        // Nothing to anchor on: the same column of the first expansion.
        self.prep_starts[first] + col.min(self.prep_line(first).len())
    }

    /// Find `word` (at byte `ws` of original `line`) among the `candidates`
    /// preprocessed lines: `(preprocessed line, byte column)`.
    fn locate(
        &self,
        word: &str,
        ws: usize,
        line: &str,
        candidates: &[usize],
    ) -> Option<(usize, usize)> {
        let orig_occ = occurrences(line, word);
        let ordinal = orig_occ.iter().position(|&o| o == ws)?;
        let prep_occ: Vec<(usize, usize)> = candidates
            .iter()
            .flat_map(|&p| {
                occurrences(self.prep_line(p), word)
                    .into_iter()
                    .map(move |c| (p, c))
            })
            .collect();
        if prep_occ.len() == orig_occ.len() {
            return prep_occ.get(ordinal).copied();
        }
        prep_occ.iter().min_by_key(|(_, c)| c.abs_diff(ws)).copied()
    }

    /// Editor position for byte offset `offset` into the preprocessed buffer.
    pub(crate) fn to_original(&self, offset: usize) -> Position {
        let offset = offset.min(self.preprocessed.len());
        let prep = match self.prep_starts.binary_search(&offset) {
            Ok(l) => l,
            Err(l) => l.saturating_sub(1),
        };
        let prep_text = self.prep_line(prep);
        let col = (offset - self.prep_starts[prep]).min(prep_text.len());
        let orig = self.origin_of(prep);
        let orig_text = self.orig_line(orig);

        if let Some((ws, we)) = word_at(prep_text, col) {
            let word = &prep_text[ws..we];
            let candidates = self.expansions_of(orig);
            let prep_occ: Vec<(usize, usize)> = candidates
                .iter()
                .flat_map(|&p| {
                    occurrences(self.prep_line(p), word)
                        .into_iter()
                        .map(move |c| (p, c))
                })
                .collect();
            let orig_occ = occurrences(orig_text, word);
            let mapped = match prep_occ.iter().position(|&o| o == (prep, ws)) {
                Some(k) if prep_occ.len() == orig_occ.len() => orig_occ.get(k).copied(),
                _ => orig_occ.iter().min_by_key(|&&o| o.abs_diff(ws)).copied(),
            };
            if let Some(at) = mapped {
                let byte = at + (col - ws).min(word.len());
                return Position {
                    line: orig as u32,
                    character: byte_to_utf16(orig_text, byte),
                };
            }
        }
        Position {
            line: orig as u32,
            character: byte_to_utf16(orig_text, col),
        }
    }

    /// Editor range for the preprocessed byte range `start..end`.
    pub(crate) fn range_to_original(&self, start: usize, end: usize) -> Range {
        let start_pos = self.to_original(start);
        // A single-identifier span keeps its width; anything else maps both
        // ends independently.
        let text = self.preprocessed.get(start..end).unwrap_or("");
        let end_pos = if !text.is_empty() && text.chars().all(is_ident_char) {
            Position {
                line: start_pos.line,
                character: start_pos.character + text.encode_utf16().count() as u32,
            }
        } else {
            self.to_original(end)
        };
        Range {
            start: start_pos,
            end: end_pos,
        }
    }

    /// [`Self::to_preprocessed`] as a position in the preprocessed buffer, for
    /// helpers that take a `Position` over that text.
    pub(crate) fn to_preprocessed_position(&self, pos: Position) -> Position {
        let offset = self.to_preprocessed(pos);
        let prep = match self.prep_starts.binary_search(&offset) {
            Ok(l) => l,
            Err(l) => l.saturating_sub(1),
        };
        let line = self.prep_line(prep);
        Position {
            line: prep as u32,
            character: byte_to_utf16(line, offset - self.prep_starts[prep]),
        }
    }
}

/// `true` for names the desugaring invents (`__typhon_q_0__`,
/// `__typhon_impl_Point`, `__typhon_Err__`), which hover and completion must
/// never surface.
pub(crate) fn is_desugaring_temporary(name: &str) -> bool {
    name.starts_with("__typhon_")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "def p(s: str) -> Result[int, str]:\n    return Ok(1)\n\ndef g(s: str) -> Result[int, str]:\n    let a: int = p(s)?\n    return Ok(a)\n\nlet y: int = 3\nprint(y)\n";

    fn map_for(src: &str) -> (tyc_syntax::preprocess::PreprocessResult, String) {
        let prep = tyc_syntax::preprocess::expand_and_preprocess_mapped(src, false);
        let text = prep.python_source.clone();
        (prep, text)
    }

    fn word_at_prep(text: &str, offset: usize) -> &str {
        let start = text[..offset]
            .rfind(|c: char| !is_ident_char(c))
            .map_or(0, |i| i + 1);
        let end = text[offset..]
            .find(|c: char| !is_ident_char(c))
            .map_or(text.len(), |i| offset + i);
        &text[start..end]
    }

    #[test]
    fn positions_below_a_question_mark_land_on_the_same_identifier() {
        let (prep, text) = map_for(SRC);
        let map = PositionMap::new(SRC, &text, &prep.line_map);
        // (line, column, identifier the editor shows there)
        for (line, col, want) in [
            (4u32, 8u32, "a"),
            (4, 17, "p"),
            (4, 19, "s"),
            (5, 14, "a"),
            (7, 4, "y"),
            (8, 6, "y"),
            (0, 4, "p"),
        ] {
            let off = map.to_preprocessed(Position {
                line,
                character: col,
            });
            assert_eq!(word_at_prep(&text, off), want, "at {line}:{col}");
            // And back again.
            let back = map.to_original(off);
            assert_eq!(
                (back.line, back.character),
                (line, col),
                "round trip of {want}"
            );
        }
    }

    #[test]
    fn repeated_names_on_an_expanded_line_map_by_order() {
        let src = "def p(s: str) -> Result[int, str]:\n    return Ok(1)\n\ndef g(s: str) -> Result[int, str]:\n    let b: int = p(s)? + p(s)?\n    return Ok(b)\n";
        let (prep, text) = map_for(src);
        let map = PositionMap::new(src, &text, &prep.line_map);
        let line = src.lines().nth(4).unwrap();
        let second = line.rfind("p(").unwrap() as u32;
        let first = line.find("p(").unwrap() as u32;
        let a = map.to_preprocessed(Position {
            line: 4,
            character: first,
        });
        let b = map.to_preprocessed(Position {
            line: 4,
            character: second,
        });
        assert_ne!(a, b, "the two calls must map to different expansions");
        assert_eq!(map.to_original(b).character, second);
        assert_eq!(map.to_original(a).character, first);
    }

    #[test]
    fn identity_map_with_stripped_prefix() {
        let src = "pub def f() -> int:\n    return 1\n";
        let (prep, text) = map_for(src);
        let map = PositionMap::new(src, &text, &prep.line_map);
        let off = map.to_preprocessed(Position {
            line: 0,
            character: 8,
        });
        assert_eq!(word_at_prep(&text, off), "f");
        assert_eq!(map.to_original(off).character, 8);
    }

    #[test]
    fn cursor_after_a_dot_maps_after_the_dot() {
        let src = "def p(s: str) -> Result[int, str]:\n    return Ok(1)\n\ndef g(s: str) -> Result[int, str]:\n    let a: int = p(s)?\n    return Ok(a)\n\nlet t = \"x\"\nt.\n";
        let (prep, text) = map_for(src);
        let map = PositionMap::new(src, &text, &prep.line_map);
        let off = map.to_preprocessed(Position {
            line: 8,
            character: 2,
        });
        assert!(text[..off].ends_with("t."), "got {:?}", &text[..off]);
    }

    #[test]
    fn desugaring_temporaries_are_recognised() {
        assert!(is_desugaring_temporary("__typhon_q_0__"));
        assert!(is_desugaring_temporary("__typhon_impl_Point"));
        assert!(!is_desugaring_temporary("typhon_value"));
    }
}
