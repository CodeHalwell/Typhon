//! Profile-guided optimisation (Phase 4).
//!
//! Reads the `typhon-profile.json` file produced by a prior `tyc profile`
//! run and decides which functions in the current build should be
//! auto-memoised based on observed call counts.
//!
//! The profile schema (emitted by `tyc/src/commands/profile.rs::_flush`)
//! is a single JSON object keyed by `"<module>.<qualname>"` whose values
//! are `{"calls": N, "total_seconds": F}`. We deliberately parse only
//! the fields we need without pulling in a full JSON crate; the schema
//! is owned by Typhon and changes here travel with it.

use std::collections::HashMap;
use std::path::Path;

/// One row in `typhon-profile.json` — the per-function counters that
/// PGO consults when deciding what to memoise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfileSample {
    pub calls: u64,
    pub total_seconds: f64,
}

/// Parse `typhon-profile.json` at `path` and return a map keyed by the
/// `<module>.<qualname>` string the profile records. Returns an empty
/// map (not an error) when the file is missing — PGO is opportunistic
/// and a missing profile simply means "no historical data".
pub fn load_profile_samples(path: &Path) -> HashMap<String, ProfileSample> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    parse_profile_json(&text)
}

/// Decide which function names in the current module should be added to
/// the auto-memoise list based on profile data.
///
/// `module_name` is the dotted Python module name for the file being
/// emitted (e.g. `main`, `pkg.sub.helpers`). The matcher requires an
/// exact `<module_name>.<fn>` key in `profile`, so a hot `main.fib`
/// sample never promotes a coincidentally-named `util.fib` in another
/// module. Bare-key profiles (no module qualifier) match by leaf as a
/// fallback for profiles produced by drivers that don't record
/// `__module__`.
///
/// Returns the subset of `candidate_fn_names` whose profile entry shows
/// at least `min_calls` invocations.
pub fn pgo_memoise_targets(
    profile: &HashMap<String, ProfileSample>,
    module_name: &str,
    candidate_fn_names: &[String],
    min_calls: u64,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in candidate_fn_names {
        let qualified = format!("{}.{}", module_name, name);
        // `tyc profile` records `fn.__module__`, and the entry module runs as
        // `__main__` — so a profile a user actually generates keys the entry
        // module's functions as `__main__.<fn>`, never `main.<fn>`. Accept
        // that spelling for the entry module (and only there: a `__main__`
        // sample can only have come from the script that was run).
        let script_qualified = format!("__main__.{}", name);
        let sample = profile
            .get(&qualified)
            .or_else(|| {
                (module_name == "main")
                    .then(|| profile.get(&script_qualified))
                    .flatten()
            })
            .or_else(|| profile.get(name));
        if sample.is_some_and(|s| s.calls >= min_calls) {
            out.push(name.clone());
        }
    }
    out
}

// ── parser ───────────────────────────────────────────────────────────────────

/// Minimal JSON parser for the profile schema. Accepts only the exact
/// shape `tyc profile` emits:
///
/// ```json
/// {
///   "module.fn": {"calls": 12, "total_seconds": 0.034},
///   …
/// }
/// ```
///
/// Tolerant of whitespace and key ordering inside each row; rejects
/// any value that isn't a non-negative number for `calls` /
/// `total_seconds`. Malformed input yields an empty map — PGO is
/// best-effort.
fn parse_profile_json(text: &str) -> HashMap<String, ProfileSample> {
    // All-or-nothing: build into a local map and only commit on success.
    // A truncated or otherwise malformed document yields an empty map
    // rather than a partially-populated one, matching the documented
    // best-effort behaviour. Without this, a parser error mid-document
    // would silently promote whatever functions had already been read.
    parse_profile_json_inner(text).unwrap_or_default()
}

