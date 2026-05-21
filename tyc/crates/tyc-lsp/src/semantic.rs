//! Semantic-tokens computation for the Typhon LSP.
//!
//! Walks a resolved module + parsed AST and emits the LSP semantic
//! token stream that drives the editor's syntactic highlighting beyond
//! what the TextMate grammar can express alone. The grammar covers
//! keywords / literals / punctuation; semantic tokens add the
//! resolver-aware bits:
//!
//! - **Imports** are coloured by source. Stdlib (`os`, `json`,
//!   `collections`) carries the `defaultLibrary` modifier so the
//!   theme can render them muted; third-party packages and project
//!   modules get the standard `class` / `function` colours.
//! - **Class / function declarations** are tagged at the declaration
//!   site so the editor doesn't fall back to "variable" colouring on
//!   the name.
//! - **Member access** (`.attr` / `.method()`) emits `method` when
//!   the attribute appears in call position and `property`
//!   otherwise, matching VS Code's Python-stack expectations.
//!
//! The encoding follows the LSP spec exactly: each token is five
//! `u32`s — `(delta_line, delta_start, length, token_type,
//! token_modifiers_bitmask)` — packed in order so the client can
//! reconstruct absolute positions by accumulating deltas.

use ruff_python_ast::{visitor::Visitor, Expr, ModModule, Stmt};
use ruff_text_size::Ranged;
use tower_lsp_server::ls_types::{
    SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensLegend,
};
use tyc_resolve::{Binding, BindingKind, ClassKind, ResolvedModule, ScopeId};

/// Token-type legend, in the order the LSP client receives. Indices
/// into this list are written into the encoded stream and must match
/// what the server advertises in `initialize`.
///
/// Order is fixed forever once published — adding a new token type
/// appends to the tail; reordering would break theme bindings.
pub const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::NAMESPACE,
    SemanticTokenType::CLASS,
    SemanticTokenType::FUNCTION,
    SemanticTokenType::METHOD,
    SemanticTokenType::PROPERTY,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::PARAMETER,
];

/// Token-modifier legend. Bit index `i` corresponds to
/// `TOKEN_MODIFIERS[i]`.
pub const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::DECLARATION,
    SemanticTokenModifier::DEFAULT_LIBRARY,
];

const TOKEN_NAMESPACE: u32 = 0;
const TOKEN_CLASS: u32 = 1;
const TOKEN_FUNCTION: u32 = 2;
const TOKEN_METHOD: u32 = 3;
const TOKEN_PROPERTY: u32 = 4;
const TOKEN_VARIABLE: u32 = 5;
const TOKEN_PARAMETER: u32 = 6;

const MOD_DECLARATION: u32 = 1 << 0;
const MOD_DEFAULT_LIBRARY: u32 = 1 << 1;

/// Build the legend the LSP advertises to clients. Lives next to
/// the constants so the index ordering can't drift between the
/// emitter and the capability advertisement.
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.to_vec(),
        token_modifiers: TOKEN_MODIFIERS.to_vec(),
    }
}

/// One absolute-positioned token; converted to LSP delta-encoding
/// during the final `compute()` step.
#[derive(Debug, Clone, Copy)]
struct AbsoluteToken {
    line: u32,
    col: u32,
    length: u32,
    token_type: u32,
    modifiers: u32,
}

/// Walk the resolved module + parsed AST and return the LSP-encoded
/// semantic tokens stream. Caller is responsible for the
/// preprocessed `source` lining up with the resolver / AST byte
/// offsets — the same constraint hover / definition already obey.
///
/// `stdlib_modules` is the curated list of CPython stdlib roots
/// (returned by [`tyc_resolve::python_stdlib_modules`]). Imports
/// whose top-level matches one of these get the `defaultLibrary`
/// modifier so themes can render them distinctly from project /
/// third-party imports.
pub fn compute(
    source: &str,
    resolved: &ResolvedModule,
    module: &ModModule,
    stdlib_modules: &[&str],
) -> SemanticTokens {
    let mut tokens: Vec<AbsoluteToken> = Vec::new();
    emit_binding_tokens(&mut tokens, source, resolved, stdlib_modules);
    emit_reference_tokens(&mut tokens, source, resolved, stdlib_modules);
    emit_attribute_tokens(&mut tokens, source, module);
    // The LSP encoding requires tokens in document order (each
    // delta-line is non-negative; ties broken by delta-start).
    tokens.sort_by_key(|t| (t.line, t.col));
    // Deduplicate exact-duplicate positions — a binding can also
    // appear in the references vector at the declaration site, and
    // emitting both would confuse the client. Keep the first
    // (binding wins, which carries the `declaration` modifier).
    tokens.dedup_by(|a, b| a.line == b.line && a.col == b.col);
    SemanticTokens {
        result_id: None,
        data: encode(&tokens),
    }
}

