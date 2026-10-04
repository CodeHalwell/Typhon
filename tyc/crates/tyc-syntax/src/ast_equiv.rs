//! "Did this text edit change the program?" — structural equality of two
//! Python sources, for the formatter's self-check.
//!
//! `tyc fmt` rewrites the user's file in place, and it is the one tool people
//! run without reading the diff. It has corrupted source in four release lines
//! (`?` → `| None`, string-literal contents respaced, …), each time by a
//! text-level edit that a parse of its own output would have caught. The
//! formatter therefore lowers its output exactly as it lowered its input,
//! parses both, and refuses to write unless the two module ASTs are equal.
//! This module owns the comparison so the vendored AST crate stays wrapped in
//! one place.

use std::collections::HashMap;

use ruff_python_ast::comparable::ComparableModModule;
use ruff_python_ast::token::{TokenKind, Tokens};
use ruff_python_ast::{self as ast, Expr, ModModule, Stmt};
use ruff_text_size::Ranged;

/// Outcome of [`compare_python_sources`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AstComparison {
    /// Both sources parse to the same module.
    Same,
    /// Both parse, to different modules.
    Different,
    /// The `before` side does not parse (message from the parser).
    BeforeDoesNotParse(String),
    /// The `after` side does not parse (message from the parser).
    AfterDoesNotParse(String),
}

/// Options for [`compare_python_sources`].
#[derive(Debug, Clone, Copy, Default)]
pub struct CompareOptions {
    /// Compare docstrings modulo whitespace: every line trimmed, blank lines
    /// dropped. `ruff format` legitimately re-indents docstrings and strips
    /// their trailing whitespace, which changes the string constant; set
    /// this only when `ruff format` touched the `after` text.
    pub lenient_docstrings: bool,
}

/// Parse `before` and `after` as Python and report whether they denote the
/// same module, ignoring source positions and comments.
///
/// Compiler-generated names (`__typhon_…`) are renamed by order of first
/// appearance on each side: some lowerings number their temporaries by
/// source line (`__typhon_guard_12`), and a formatter that adds a PEP 8
/// blank line legitimately renumbers them. Only identifier tokens are
/// renamed — the same text inside a string literal is program data, and
/// a change to it is a change to the program.
pub fn compare_python_sources(before: &str, after: &str, opts: CompareOptions) -> AstComparison {
    let mut before = match parse_canonical(before) {
        Ok(module) => module,
        Err(e) => return AstComparison::BeforeDoesNotParse(e),
    };
    let mut after = match parse_canonical(after) {
        Ok(module) => module,
        Err(e) => return AstComparison::AfterDoesNotParse(e),
    };
    if opts.lenient_docstrings {
        normalise_docstrings(&mut before.body);
        normalise_docstrings(&mut after.body);
    }
    if modules_equal(&before, &after) {
        AstComparison::Same
    } else {
        AstComparison::Different
    }
}

fn modules_equal(a: &ModModule, b: &ModModule) -> bool {
    ComparableModModule::from(a) == ComparableModModule::from(b)
}

/// Parse `src`, with its compiler-generated names canonicalised.
fn parse_canonical(src: &str) -> Result<ModModule, String> {
    let parsed = ruff_python_parser::parse_module(src).map_err(|e| e.to_string())?;
    match canonicalise_generated_names(src, parsed.tokens()) {
        None => Ok(parsed.into_syntax()),
        Some(canonical) => ruff_python_parser::parse_module(&canonical)
            .map(|p| p.into_syntax())
            .map_err(|e| e.to_string()),
    }
}

/// Rename every identifier token of `src` that starts with `__typhon_` to
/// `__typhon_c<N>__`, numbering distinct names by first appearance. String
/// literal contents (f-string literal parts included) and comments are
/// different tokens, so they are left exactly as written. `None` when no
/// identifier needs renaming.
fn canonicalise_generated_names(src: &str, tokens: &Tokens) -> Option<String> {
    const PREFIX: &str = "__typhon_";
    if !src.contains(PREFIX) {
        return None;
    }
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut out = String::with_capacity(src.len());
    let mut last = 0usize;
    for token in tokens {
        if token.kind() != TokenKind::Name {
            continue;
        }
        let range = token.range();
        let name = &src[range];
        if !name.starts_with(PREFIX) {
            continue;
        }
        let next = seen.len();
        let id = *seen.entry(name).or_insert(next);
        out.push_str(&src[last..range.start().to_usize()]);
        out.push_str(&format!("__typhon_c{id}__"));
        last = range.end().to_usize();
    }
    if seen.is_empty() {
        return None;
    }
    out.push_str(&src[last..]);
    Some(out)
}

/// Replace every docstring (the leading string-literal expression statement
/// of a module, class or function body) with its whitespace-normalised form.
fn normalise_docstrings(body: &mut [Stmt]) {
    if let Some(Stmt::Expr(stmt)) = body.first_mut() {
        if let Expr::StringLiteral(lit) = stmt.value.as_mut() {
            // Join the parts by hand: `StringLiteralValue::to_str` caches the
            // joined text of a concatenated literal, and that cache would go
            // stale under the edit below.
            let joined: String = lit.value.iter().map(|part| &*part.value).collect();
            let normalised = joined
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let mut parts = lit.value.iter_mut();
            if let Some(first) = parts.next() {
                first.value = normalised.into_boxed_str();
            }
            for rest in parts {
                rest.value = "".into();
            }
        }
    }
    for stmt in body.iter_mut() {
        for_each_nested_body(stmt, &mut |inner| normalise_docstrings(inner));
    }
}

