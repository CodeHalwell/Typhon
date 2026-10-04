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

use ruff_python_ast::comparable::ComparableModModule;
use ruff_python_ast::{self as ast, Expr, ModModule, Stmt};

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
/// appearance on each side before parsing: some lowerings number their
/// temporaries by source line (`__typhon_guard_12`), and a formatter that
/// adds a PEP 8 blank line legitimately renumbers them.
pub fn compare_python_sources(before: &str, after: &str, opts: CompareOptions) -> AstComparison {
    let before = canonicalise_generated_names(before);
    let after = canonicalise_generated_names(after);
    let mut before = match ruff_python_parser::parse_module(&before) {
        Ok(parsed) => parsed.into_syntax(),
        Err(e) => return AstComparison::BeforeDoesNotParse(e.to_string()),
    };
    let mut after = match ruff_python_parser::parse_module(&after) {
        Ok(parsed) => parsed.into_syntax(),
        Err(e) => return AstComparison::AfterDoesNotParse(e.to_string()),
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

/// Rename every identifier-shaped run that starts with `__typhon_` to
/// `__typhon_c<N>__`, numbering distinct names by first appearance.
fn canonicalise_generated_names(src: &str) -> String {
    const PREFIX: &str = "__typhon_";
    if !src.contains(PREFIX) {
        return src.to_owned();
    }
    let bytes = src.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut out = String::with_capacity(src.len());
    let mut last = 0usize;
    let mut search = 0usize;
    while let Some(rel) = src[search..].find(PREFIX) {
        let start = search + rel;
        // Must start an identifier, not sit inside one (`x__typhon_y`).
        if start > 0 && is_ident(bytes[start - 1]) {
            search = start + PREFIX.len();
            continue;
        }
        let mut end = start + PREFIX.len();
        while end < bytes.len() && is_ident(bytes[end]) {
            end += 1;
        }
        let name = &src[start..end];
        let next = seen.len();
        let id = *seen.entry(name).or_insert(next);
        out.push_str(&src[last..start]);
        out.push_str(&format!("__typhon_c{id}__"));
        last = end;
        search = end;
    }
    out.push_str(&src[last..]);
    out
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
