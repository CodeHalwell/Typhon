//! Typhon-specific tokens that extend the Python token set.
//!
//! Phase 0 introduces `val` and `var` as first-class keywords.
//! Phase 2 adds `model` (Pydantic class emission) and `comptime` (build-time
//! constant evaluation).
//! Phase 3 adds `impl`, `extend`, `interface`, `unsafe`, `gather`, `go`, and
//! `lazy` for extension methods, structural typing, dynamism boundaries,
//! concurrency primitives, and deferred module loading.
//! These are recognised here and stripped (pre-processed) before the
//! underlying Python parser sees them, so the existing Python parser can
//! handle the remainder of the grammar unchanged.

/// A Typhon keyword that is not part of standard Python.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TyphonKeyword {
    /// `val` — declares an immutable binding (like Rust's `let` or Kotlin's `val`).
    Val,
    /// `var` — declares a mutable binding.
    Var,
    /// `model` — declares a Pydantic `BaseModel` class instead of a dataclass.
    Model,
    /// `comptime` — marks a binding whose RHS is evaluated at build time and
    /// inlined as a literal in the emitted Python.
    Comptime,
    /// `impl` — attaches a method block to a previously-declared class, Rust-style.
    /// The preprocessor rewrites `impl ClassName:` to `class __typhon_impl_ClassName(object):`,
    /// and the desugar pass merges the methods back into the target class.
    Impl,
    /// `extend` — like `impl`, but for adding methods to a type.
    /// For user-defined classes it is treated identically to `impl`.
    Extend,
    /// `interface` — declares a structural protocol. The preprocessor rewrites
    /// `interface Name:` to `class Name(Protocol):`.
    Interface,
    /// `unsafe` — opens a lexical region in which `Any` types may flow freely.
    /// The preprocessor rewrites `unsafe:` to `if True:  # __typhon_unsafe__` so
    /// scoping survives the Python round-trip.
    Unsafe,
    /// `gather` — runs the inner bindings concurrently. Defaults to
    /// `asyncio.TaskGroup`; `gather(strategy="best-effort"):` lowers to
    /// `asyncio.gather(..., return_exceptions=True)`.
    Gather,
    /// `go` — spawns a background task via `typhon_runtime.tasks.spawn(...)`.
    Go,
    /// `lazy` — prefix for `lazy import X = module`; the module is loaded on
    /// first attribute access rather than at import time.
    Lazy,
}

impl TyphonKeyword {
    /// Return the source spelling of this keyword.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Val => "val",
            Self::Var => "var",
            Self::Model => "model",
            Self::Comptime => "comptime",
            Self::Impl => "impl",
            Self::Extend => "extend",
            Self::Interface => "interface",
            Self::Unsafe => "unsafe",
            Self::Gather => "gather",
            Self::Go => "go",
            Self::Lazy => "lazy",
        }
    }

    /// Try to parse a keyword from a source slice.
    pub fn keyword_of(s: &str) -> Option<Self> {
        match s {
            "val" => Some(Self::Val),
            "var" => Some(Self::Var),
            "model" => Some(Self::Model),
            "comptime" => Some(Self::Comptime),
            "impl" => Some(Self::Impl),
            "extend" => Some(Self::Extend),
            "interface" => Some(Self::Interface),
            "unsafe" => Some(Self::Unsafe),
            "gather" => Some(Self::Gather),
            "go" => Some(Self::Go),
            "lazy" => Some(Self::Lazy),
            _ => None,
        }
    }
}

/// A single token produced by the Typhon lexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TyphonToken<'src> {
    /// A Typhon-specific keyword (`val` / `var`).
    Keyword(TyphonKeyword),
    /// Any other text fragment (passed through unchanged to the Python parser).
    Text(&'src str),
}

/// Lex a Typhon source string into a sequence of [`TyphonToken`]s.
///
/// This is a *word-boundary* lexer: it splits the source on whitespace
/// boundaries, recognises `val` and `var`, and returns everything else as
/// [`TyphonToken::Text`] slices pointing into the original string.
pub fn lex(source: &str) -> Vec<TyphonToken<'_>> {
    let mut tokens = Vec::new();
    let mut start = 0;

    let bytes = source.as_bytes();
    let len = bytes.len();

    while start < len {
        // Skip to the next word-start (non-whitespace, non-special).
        // We scan for potential identifiers: [a-zA-Z_][a-zA-Z0-9_]*
        // Emit any non-identifier bytes as Text fragments directly.
        let ch = bytes[start] as char;
        if ch.is_ascii_alphabetic() || ch == '_' {
            // Scan to end of identifier.
            let mut end = start + 1;
            while end < len && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            let word = &source[start..end];
            if let Some(kw) = TyphonKeyword::keyword_of(word) {
                tokens.push(TyphonToken::Keyword(kw));
            } else {
                tokens.push(TyphonToken::Text(word));
            }
            start = end;
        } else {
            // Emit single non-identifier character as Text.
            // Coalesce consecutive non-identifier characters into one slice.
            let mut end = start + 1;
            while end < len {
                let c = bytes[end] as char;
                if c.is_ascii_alphabetic() || c == '_' {
                    break;
                }
                end += 1;
            }
            tokens.push(TyphonToken::Text(&source[start..end]));
            start = end;
        }
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_val_and_var() {
        let src = "val x: int = 1";
        let tokens = lex(src);
        assert_eq!(tokens[0], TyphonToken::Keyword(TyphonKeyword::Val));
    }

    #[test]
    fn does_not_match_partial_keyword() {
        // "value" should NOT be tokenised as `val` + `ue`.
        let src = "value = 1";
        let tokens = lex(src);
        assert!(
            tokens.iter().all(|t| !matches!(t, TyphonToken::Keyword(_))),
            "partial keyword match should not occur"
        );
    }

    #[test]
    fn var_keyword() {
        let src = "var count: int = 0";
        let tokens = lex(src);
        assert_eq!(tokens[0], TyphonToken::Keyword(TyphonKeyword::Var));
    }
}
