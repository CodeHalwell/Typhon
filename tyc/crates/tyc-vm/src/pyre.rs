//! Python regular-expression semantics over the Rust `regex` engine.
//!
//! The `re` shim hands its patterns to `regex`, whose syntax is close to
//! Python's but not the same. This module is the one place that knows the
//! differences:
//!
//! - `$` without `MULTILINE` matches at the end of the string **and just
//!   before a newline that ends it** (`re.search("a$", "a\n")` matches);
//!   `regex`'s `$` is the very end only.
//! - `\Z` is Python's "very end" (`regex` spells it `\z`).
//! - `fullmatch` asks whether *some* way of matching covers the whole
//!   string. Taking the engine's preferred (leftmost-first) match and then
//!   comparing its end is not that: `x*|xy` prefers `x` on `"xy"`, yet
//!   `xy` matches all of it. The whole alternation is anchored instead:
//!   `(?:x*|xy)\z`.
//! - `(?P<name>…)`, `(?#comment)`, and a character class may hold a bare
//!   `[` (and `&&` / `~~`, which `regex` reads as set operators).
//! - Successive matches may be empty right after a non-empty one
//!   (`re.sub("x*", "-", "abxd")` is `-a-b--d-`).

use std::cell::OnceCell;

use regex::{Captures, Regex};

/// A compiled Python pattern. Derefs to the underlying [`Regex`] for the
/// structural queries (group count and names); matching goes through the
/// methods here.
pub struct PyRe {
    re: Regex,
    prefix: String,
    parts: Translated,
    /// `$` as "before any newline or at the end" (see [`PyRe::captures_at`]).
    line: OnceCell<Option<Regex>>,
    /// For a haystack with its final newline cut off: `\Z` never matches.
    cut: OnceCell<Option<Regex>>,
    /// `(?:pattern)\z`, for `fullmatch`.
    full: OnceCell<Option<Regex>>,
}

impl std::ops::Deref for PyRe {
    type Target = Regex;
    fn deref(&self) -> &Regex {
        &self.re
    }
}

/// The three spellings of one pattern (see [`PyRe`]'s fields).
struct Translated {
    base: String,
    line: String,
    cut: String,
    /// A `$` whose Python meaning differs from `regex`'s is present.
    dollar: bool,
    verbose: bool,
}

/// The letters of a global inline-flag group opening the pattern
/// (`(?im)…`), and the length of that group.
fn leading_flags(chars: &[char]) -> Option<(String, usize)> {
    if chars.len() < 4 || chars[0] != '(' || chars[1] != '?' {
        return None;
    }
    let mut i = 2;
    while i < chars.len() && "aiLmsux".contains(chars[i]) {
        i += 1;
    }
    (i > 2 && chars.get(i) == Some(&')')).then(|| (chars[2..i].iter().collect(), i + 1))
}