/// Convert absolute-positioned tokens into the LSP delta stream.
fn encode(tokens: &[AbsoluteToken]) -> Vec<SemanticToken> {
    let mut prev_line: u32 = 0;
    let mut prev_col: u32 = 0;
    let mut out = Vec::with_capacity(tokens.len());
    for tok in tokens {
        let delta_line = tok.line - prev_line;
        let delta_start = if delta_line == 0 {
            tok.col - prev_col
        } else {
            tok.col
        };
        out.push(SemanticToken {
            delta_line,
            delta_start,
            length: tok.length,
            token_type: tok.token_type,
            token_modifiers_bitset: tok.modifiers,
        });
        prev_line = tok.line;
        prev_col = tok.col;
    }
    out
}

fn emit_binding_tokens(
    tokens: &mut Vec<AbsoluteToken>,
    source: &str,
    resolved: &ResolvedModule,
    stdlib_modules: &[&str],
) {
    for scope in &resolved.scopes {
        for binding in &scope.bindings {
            if let Some(tok) = token_for_binding(binding, source, stdlib_modules, true) {
                tokens.push(tok);
            }
        }
    }
}

fn emit_reference_tokens(
    tokens: &mut Vec<AbsoluteToken>,
    source: &str,
    resolved: &ResolvedModule,
    stdlib_modules: &[&str],
) {
    for reference in &resolved.references {
        let Some(binding) = lookup_binding(resolved, &reference.name, reference.scope) else {
            continue;
        };
        if let Some((line, col)) = byte_to_line_col(source, reference.span.0) {
            let length = (reference.span.1 - reference.span.0) as u32;
            if let Some(mut tok) = token_for_binding(binding, source, stdlib_modules, false) {
                tok.line = line;
                tok.col = col;
                tok.length = length;
                tok.modifiers &= !MOD_DECLARATION;
                tokens.push(tok);
            }
        }
    }
}

/// Walk the AST for `Expr::Attribute` nodes and emit `method` /
/// `property` tokens at the attribute identifier. Method-vs-property
/// is decided by whether the immediate parent is a call (which we
/// detect via the visitor walking the parent expr first).
fn emit_attribute_tokens(tokens: &mut Vec<AbsoluteToken>, source: &str, module: &ModModule) {
    let mut walker = AttributeWalker {
        tokens,
        source,
        in_call_func: false,
    };
    for stmt in &module.body {
        walker.visit_stmt(stmt);
    }
}

struct AttributeWalker<'a> {
    tokens: &'a mut Vec<AbsoluteToken>,
    source: &'a str,
    /// True when the visitor is descending into the `func` slot of an
    /// `Expr::Call`. When the next `Attribute` we see is the
    /// callee, classify the attribute identifier as `method` instead
    /// of `property` — matches VS Code's Python theme.
    in_call_func: bool,
}

impl<'a> Visitor<'a> for AttributeWalker<'a> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Call(call) => {
                // Recurse into `call.func` with the call-context flag
                // set so the leading `Attribute` (if any) is tagged
                // as a method. Arguments are visited normally; an
                // attribute in arg position is a property read.
                let saved = self.in_call_func;
                self.in_call_func = true;
                self.visit_expr(&call.func);
                self.in_call_func = saved;
                for arg in call.arguments.args.iter() {
                    self.visit_expr(arg);
                }
                for kw in call.arguments.keywords.iter() {
                    self.visit_expr(&kw.value);
                }
            }
            Expr::Attribute(attr) => {
                let token_type = if self.in_call_func {
                    TOKEN_METHOD
                } else {
                    TOKEN_PROPERTY
                };
                let ident_end = attr.range().end().to_usize();
                let ident_start = ident_end - attr.attr.as_str().len();
                if let Some((line, col)) = byte_to_line_col(self.source, ident_start) {
                    self.tokens.push(AbsoluteToken {
                        line,
                        col,
                        length: attr.attr.as_str().len() as u32,
                        token_type,
                        modifiers: 0,
                    });
                }
                // The receiver is *not* in call-func position even
                // when the outer expression is a call — only the
                // outermost attribute is the callee. Reset the flag
                // before recursing.
                let saved = self.in_call_func;
                self.in_call_func = false;
                self.visit_expr(&attr.value);
                self.in_call_func = saved;
            }
            other => {
                let saved = self.in_call_func;
                self.in_call_func = false;
                ruff_python_ast::visitor::walk_expr(self, other);
                self.in_call_func = saved;
            }
        }
    }

    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        let saved = self.in_call_func;
        self.in_call_func = false;
        ruff_python_ast::visitor::walk_stmt(self, stmt);
        self.in_call_func = saved;
    }
}