/// Call `f` on every statement list nested directly in `stmt`.
fn for_each_nested_body(stmt: &mut Stmt, f: &mut dyn FnMut(&mut [Stmt])) {
    match stmt {
        Stmt::FunctionDef(def) => f(&mut def.body),
        Stmt::ClassDef(class) => f(&mut class.body),
        Stmt::If(s) => {
            f(&mut s.body);
            for clause in &mut s.elif_else_clauses {
                f(&mut clause.body);
            }
        }
        Stmt::For(s) => {
            f(&mut s.body);
            f(&mut s.orelse);
        }
        Stmt::While(s) => {
            f(&mut s.body);
            f(&mut s.orelse);
        }
        Stmt::With(s) => f(&mut s.body),
        Stmt::Try(s) => {
            f(&mut s.body);
            for handler in &mut s.handlers {
                let ast::ExceptHandler::ExceptHandler(h) = handler;
                f(&mut h.body);
            }
            f(&mut s.orelse);
            f(&mut s.finalbody);
        }
        Stmt::Match(s) => {
            for case in &mut s.cases {
                f(&mut case.body);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmp(a: &str, b: &str) -> AstComparison {
        compare_python_sources(a, b, CompareOptions::default())
    }

    #[test]
    fn whitespace_and_comments_do_not_matter() {
        assert_eq!(
            cmp("x=f(1,2)\n", "x = f(1, 2)  # note\n\n\n"),
            AstComparison::Same
        );
    }

    #[test]
    fn a_changed_string_constant_is_different() {
        assert_eq!(
            cmp("a = \"q,r  s\"\n", "a = \"q, r s\"\n"),
            AstComparison::Different
        );
    }

    #[test]
    fn an_optional_rewritten_to_a_union_is_different() {
        assert_eq!(
            cmp("z: int = r\n", "z: int = r | None\n"),
            AstComparison::Different
        );
    }

    #[test]
    fn let_and_mut_are_part_of_the_ast() {
        assert_eq!(
            cmp("let x: int = 1\n", "mut x: int = 1\n"),
            AstComparison::Different
        );
    }

    #[test]
    fn line_numbered_generated_names_are_canonicalised() {
        assert_eq!(
            cmp(
                "__typhon_guard_3 = f()\nx = __typhon_guard_3\n",
                "__typhon_guard_5 = f()\nx = __typhon_guard_5\n",
            ),
            AstComparison::Same
        );
        // Two distinct generated names stay distinct.
        assert_eq!(
            cmp(
                "__typhon_guard_3 = f()\nx = __typhon_guard_4\n",
                "__typhon_guard_5 = f()\nx = __typhon_guard_5\n",
            ),
            AstComparison::Different
        );
    }

    #[test]
    fn generated_names_inside_strings_are_compared_as_written() {
        // Only identifiers are canonicalised: the same text inside a
        // string literal is data, and a changed literal is a changed program.
        assert_eq!(
            cmp("x = \"__typhon_old\"\n", "x = \"__typhon_new\"\n"),
            AstComparison::Different
        );
        assert_eq!(
            cmp(
                "x = f\"{__typhon_a}-__typhon_old\"\n",
                "x = f\"{__typhon_b}-__typhon_new\"\n",
            ),
            AstComparison::Different
        );
        // An identifier inside an f-string replacement field is still one.
        assert_eq!(
            cmp(
                "__typhon_q_1 = 1\nx = f\"{__typhon_q_1}-__typhon_lit\"\n",
                "__typhon_q_7 = 1\nx = f\"{__typhon_q_7}-__typhon_lit\"\n",
            ),
            AstComparison::Same
        );
        // A string mentioning a generated name does not shift the numbering.
        assert_eq!(
            cmp(
                "s = \"__typhon_z\"\n__typhon_g_3 = f()\n",
                "s = \"__typhon_z\"\n__typhon_g_9 = f()\n",
            ),
            AstComparison::Same
        );
    }

    #[test]
    fn an_unparseable_after_is_reported() {
        assert!(matches!(
            cmp("x = 1\n", "x = (\n"),
            AstComparison::AfterDoesNotParse(_)
        ));
    }

    #[test]
    fn docstring_whitespace_is_compared_only_when_lenient() {
        let a = "def f():\n    \"\"\"Doc.\n\n        more   \n    \"\"\"\n";
        let b = "def f():\n    \"\"\"Doc.\n\n    more\n    \"\"\"\n";
        assert_eq!(cmp(a, b), AstComparison::Different);
        assert_eq!(
            compare_python_sources(
                a,
                b,
                CompareOptions {
                    lenient_docstrings: true
                }
            ),
            AstComparison::Same
        );
        // A real content change in a docstring still differs when lenient.
        let c = "def f():\n    \"\"\"Doc!\n    \"\"\"\n";
        assert_eq!(
            compare_python_sources(
                a,
                c,
                CompareOptions {
                    lenient_docstrings: true
                }
            ),
            AstComparison::Different
        );
    }
}