fn translate(pattern: &str, mut multiline: bool, mut verbose: bool) -> Translated {
    let chars: Vec<char> = pattern.chars().collect();
    let n = chars.len();
    let mut out = [String::new(), String::new(), String::new()];
    let all = |s: &str, out: &mut [String; 3]| {
        for o in out.iter_mut() {
            o.push_str(s);
        }
    };
    let mut dollar = false;
    let mut scoped_multiline = false;
    let mut i = 0;
    if let Some((flags, len)) = leading_flags(&chars) {
        multiline |= flags.contains('m');
        verbose |= flags.contains('x');
        // `a` (ASCII) is `regex`'s `-u`; `u` is already the default.
        let mut group = String::from("(?");
        group.extend(flags.chars().filter(|c| !matches!(c, 'a' | 'u')));
        if flags.contains('a') {
            group.push_str("-u");
        }
        group.push(')');
        if group != "(?)" {
            all(&group, &mut out);
        }
        i = len;
    }
    while i < n {
        let c = chars[i];
        match c {
            '\\' => {
                match chars.get(i + 1) {
                    Some('Z') => {
                        out[0].push_str("\\z");
                        out[1].push_str("\\z");
                        // No position of the shortened haystack is the end
                        // of the real one.
                        out[2].push_str("[^\\s\\S]");
                    }
                    Some(next) => {
                        let mut pair = String::from('\\');
                        pair.push(*next);
                        all(&pair, &mut out);
                    }
                    None => all("\\", &mut out),
                }
                i += 2;
            }
            '[' => {
                // A character class, copied through its closing bracket.
                let mut class = String::from('[');
                let mut j = i + 1;
                if chars.get(j) == Some(&'^') {
                    class.push('^');
                    j += 1;
                }
                // A `]` first in the class is a literal.
                if chars.get(j) == Some(&']') {
                    class.push_str("\\]");
                    j += 1;
                }
                while j < n && chars[j] != ']' {
                    match chars[j] {
                        '\\' => {
                            class.push('\\');
                            if let Some(next) = chars.get(j + 1) {
                                class.push(*next);
                            }
                            j += 1;
                        }
                        // Literals in Python; syntax in `regex`.
                        '[' => class.push_str("\\["),
                        '&' => class.push_str("\\&"),
                        '~' => class.push_str("\\~"),
                        other => class.push(other),
                    }
                    j += 1;
                }
                if j < n {
                    class.push(']');
                }
                all(&class, &mut out);
                i = j + 1;
            }
            '(' if chars.get(i + 1) == Some(&'?') => {
                let rest: String = chars[i + 2..n.min(i + 12)].iter().collect();
                if rest.starts_with("P<") {
                    all("(?<", &mut out);
                    i += 4;
                } else if rest.starts_with('#') {
                    // `(?#comment)`: `regex` has no such group.
                    let mut j = i + 3;
                    while j < n && chars[j] != ')' {
                        j += 1;
                    }
                    i = j + 1;
                } else {
                    // A scoped flag group that touches `m` changes what `$`
                    // means inside it; leave such patterns as written.
                    let flags: String = rest
                        .chars()
                        .take_while(|c| "aiLmsux-".contains(*c))
                        .collect();
                    if !flags.is_empty() && flags.contains('m') {
                        scoped_multiline = true;
                    }
                    all("(", &mut out);
                    i += 1;
                }
            }
            '$' => {
                dollar = true;
                out[0].push('$');
                out[1].push_str(if multiline { "$" } else { "(?m:$)" });
                out[2].push('$');
                i += 1;
            }
            '#' if verbose => {
                // A verbose-mode comment runs to the end of the line.
                let mut j = i;
                let mut comment = String::new();
                while j < n && chars[j] != '\n' {
                    comment.push(chars[j]);
                    j += 1;
                }
                all(&comment, &mut out);
                i = j;
            }
            other => {
                let mut buf = [0u8; 4];
                all(other.encode_utf8(&mut buf), &mut out);
                i += 1;
            }
        }
    }
    let [base, line, cut] = out;
    Translated {
        base,
        line,
        cut,
        dollar: dollar && !multiline && !scoped_multiline,
        verbose,
    }
}

impl PyRe {
    /// Compile `pattern`. `prefix` is the inline-flag group for the flags
    /// passed to `re.compile` (`"(?im)"`, or empty); `multiline` and
    /// `verbose` say whether those flags include `M` / `X`.
    pub fn new(
        pattern: &str,
        prefix: &str,
        multiline: bool,
        verbose: bool,
    ) -> Result<PyRe, regex::Error> {
        let parts = translate(pattern, multiline, verbose);
        let re = Regex::new(&format!("{prefix}{}", parts.base))?;
        Ok(PyRe {
            re,
            prefix: prefix.to_owned(),
            parts,
            line: OnceCell::new(),
            cut: OnceCell::new(),
            full: OnceCell::new(),
        })
    }