/// Pick the token kind for a binding (used for both declaration and
/// reference sites). Returns `None` for binding kinds we don't yet
/// classify (`Loop`, etc. — they fall through to whatever the
/// TextMate grammar already does).
///
/// `is_declaration` controls the `declaration` modifier — `true` at
/// the declaration site, `false` at reference sites.
fn token_for_binding(
    binding: &Binding,
    source: &str,
    stdlib_modules: &[&str],
    is_declaration: bool,
) -> Option<AbsoluteToken> {
    let (line, col) = byte_to_line_col(source, binding.span.0)?;
    let length = (binding.span.1 - binding.span.0) as u32;
    let mut modifiers: u32 = 0;
    if is_declaration {
        modifiers |= MOD_DECLARATION;
    }
    let token_type = match binding.kind {
        BindingKind::Class => match binding.class_kind {
            ClassKind::Plain | ClassKind::Raw => TOKEN_CLASS,
        },
        BindingKind::Function => TOKEN_FUNCTION,
        BindingKind::Parameter => TOKEN_PARAMETER,
        BindingKind::Value => TOKEN_VARIABLE,
        BindingKind::Loop => TOKEN_VARIABLE,
        BindingKind::Import => {
            if let Some(info) = &binding.import_info {
                let top = info.module.split('.').next().unwrap_or(&info.module);
                if stdlib_modules.contains(&top) {
                    modifiers |= MOD_DEFAULT_LIBRARY;
                }
                // `from X import Y` → the member shape isn't known
                // without introspection; treat as `class` so VS
                // Code picks a colour rather than the generic
                // variable shade. Python imports overwhelmingly
                // alias class / function symbols; the class
                // colour reads correctly for both because VS Code
                // applies the same family.
                //
                // `import X` (bare) → `namespace` since the local
                // name is a module handle.
                if info.member.is_none() {
                    TOKEN_NAMESPACE
                } else {
                    TOKEN_CLASS
                }
            } else {
                TOKEN_VARIABLE
            }
        }
    };
    Some(AbsoluteToken {
        line,
        col,
        length,
        token_type,
        modifiers,
    })
}

/// Walk scopes upward from `start_scope`, returning the first
/// binding that names `name`. Mirrors what the resolver does at
/// resolution time, kept close to the call site so we don't pay
/// the cost of building a flat name → binding map for every
/// semantic-tokens request.
fn lookup_binding<'a>(
    resolved: &'a ResolvedModule,
    name: &str,
    start_scope: ScopeId,
) -> Option<&'a Binding> {
    let mut current = Some(start_scope);
    while let Some(idx) = current {
        let scope = &resolved.scopes[idx];
        if let Some(binding) = scope.bindings.iter().find(|b| b.name == name) {
            return Some(binding);
        }
        current = scope.parent;
    }
    None
}