fn parse_profile_json_inner(text: &str) -> Option<HashMap<String, ProfileSample>> {
    let mut out = HashMap::new();
    let mut cursor = Cursor::new(text);
    // JSON allows leading whitespace before the root value; tolerate it
    // so a profile saved with `json.dump(..., indent=2)` that wraps in
    // newlines still parses.
    cursor.skip_ws();
    if !cursor.eat_char('{') {
        return None;
    }
    cursor.skip_ws();
    if cursor.peek() == Some('}') {
        return Some(out);
    }
    loop {
        cursor.skip_ws();
        let key = cursor.read_string()?;
        cursor.skip_ws();
        if !cursor.eat_char(':') {
            return None;
        }
        cursor.skip_ws();
        let sample = cursor.read_sample()?;
        out.insert(key, sample);
        cursor.skip_ws();
        match cursor.peek() {
            Some(',') => {
                cursor.next();
            }
            Some('}') => break,
            _ => return None,
        }
    }
    Some(out)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.bytes.get(self.pos).map(|b| *b as char)
    }

    fn next(&mut self) {
        self.pos += 1;
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_ascii_whitespace() {
                self.next();
            } else {
                break;
            }
        }
    }

    fn eat_char(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.next();
            true
        } else {
            false
        }
    }

    /// Read a JSON string literal. Returns the unescaped contents (only
    /// the escapes we emit — `\"` and `\\`).
    fn read_string(&mut self) -> Option<String> {
        if !self.eat_char('"') {
            return None;
        }
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            match c {
                '"' => {
                    self.next();
                    return Some(out);
                }
                '\\' => {
                    self.next();
                    let esc = self.peek()?;
                    self.next();
                    out.push(esc);
                }
                _ => {
                    self.next();
                    out.push(c);
                }
            }
        }
    }

    /// Read a JSON number (positive only — the profile never emits a
    /// negative count or duration). Accepts integer or decimal.
    fn read_number(&mut self) -> Option<f64> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-' {
                self.next();
            } else {
                break;
            }
        }
        let s = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        s.parse::<f64>().ok()
    }

    /// Read a `{"calls": N, "total_seconds": F}` object in any field
    /// order.  Returns `None` if either field is missing or malformed.
    fn read_sample(&mut self) -> Option<ProfileSample> {
        if !self.eat_char('{') {
            return None;
        }
        let mut calls: Option<u64> = None;
        let mut total: Option<f64> = None;
        loop {
            self.skip_ws();
            let key = self.read_string()?;
            self.skip_ws();
            if !self.eat_char(':') {
                return None;
            }
            self.skip_ws();
            let value = self.read_number()?;
            match key.as_str() {
                "calls" => calls = Some(value.max(0.0) as u64),
                "total_seconds" => total = Some(value.max(0.0)),
                _ => {} // ignore unknown fields for forward compat
            }
            self.skip_ws();
            match self.peek() {
                Some(',') => {
                    self.next();
                }
                Some('}') => {
                    self.next();
                    break;
                }
                _ => return None,
            }
        }
        Some(ProfileSample {
            calls: calls.unwrap_or(0),
            total_seconds: total.unwrap_or(0.0),
        })
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_object_returns_empty_map() {
        let m = parse_profile_json("{}");
        assert!(m.is_empty());
    }

    #[test]
    fn parse_single_entry() {
        let json = "{\"main.fib\": {\"calls\": 42, \"total_seconds\": 0.123}}";
        let m = parse_profile_json(json);
        assert_eq!(m.len(), 1);
        let s = m.get("main.fib").unwrap();
        assert_eq!(s.calls, 42);
        assert!((s.total_seconds - 0.123).abs() < 1e-9);
    }

    #[test]
    fn parse_multiple_entries_with_whitespace() {
        let json = "{\n  \"a.f\": {\"calls\": 1, \"total_seconds\": 0.5},\n  \"b.g\": {\"calls\": 9, \"total_seconds\": 1.5}\n}";
        let m = parse_profile_json(json);
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("a.f").unwrap().calls, 1);
        assert_eq!(m.get("b.g").unwrap().calls, 9);
    }

    #[test]
    fn parse_handles_reordered_fields() {
        let json = "{\"x.y\": {\"total_seconds\": 2.0, \"calls\": 7}}";
        let m = parse_profile_json(json);
        assert_eq!(m.get("x.y").unwrap().calls, 7);
    }

    #[test]
    fn parse_returns_empty_on_malformed_input() {
        assert!(parse_profile_json("not json").is_empty());
        assert!(parse_profile_json("{\"a.b\": 5}").is_empty());
    }

    fn sample(calls: u64) -> ProfileSample {
        ProfileSample {
            calls,
            total_seconds: 0.1,
        }
    }

    #[test]
    fn pgo_targets_require_module_qualified_match() {
        // Regression for the Copilot/Codex review on leaf-name false
        // positives: a hot `main.fib` profile entry must promote `fib`
        // when emitting `main` but NOT when emitting `util` — those are
        // different functions despite sharing a leaf name.
        let mut profile = HashMap::new();
        profile.insert("main.fib".to_string(), sample(1000));
        let candidates = vec!["fib".to_string()];
        assert_eq!(
            pgo_memoise_targets(&profile, "main", &candidates, 100),
            vec!["fib"]
        );
        assert!(
            pgo_memoise_targets(&profile, "util", &candidates, 100).is_empty(),
            "different module must not match"
        );
    }

    /// A profile written by `tyc profile` keys the entry module's functions
    /// as `__main__.<fn>` (that is what `fn.__module__` reads when the script
    /// is run), so the round trip never promoted anything until the entry
    /// module accepted that spelling. Other modules must not: a
    /// `__main__.helper` sample says nothing about `util.helper`.
    #[test]
    fn pgo_targets_accept_dunder_main_for_the_entry_module() {
        let mut profile = HashMap::new();
        profile.insert("__main__.fib".to_string(), sample(1000));
        let candidates = vec!["fib".to_string()];
        assert_eq!(
            pgo_memoise_targets(&profile, "main", &candidates, 100),
            vec!["fib"]
        );
        assert!(pgo_memoise_targets(&profile, "util", &candidates, 100).is_empty());
    }

    #[test]
    fn pgo_targets_match_in_subpackage() {
        // Regression for the gemini review on subpackages: dotted module
        // names like `pkg.sub` must match a profile entry of the same
        // shape (`pkg.sub.helper`), not be rejected for containing a dot.
        let mut profile = HashMap::new();
        profile.insert("pkg.sub.helper".to_string(), sample(1000));
        let candidates = vec!["helper".to_string()];
        let targets = pgo_memoise_targets(&profile, "pkg.sub", &candidates, 100);
        assert_eq!(targets, vec!["helper"], "subpackage match must work");
    }

    #[test]
    fn pgo_targets_respect_min_calls_threshold() {
        let mut profile = HashMap::new();
        profile.insert("main.cold".to_string(), sample(5));
        let candidates = vec!["cold".to_string()];
        let targets = pgo_memoise_targets(&profile, "main", &candidates, 100);
        assert!(
            targets.is_empty(),
            "below-threshold fn must not be promoted"
        );
    }

    #[test]
    fn pgo_targets_skip_class_methods() {
        // `main.Foo.method` does not match the constructed `main.method`
        // key, so the matcher correctly skips it without needing any
        // class-naming heuristic.
        let mut profile = HashMap::new();
        profile.insert("main.Foo.method".to_string(), sample(1000));
        let candidates = vec!["method".to_string()];
        let targets = pgo_memoise_targets(&profile, "main", &candidates, 100);
        assert!(targets.is_empty(), "class methods must not match");
    }

    #[test]
    fn pgo_targets_match_bare_key_fallback() {
        // A profile entry written as bare `fib` (no module qualifier)
        // still matches as a fallback — useful for profiles produced by
        // drivers that don't record `__module__`.
        let mut profile = HashMap::new();
        profile.insert("fib".to_string(), sample(1000));
        let candidates = vec!["fib".to_string()];
        let targets = pgo_memoise_targets(&profile, "main", &candidates, 100);
        assert_eq!(targets, vec!["fib"]);
    }

    #[test]
    fn parse_tolerates_leading_whitespace() {
        // Regression for the gemini review: JSON allows leading
        // whitespace before the root value. A profile written with
        // `json.dump(..., indent=2)` plus surrounding newlines must
        // still parse.
        let json = "\n  \t{\"main.fib\": {\"calls\": 7, \"total_seconds\": 0.1}}";
        let m = parse_profile_json(json);
        assert_eq!(m.get("main.fib").unwrap().calls, 7);
    }

    #[test]
    fn parse_malformed_after_valid_entries_yields_empty_map() {
        // Regression for the Copilot review on partial maps: a profile
        // that is well-formed for the first row but truncated after must
        // produce an empty map, not a half-populated one.
        let json = "{\"main.fib\": {\"calls\": 7, \"total_seconds\": 0.1}, \"main.broken\": ";
        let m = parse_profile_json(json);
        assert!(m.is_empty(), "partial parse must collapse to empty map");
    }

    #[test]
    fn load_missing_profile_returns_empty_map() {
        let path = Path::new("/no/such/typhon-profile.json");
        assert!(load_profile_samples(path).is_empty());
    }

    #[test]
    fn load_existing_profile_round_trips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("typhon-profile.json");
        std::fs::write(&path, "{\"m.f\": {\"calls\": 17, \"total_seconds\": 0.05}}").unwrap();
        let m = load_profile_samples(&path);
        assert_eq!(m.get("m.f").unwrap().calls, 17);
    }
}