    fn variant<'a>(
        &'a self,
        cell: &'a OnceCell<Option<Regex>>,
        body: impl FnOnce() -> String,
    ) -> &'a Regex {
        cell.get_or_init(|| Regex::new(&format!("{}{}", self.prefix, body())).ok())
            .as_ref()
            // The variants differ from the base pattern only by assertions
            // `regex` accepts; should one still fail to compile, fall back.
            .unwrap_or(&self.re)
    }

    /// The first match starting at or after byte `pos` — `Pattern.search`.
    ///
    /// Python's `$` also matches just before a final newline. For a
    /// haystack that does not end in one, that is `regex`'s `$`. For a
    /// haystack whose only newline is the final one, it is exactly
    /// `(?m:$)`. Otherwise two searches are compared: the pattern as
    /// written (`$` at the very end), and the pattern over the haystack
    /// without its final newline (`$` just before it); the leftmost wins.
    pub fn captures_at<'h>(&self, hay: &'h str, pos: usize) -> Option<Captures<'h>> {
        self.captures_at_in(hay, pos, self.classify(hay))
    }

    /// Which of the three `$` strategies `hay` needs. It depends only on
    /// the haystack, so a caller making many searches over one haystack
    /// works it out once: re-scanning for an interior newline on every
    /// match made `findall("a|$", "a" * n + "\n")` quadratic.
    fn classify(&self, hay: &str) -> Dollar {
        if !self.parts.dollar || !hay.ends_with('\n') {
            Dollar::Plain
        } else if hay[..hay.len() - 1].contains('\n') {
            Dollar::Compare
        } else {
            Dollar::LineMode
        }
    }

    fn captures_at_in<'h>(&self, hay: &'h str, pos: usize, dollar: Dollar) -> Option<Captures<'h>> {
        match dollar {
            Dollar::Plain => return self.re.captures_at(hay, pos),
            Dollar::LineMode => {
                return self
                    .variant(&self.line, || self.parts.line.clone())
                    .captures_at(hay, pos)
            }
            Dollar::Compare => {}
        }
        let head = &hay[..hay.len() - 1];
        let at_end = self.re.captures_at(hay, pos);
        if pos > head.len() {
            return at_end;
        }
        let before_newline = self
            .variant(&self.cut, || self.parts.cut.clone())
            .captures_at(head, pos);
        match (at_end, before_newline) {
            (Some(a), Some(b)) => {
                let (sa, sb) = (a.get(0).map(|m| m.start()), b.get(0).map(|m| m.start()));
                Some(if sb < sa { b } else { a })
            }
            (a, b) => a.or(b),
        }
    }

    /// `Pattern.search` from the start.
    pub fn captures<'h>(&self, hay: &'h str) -> Option<Captures<'h>> {
        self.captures_at(hay, 0)
    }

    /// `Pattern.match`: a match that starts exactly at `pos`.
    pub fn match_at<'h>(&self, hay: &'h str, pos: usize) -> Option<Captures<'h>> {
        self.captures_at(hay, pos)
            .filter(|c| c.get(0).is_some_and(|m| m.start() == pos))
    }

    /// `Pattern.fullmatch`: a match from `pos` to the end of `hay`.
    pub fn fullmatch_at<'h>(&self, hay: &'h str, pos: usize) -> Option<Captures<'h>> {
        let body = || {
            if self.parts.verbose {
                // A trailing `# comment` must not swallow the closing group.
                format!("(?:{}\n)\\z", self.parts.base)
            } else {
                format!("(?:{})\\z", self.parts.base)
            }
        };
        self.variant(&self.full, body)
            .captures_at(hay, pos)
            .filter(|c| {
                c.get(0)
                    .is_some_and(|m| m.start() == pos && m.end() == hay.len())
            })
    }

    /// Every successive match from `pos`, as `finditer` / `findall` /
    /// `sub` / `split` walk them: the next search starts where the last
    /// match ended, and an empty match is not repeated at the same place.
    pub fn captures_all<'h>(&self, hay: &'h str, pos: usize) -> Vec<Captures<'h>> {
        self.captures_up_to(hay, pos, 0)
    }

    /// [`Self::captures_all`], stopping after `limit` matches (`0` means no
    /// limit) — `sub(count=…)` and `split(maxsplit=…)` need only that many,
    /// and `sub("a", "b", "a" * 10**7, count=1)` must not collect ten
    /// million matches to use one.
    pub fn captures_up_to<'h>(&self, hay: &'h str, pos: usize, limit: usize) -> Vec<Captures<'h>> {
        let dollar = self.classify(hay);
        let mut out = Vec::new();
        let mut at = pos;
        let mut last_empty: Option<usize> = None;
        while at <= hay.len() && (limit == 0 || out.len() < limit) {
            let Some(caps) = self.captures_at_in(hay, at, dollar) else {
                break;
            };
            let Some(whole) = caps.get(0) else {
                break;
            };
            let (start, end) = (whole.start(), whole.end());
            if start == end && last_empty == Some(start) {
                // The same empty match again: step one character.
                match hay[at..].chars().next() {
                    Some(ch) => at += ch.len_utf8(),
                    None => break,
                }
                last_empty = None;
                continue;
            }
            last_empty = (start == end).then_some(end);
            at = end;
            out.push(caps);
        }
        out
    }
}

/// How `$` is matched for one haystack; see [`PyRe::classify`].
#[derive(Clone, Copy)]
enum Dollar {
    /// The pattern has no `$`, or the haystack does not end in a newline.
    Plain,
    /// The haystack's only newline is its last character: `(?m:$)`.
    LineMode,
    /// Interior newlines and a final one: two searches, leftmost wins.
    Compare,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(c: Option<Captures<'_>>) -> Option<(usize, usize)> {
        c.and_then(|c| c.get(0).map(|m| (m.start(), m.end())))
    }