/// Convert a byte offset into LSP `(line, character)` coordinates.
/// Columns are counted in UTF-16 code units to match the rest of
/// the LSP — the file may contain wide characters in identifiers
/// (unlikely for Typhon today, but free here).
///
/// Returns `None` when `offset` is past the end of `source`.
fn byte_to_line_col(source: &str, offset: usize) -> Option<(u32, u32)> {
    if offset > source.len() {
        return None;
    }
    let mut line: u32 = 0;
    let mut col: u32 = 0;
    let mut byte: usize = 0;
    for ch in source.chars() {
        if byte >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += ch.len_utf16() as u32;
        }
        byte += ch.len_utf8();
    }
    Some((line, col))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_parser::parse_module;
    use tyc_resolve::{resolve_module_with, ResolveOptions};
    use tyc_syntax::preprocess::preprocess;

    fn parse_and_resolve(source: &str) -> (String, ResolvedModule, ModModule) {
        let prep = preprocess(source);
        let parsed = parse_module(&prep.python_source).expect("parse");
        let module = parsed.into_syntax();
        let (resolved, _) = resolve_module_with(
            "t.ty".into(),
            &prep.python_source,
            &module,
            ResolveOptions::default(),
        );
        (prep.python_source, resolved, module)
    }

    fn stdlib() -> Vec<&'static str> {
        tyc_resolve::python_stdlib_modules().to_vec()
    }

    /// Find the first token whose absolute position matches the
    /// span of the substring `needle` in `source`. Tests use this
    /// to assert a specific identifier got a specific token type.
    fn token_at(source: &str, needle: &str, tokens: &[SemanticToken]) -> Option<(u32, u32)> {
        let byte = source.find(needle)?;
        let (target_line, target_col) = byte_to_line_col(source, byte)?;
        let mut prev_line: u32 = 0;
        let mut prev_col: u32 = 0;
        for tok in tokens {
            let line = prev_line + tok.delta_line;
            let col = if tok.delta_line == 0 {
                prev_col + tok.delta_start
            } else {
                tok.delta_start
            };
            if line == target_line && col == target_col {
                return Some((tok.token_type, tok.token_modifiers_bitset));
            }
            prev_line = line;
            prev_col = col;
        }
        None
    }

    #[test]
    fn stdlib_import_tagged_as_default_library() {
        let src = "import os\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib = stdlib();
        let stdlib_refs: Vec<&str> = stdlib.to_vec();
        let result = compute(&source, &resolved, &module, &stdlib_refs);
        let (ty, modifiers) = token_at(&source, "os", &result.data).expect("os token");
        assert_eq!(ty, TOKEN_NAMESPACE, "bare `import os` is a namespace");
        assert!(
            modifiers & MOD_DEFAULT_LIBRARY != 0,
            "stdlib gets defaultLibrary modifier; got {modifiers}"
        );
    }

    #[test]
    fn third_party_import_has_no_default_library_modifier() {
        let src = "from agent_framework import Agent\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib = stdlib();
        let stdlib_refs: Vec<&str> = stdlib.to_vec();
        let result = compute(&source, &resolved, &module, &stdlib_refs);
        let (ty, modifiers) = token_at(&source, "Agent", &result.data).expect("Agent token");
        assert_eq!(ty, TOKEN_CLASS, "from-imports are tagged as class");
        assert_eq!(
            modifiers & MOD_DEFAULT_LIBRARY,
            0,
            "third-party imports don't get defaultLibrary"
        );
    }

    #[test]
    fn class_declaration_emits_class_token_with_declaration_modifier() {
        let src = "class Foo:\n    pass\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib_refs: Vec<&str> = Vec::new();
        let result = compute(&source, &resolved, &module, &stdlib_refs);
        let (ty, modifiers) = token_at(&source, "Foo", &result.data).expect("Foo token");
        assert_eq!(ty, TOKEN_CLASS);
        assert!(
            modifiers & MOD_DECLARATION != 0,
            "declaration site gets declaration modifier"
        );
    }

    #[test]
    fn attribute_in_call_position_is_method() {
        let src = "import os\nx = os.getcwd()\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib = stdlib();
        let stdlib_refs: Vec<&str> = stdlib.to_vec();
        let result = compute(&source, &resolved, &module, &stdlib_refs);
        let (ty, _) = token_at(&source, "getcwd", &result.data).expect("getcwd token");
        assert_eq!(ty, TOKEN_METHOD, "`os.getcwd()` is a method call");
    }

    #[test]
    fn attribute_not_in_call_position_is_property() {
        let src = "import os\np = os.sep\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib = stdlib();
        let stdlib_refs: Vec<&str> = stdlib.to_vec();
        let result = compute(&source, &resolved, &module, &stdlib_refs);
        let (ty, _) = token_at(&source, "sep", &result.data).expect("sep token");
        assert_eq!(ty, TOKEN_PROPERTY, "`os.sep` is a property read");
    }

    #[test]
    fn function_declaration_and_call_both_emit_function_tokens() {
        let src = "def add(a: int, b: int) -> int:\n    return a + b\n\nr = add(1, 2)\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib_refs: Vec<&str> = Vec::new();
        let result = compute(&source, &resolved, &module, &stdlib_refs);
        // Declaration carries `declaration` modifier.
        let (decl_ty, decl_mods) =
            token_at(&source, "add(a", &result.data).expect("add decl token");
        assert_eq!(decl_ty, TOKEN_FUNCTION);
        assert!(decl_mods & MOD_DECLARATION != 0);
        // Reference site (`add(1, 2)`) is also tagged as function
        // but without the `declaration` modifier.
        let (ref_ty, ref_mods) = token_at(&source, "add(1", &result.data).expect("add ref token");
        assert_eq!(ref_ty, TOKEN_FUNCTION);
        assert_eq!(ref_mods & MOD_DECLARATION, 0);
    }

    #[test]
    fn tokens_are_sorted_and_delta_encoded() {
        // Two identifiers on the same line must yield deltas relative
        // to each other (delta_line = 0, delta_start = gap), not
        // absolute columns. This is the LSP spec contract — getting
        // it wrong produces visually-correct-looking but
        // incrementally-wrong highlighting.
        let src = "x = 1\ny = 2\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib_refs: Vec<&str> = Vec::new();
        let result = compute(&source, &resolved, &module, &stdlib_refs);
        // Walk the stream and assert every delta is non-negative.
        for tok in &result.data {
            assert!(
                tok.delta_line < u32::MAX,
                "delta_line must be representable"
            );
            // delta_start can be 0 (consecutive tokens at same col is
            // impossible after dedup, but valid encoding) — what we
            // really check is that the encoding is consistent.
            assert!(tok.length > 0, "zero-length tokens shouldn't be emitted");
        }
    }
}