    fn re(p: &str) -> PyRe {
        PyRe::new(p, "", false, false).expect("pattern compiles")
    }

    #[test]
    fn dollar_matches_before_a_final_newline_only() {
        assert_eq!(span(re("a$").captures("a\n")), Some((0, 1)));
        assert_eq!(span(re("a$").captures("a")), Some((0, 1)));
        // Not before an interior newline…
        assert_eq!(span(re("a$").captures("a\nb")), None);
        assert_eq!(span(re("a$").captures("a\nb\n")), None);
        // …but before the final one of several.
        assert_eq!(span(re("b$").captures("a\nb\n")), Some((2, 3)));
        assert_eq!(span(re(r"^\d+$").captures("123\n")), Some((0, 3)));
        assert_eq!(span(re(".*$").captures("abc\n")), Some((0, 3)));
        // `\Z` is the very end, and an escaped or bracketed `$` is literal.
        assert_eq!(span(re(r"a\Z").captures("a\n")), None);
        assert_eq!(span(re(r"a\n\Z").captures("x\na\n")), Some((2, 4)));
        assert_eq!(span(re(r"a\$").captures("a$\n")), Some((0, 2)));
        assert_eq!(span(re("a[$]").captures("a$")), Some((0, 2)));
    }

    #[test]
    fn multiline_dollar_is_left_alone() {
        let m = PyRe::new("a$", "(?m)", true, false).unwrap();
        assert_eq!(span(m.captures("a\nb\n")), Some((0, 1)));
        let inline = re("(?m)a$");
        assert_eq!(span(inline.captures("x\na\nb")), Some((2, 3)));
    }

    #[test]
    fn fullmatch_tries_every_alternative() {
        assert_eq!(span(re("x*|xy").fullmatch_at("xy", 0)), Some((0, 2)));
        assert_eq!(span(re("a|ab").fullmatch_at("ab", 0)), Some((0, 2)));
        assert_eq!(span(re("a|ab").fullmatch_at("abc", 0)), None);
        assert_eq!(span(re("a$").fullmatch_at("a\n", 0)), None);
        assert_eq!(span(re("b|bc").fullmatch_at("abc", 1)), Some((1, 3)));
        // The preferred alternative still wins for `match`.
        assert_eq!(span(re("x*|xy").match_at("xy", 0)), Some((0, 1)));
    }

    /// A bounded operation collects only the matches it will use, and many
    /// matches over one haystack classify it once (both were quadratic or
    /// worse: `sub(count=1)` over ten million matches, `findall("a|$", …)`).
    #[test]
    fn bounded_and_repeated_matching_stay_linear() {
        let hay = "a".repeat(2_000_000);
        assert_eq!(re("a").captures_up_to(&hay, 0, 1).len(), 1);
        assert_eq!(re("a").captures_up_to(&hay, 0, 3).len(), 3);
        assert_eq!(re("a").captures_up_to("aaaa", 0, 0).len(), 4);

        let mut line = "a".repeat(200_000);
        line.push('\n');
        let started = std::time::Instant::now();
        // 200,000 `a` matches, then `$` before the newline and at the end.
        assert_eq!(re("a|$").captures_all(&line, 0).len(), 200_002);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn successive_matches_follow_python() {
        let spans = |p: &str, s: &str| -> Vec<(usize, usize)> {
            re(p)
                .captures_all(s, 0)
                .into_iter()
                .filter_map(|c| span(Some(c)))
                .collect()
        };
        assert_eq!(
            spans("x*", "abxd"),
            vec![(0, 0), (1, 1), (2, 3), (3, 3), (4, 4)]
        );
        assert_eq!(spans("a", "aa"), vec![(0, 1), (1, 2)]);
        assert_eq!(spans("$", "a\n"), vec![(1, 1), (2, 2)]);
    }

    #[test]
    fn python_only_syntax_is_translated() {
        assert_eq!(span(re(r"(?P<n>a)(?#note)b").captures("ab")), Some((0, 2)));
        assert_eq!(span(re(r"[[a]+").captures("[a[")), Some((0, 3)));
        assert_eq!(span(re(r"[]a]+").captures("]a")), Some((0, 2)));
        assert_eq!(span(re(r"[a&&b]+").captures("a&&b")), Some((0, 4)));
    }
}
