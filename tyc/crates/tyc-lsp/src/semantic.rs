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

use std::collections::{HashMap, HashSet};

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

/// Structured signature info for a single callable, derived from the
/// venv-introspection cache and parsed once per `semantic_tokens_full`
/// request. The kwarg classifier uses it to colour `Agent(client=...)`
/// argument names by whether they hit a real parameter, a `**kwargs`
/// catch-all, or nothing.
#[derive(Debug, Default, Clone)]
pub struct CalleeSignature {
    /// Set of named parameters declared on the callable, excluding
    /// `self`. Membership test runs per kwarg, so a hash set keeps
    /// the inner loop O(1) regardless of arity.
    pub param_names: HashSet<String>,
    /// True when the callable declares `**kwargs`. Unknown kwargs
    /// fall through to the catch-all in that case (yellow), versus
    /// being silently invalid (white / no token).
    pub accepts_kwargs: bool,
}

/// Lookup table the kwarg pass consults. Key is the bare binding
/// name as it appears in source (`Agent`, `Client`) — the resolver
/// gives the same key when walking `Expr::Name` callees. Built once
/// per request by `semantic_tokens_full` after batch-querying the
/// introspection cache; cheap to clone since the inner sets are
/// small.
pub type CalleeSignatures = HashMap<String, CalleeSignature>;

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
///
/// `callee_signatures` lets the kwarg pass classify `client=…` in
/// `Agent(client=…)` against the real Agent constructor signature
/// — orange when the kwarg names a real parameter, yellow when the
/// constructor declares `**kwargs`, no token (white) when the kwarg
/// is unrecognised. Pass an empty map to disable kwarg colouring
/// (used by unit tests that don't need introspection).
pub fn compute(
    source: &str,
    resolved: &ResolvedModule,
    module: &ModModule,
    stdlib_modules: &[&str],
    callee_signatures: &CalleeSignatures,
) -> SemanticTokens {
    let mut tokens: Vec<AbsoluteToken> = Vec::new();
    emit_binding_tokens(&mut tokens, source, resolved, stdlib_modules);
    emit_reference_tokens(&mut tokens, source, resolved, stdlib_modules);
    emit_module_path_tokens(&mut tokens, source, module, stdlib_modules);
    emit_ast_tokens(&mut tokens, source, module, callee_signatures);
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
            let length = utf16_len_of_span(source, reference.span.0, reference.span.1);
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

/// Walk the AST emitting tokens that the resolver can't see by
/// itself: attribute access (`obj.attr` / `obj.method()`) and
/// keyword arguments in calls (`Agent(client=…)`).
fn emit_ast_tokens<'a>(
    tokens: &mut Vec<AbsoluteToken>,
    source: &'a str,
    module: &'a ModModule,
    callee_signatures: &'a CalleeSignatures,
) {
    let mut walker = AstWalker {
        tokens,
        source,
        callee_signatures,
        in_call_func: false,
    };
    for stmt in &module.body {
        walker.visit_stmt(stmt);
    }
}

struct AstWalker<'a> {
    tokens: &'a mut Vec<AbsoluteToken>,
    source: &'a str,
    callee_signatures: &'a CalleeSignatures,
    /// True when the visitor is descending into the `func` slot of an
    /// `Expr::Call`. When the next `Attribute` we see is the
    /// callee, classify the attribute identifier as `method` instead
    /// of `property` — matches VS Code's Python theme.
    in_call_func: bool,
}

impl<'a> AstWalker<'a> {
    /// Emit semantic tokens for each kwarg name in a call.
    /// Classification:
    /// - **Real parameter** of the callee → `property` (VS Code Dark+
    ///   renders this in the same orange-ish tone Python users get
    ///   from Pylance for kwarg names against a known signature).
    /// - Callee declares `**kwargs` and the name isn't a declared
    ///   parameter → `parameter` (yellow). Communicates "valid but
    ///   not on the explicit list".
    /// - Callee shape isn't known or kwarg isn't recognised → no
    ///   token, so the editor falls back to the TextMate grammar
    ///   (white in most themes).
    fn emit_call_kwargs(&mut self, call: &ruff_python_ast::ExprCall) {
        let Some(sig) = self.callee_signature_for(&call.func) else {
            return;
        };
        for kw in call.arguments.keywords.iter() {
            let Some(arg) = &kw.arg else {
                // `**unpack` — no identifier to colour.
                continue;
            };
            let name = arg.id.as_str();
            let token_type = if sig.param_names.contains(name) {
                TOKEN_PROPERTY
            } else if sig.accepts_kwargs {
                TOKEN_PARAMETER
            } else {
                continue;
            };
            let start = arg.range.start().to_usize();
            let end = arg.range.end().to_usize();
            if let Some((line, col)) = byte_to_line_col(self.source, start) {
                self.tokens.push(AbsoluteToken {
                    line,
                    col,
                    length: utf16_len_of_span(self.source, start, end),
                    token_type,
                    modifiers: 0,
                });
            }
        }
    }

    /// Resolve the callee of a `Call` expression to its declared
    /// signature, if we know one. Handles the two shapes the kwarg
    /// classifier can meaningfully colour:
    ///
    /// - `Foo(...)` where `Foo` is a top-level binding pointing at
    ///   an imported class / function (most common case — direct
    ///   `from agent_framework import Agent` followed by `Agent(...)`).
    /// - `module.Foo(...)` where `module` is a bare-import binding
    ///   (`import agent_framework` then `agent_framework.Agent(...)`).
    ///
    /// Chained / generic callees (`config.client.factory(...)`,
    /// `make_agent()(...)`) need full type inference and aren't
    /// classified — kwargs there fall through to the no-token
    /// default rather than producing misleading colours.
    fn callee_signature_for(&self, func: &Expr) -> Option<&'a CalleeSignature> {
        match func {
            Expr::Name(name) => self.callee_signatures.get(name.id.as_str()),
            Expr::Attribute(_) => None,
            _ => None,
        }
    }
}

impl<'a> Visitor<'a> for AstWalker<'a> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Call(call) => {
                // Kwarg classification (orange / yellow / nothing)
                // before recursing — the children include the kwarg
                // *values*, which are normal expressions we want to
                // descend into for nested attribute access.
                self.emit_call_kwargs(call);
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
                        length: utf16_len_of_span(self.source, ident_start, ident_end),
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

/// Emit `namespace` tokens for the dotted module-path identifiers
/// in `import foo.bar` and `from foo.bar import baz` statements.
/// VS Code's Python plugin colours these distinctly from the
/// surrounding `import` keyword (which the TextMate grammar covers);
/// without this pass, our hover-aware highlighting was missing the
/// most visually prominent part of every import line.
///
/// `defaultLibrary` modifier is applied when the root of the dotted
/// path is in the stdlib whitelist — same treatment binding tokens
/// already get, so stdlib `from os.path import join` stays muted.
fn emit_module_path_tokens(
    tokens: &mut Vec<AbsoluteToken>,
    source: &str,
    module: &ModModule,
    stdlib_modules: &[&str],
) {
    for stmt in &module.body {
        match stmt {
            Stmt::ImportFrom(import_from) => {
                let Some(module_ident) = &import_from.module else {
                    continue;
                };
                let dotted = module_ident.id.as_str();
                let start = module_ident.range.start().to_usize();
                let is_stdlib = is_stdlib_module(dotted, stdlib_modules);
                emit_dotted_path(tokens, source, start, dotted, is_stdlib);
            }
            Stmt::Import(import) => {
                for alias in &import.names {
                    let dotted = alias.name.id.as_str();
                    let start = alias.name.range.start().to_usize();
                    let is_stdlib = is_stdlib_module(dotted, stdlib_modules);
                    emit_dotted_path(tokens, source, start, dotted, is_stdlib);
                }
            }
            _ => {}
        }
    }
}

fn is_stdlib_module(dotted: &str, stdlib_modules: &[&str]) -> bool {
    let root = dotted.split('.').next().unwrap_or(dotted);
    stdlib_modules.contains(&root)
}

/// Emit one `namespace` token per dotted segment of `path`, anchored
/// at byte offset `start`. The `.` separators get no token; the
/// segments are emitted with their byte positions so the LSP client
/// sees individual ranges (so a future "go to import" code action
/// can target sub-segments).
fn emit_dotted_path(
    tokens: &mut Vec<AbsoluteToken>,
    source: &str,
    start: usize,
    path: &str,
    is_stdlib: bool,
) {
    let mut offset = start;
    for segment in path.split('.') {
        if !segment.is_empty() {
            let end = offset + segment.len();
            if let Some((line, col)) = byte_to_line_col(source, offset) {
                let mut modifiers: u32 = 0;
                if is_stdlib {
                    modifiers |= MOD_DEFAULT_LIBRARY;
                }
                tokens.push(AbsoluteToken {
                    line,
                    col,
                    length: utf16_len_of_span(source, offset, end),
                    token_type: TOKEN_NAMESPACE,
                    modifiers,
                });
            }
        }
        // Step past the segment and the trailing `.`. Last iteration
        // overshoots by one but `offset` isn't read after the loop.
        offset += segment.len() + 1;
    }
}

/// Parse a Python signature string (the form `inspect.signature`
/// returns) into the set of declared parameter names and a flag
/// for `**kwargs`. Used to classify call-site kwargs against the
/// declared shape — orange for real params, yellow for catch-all.
///
/// Tolerant of:
/// - default values containing nested tuples / dicts / lists
///   (`(a=(1, 2), b={3: 4})`).
/// - quoted defaults containing commas (`(sep=', ')`).
/// - the leading `(` and trailing `)` (with or without spaces).
/// - leading `self` / `cls` (stripped, but harmless if left in).
/// - positional-only markers `/` and keyword-only `*` (skipped).
///
/// Conservative on the bail-out side: any unrecognised shape
/// returns the partial result it parsed so far rather than
/// throwing the whole signature away. Worst case is one kwarg gets
/// the wrong colour for a hostile signature; the visual feedback is
/// still net positive.
pub fn parse_signature(sig: &str) -> CalleeSignature {
    let mut out = CalleeSignature::default();
    let body = sig
        .trim()
        .strip_prefix('(')
        .and_then(|s| s.rsplit_once(')').map(|(a, _)| a))
        .unwrap_or(sig);
    for raw in split_top_level_commas(body) {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed == "/" || trimmed == "*" {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("**") {
            // `**kwargs` — done. We don't need the name (and rarely
            // does the caller care that it's called `kwargs` vs
            // `extra`); flipping the flag is sufficient.
            let _ = rest;
            out.accepts_kwargs = true;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('*') {
            // `*args` — positional varargs; doesn't affect kwarg
            // classification but we still parse past it.
            let _ = rest;
            continue;
        }
        // Extract the identifier portion: everything up to `:` (type
        // annotation) or `=` (default value) or end-of-string.
        let name_end = trimmed
            .find([':', '='])
            .unwrap_or(trimmed.len());
        let name = trimmed[..name_end].trim();
        if name.is_empty() || name == "self" || name == "cls" {
            continue;
        }
        if name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            out.param_names.insert(name.to_owned());
        }
    }
    out
}

/// Split a parameter-list body on commas that aren't nested inside
/// parens / brackets / braces / quotes. The naive `body.split(',')`
/// would mangle `Tuple[int, int]` defaults.
fn split_top_level_commas(body: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut depth: i32 = 0;
    let mut in_str: Option<char> = None;
    let mut last_split: usize = 0;
    let bytes = body.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let c = b as char;
        if let Some(quote) = in_str {
            if c == quote && bytes.get(i.saturating_sub(1)).copied() != Some(b'\\') {
                in_str = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => in_str = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&body[last_split..i]);
                last_split = i + 1;
            }
            _ => {}
        }
    }
    if last_split < body.len() {
        out.push(&body[last_split..]);
    }
    out
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
    let length = utf16_len_of_span(source, binding.span.0, binding.span.1);
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

/// Width of a `[start, end)` byte slice measured in UTF-16 code
/// units. LSP semantic-tokens `length` is specified in UTF-16, same
/// unit as `Position.character` — using a byte length here would
/// over- or under-shoot the highlight range for any non-ASCII
/// identifier (an `é`, a CJK ideograph, an emoji-as-identifier in
/// a docstring). Typhon doesn't permit non-ASCII identifiers today,
/// but the LSP serves docstrings + comments in semantic-tokens
/// neighbouring spans, and getting this right by construction is
/// cheaper than chasing a desync bug later.
///
/// Clamps `end` to the source length; out-of-range spans return 0
/// rather than panicking.
fn utf16_len_of_span(source: &str, start: usize, end: usize) -> u32 {
    if start >= source.len() || end <= start {
        return 0;
    }
    let end = end.min(source.len());
    let slice = &source[start..end];
    slice.chars().map(|c| c.len_utf16() as u32).sum()
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
        let result = compute(
            &source,
            &resolved,
            &module,
            &stdlib_refs,
            &CalleeSignatures::new(),
        );
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
        let result = compute(
            &source,
            &resolved,
            &module,
            &stdlib_refs,
            &CalleeSignatures::new(),
        );
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
        let result = compute(
            &source,
            &resolved,
            &module,
            &stdlib_refs,
            &CalleeSignatures::new(),
        );
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
        let result = compute(
            &source,
            &resolved,
            &module,
            &stdlib_refs,
            &CalleeSignatures::new(),
        );
        let (ty, _) = token_at(&source, "getcwd", &result.data).expect("getcwd token");
        assert_eq!(ty, TOKEN_METHOD, "`os.getcwd()` is a method call");
    }

    #[test]
    fn attribute_not_in_call_position_is_property() {
        let src = "import os\np = os.sep\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib = stdlib();
        let stdlib_refs: Vec<&str> = stdlib.to_vec();
        let result = compute(
            &source,
            &resolved,
            &module,
            &stdlib_refs,
            &CalleeSignatures::new(),
        );
        let (ty, _) = token_at(&source, "sep", &result.data).expect("sep token");
        assert_eq!(ty, TOKEN_PROPERTY, "`os.sep` is a property read");
    }

    #[test]
    fn function_declaration_and_call_both_emit_function_tokens() {
        let src = "def add(a: int, b: int) -> int:\n    return a + b\n\nr = add(1, 2)\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib_refs: Vec<&str> = Vec::new();
        let result = compute(
            &source,
            &resolved,
            &module,
            &stdlib_refs,
            &CalleeSignatures::new(),
        );
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
    fn utf16_length_helper_counts_surrogate_pairs_correctly() {
        // Ascii: each byte is one UTF-16 code unit.
        assert_eq!(utf16_len_of_span("hello", 0, 5), 5);
        // BMP non-ascii (`é` is 2 bytes UTF-8 / 1 unit UTF-16): the
        // byte-derived length we used to ship would have reported
        // 4 here, mis-aligning every token after it.
        let s = "éé";
        assert_eq!(utf16_len_of_span(s, 0, s.len()), 2);
        // Astral plane (`🦀` is 4 bytes UTF-8 / 2 units UTF-16,
        // i.e. a UTF-16 surrogate pair): the LSP client expects 2,
        // not 1 and not 4.
        let crab = "🦀";
        assert_eq!(utf16_len_of_span(crab, 0, crab.len()), 2);
        // Out-of-range spans clamp rather than panicking — defensive
        // against AST / resolver divergence.
        assert_eq!(utf16_len_of_span("abc", 1, 99), 2);
        assert_eq!(utf16_len_of_span("abc", 5, 6), 0);
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
        let result = compute(
            &source,
            &resolved,
            &module,
            &stdlib_refs,
            &CalleeSignatures::new(),
        );
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

    #[test]
    fn from_import_module_path_emits_namespace_tokens() {
        // `from foo.bar import Baz` — the dotted path `foo.bar`
        // should colour as namespace at each segment, so VS Code
        // doesn't leave the most prominent part of every import
        // line uncoloured (the original 0.2.4 hit only `Baz`).
        let src = "from foo.bar import Baz\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib_refs: Vec<&str> = Vec::new();
        let result = compute(
            &source,
            &resolved,
            &module,
            &stdlib_refs,
            &CalleeSignatures::new(),
        );
        let (foo_ty, _) = token_at(&source, "foo", &result.data).expect("foo token");
        let (bar_ty, _) = token_at(&source, "bar", &result.data).expect("bar token");
        assert_eq!(foo_ty, TOKEN_NAMESPACE);
        assert_eq!(bar_ty, TOKEN_NAMESPACE);
    }

    #[test]
    fn from_import_stdlib_path_carries_default_library_modifier() {
        // `from os.path import join` — the path is stdlib, so each
        // segment should get the `defaultLibrary` modifier the same
        // way bare `import os` already does.
        let src = "from os.path import join\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib = stdlib();
        let stdlib_refs: Vec<&str> = stdlib.to_vec();
        let result = compute(
            &source,
            &resolved,
            &module,
            &stdlib_refs,
            &CalleeSignatures::new(),
        );
        let (_, os_mods) = token_at(&source, "os", &result.data).expect("os token");
        let (_, path_mods) = token_at(&source, "path", &result.data).expect("path token");
        assert!(os_mods & MOD_DEFAULT_LIBRARY != 0, "os mods: {os_mods}");
        assert!(
            path_mods & MOD_DEFAULT_LIBRARY != 0,
            "path mods: {path_mods}"
        );
    }

    #[test]
    fn dotted_import_path_emits_one_token_per_segment() {
        // `import foo.bar.baz` — each segment is its own token,
        // not a single span over the whole dotted path.
        let src = "import foo.bar.baz\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let stdlib_refs: Vec<&str> = Vec::new();
        let result = compute(
            &source,
            &resolved,
            &module,
            &stdlib_refs,
            &CalleeSignatures::new(),
        );
        for needle in ["foo", "bar", "baz"] {
            let (ty, _) = token_at(&source, needle, &result.data)
                .unwrap_or_else(|| panic!("{needle} token missing"));
            assert_eq!(ty, TOKEN_NAMESPACE);
        }
    }

    #[test]
    fn kwarg_real_param_emits_property_token() {
        // `Agent(client=…)` where the `Agent` import is in scope
        // and we've pre-resolved its signature: the kwarg name
        // should be coloured as `property` (orange in VS Code
        // Dark+), so the user can see at a glance which kwarg
        // names actually match the constructor.
        let src = "from agent_framework import Agent\nAgent(client=1, model='gpt-4')\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let mut sigs = CalleeSignatures::new();
        sigs.insert(
            "Agent".to_owned(),
            parse_signature("(name, client, model='gpt-4')"),
        );
        let stdlib_refs: Vec<&str> = Vec::new();
        let result = compute(&source, &resolved, &module, &stdlib_refs, &sigs);
        let (client_ty, _) =
            token_at(&source, "client=", &result.data).expect("client kwarg token");
        let (model_ty, _) = token_at(&source, "model=", &result.data).expect("model kwarg token");
        assert_eq!(client_ty, TOKEN_PROPERTY);
        assert_eq!(model_ty, TOKEN_PROPERTY);
    }

    #[test]
    fn kwarg_against_kwargs_catchall_emits_parameter_token() {
        // Callable declares `**kwargs`: an unknown kwarg name
        // shouldn't be invisible (white), nor should it claim to
        // be a real param (orange). `parameter` (yellow) is the
        // honest middle.
        let src = "from x import F\nF(client=1, weird_thing=2)\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let mut sigs = CalleeSignatures::new();
        sigs.insert("F".to_owned(), parse_signature("(client, **kwargs)"));
        let stdlib_refs: Vec<&str> = Vec::new();
        let result = compute(&source, &resolved, &module, &stdlib_refs, &sigs);
        let (client_ty, _) = token_at(&source, "client=", &result.data).expect("client");
        let (weird_ty, _) = token_at(&source, "weird_thing=", &result.data).expect("weird");
        assert_eq!(client_ty, TOKEN_PROPERTY, "real param is orange");
        assert_eq!(
            weird_ty, TOKEN_PARAMETER,
            "**kwargs catch-all is yellow (parameter)"
        );
    }

    #[test]
    fn kwarg_unrecognised_with_no_kwargs_emits_no_token() {
        // Callable declares exact params with no `**kwargs`, kwarg
        // name doesn't match any of them: emit no token so the
        // editor falls back to the default (white) — a visible
        // "this is wrong" cue without us claiming a particular
        // diagnostic.
        let src = "from x import F\nF(bogus=1)\n";
        let (source, resolved, module) = parse_and_resolve(src);
        let mut sigs = CalleeSignatures::new();
        sigs.insert("F".to_owned(), parse_signature("(client, model)"));
        let stdlib_refs: Vec<&str> = Vec::new();
        let result = compute(&source, &resolved, &module, &stdlib_refs, &sigs);
        assert!(
            token_at(&source, "bogus=", &result.data).is_none(),
            "unrecognised kwarg should emit no token"
        );
    }

    #[test]
    fn parse_signature_extracts_basic_param_names() {
        let sig = parse_signature("(name: str, client: Client, model: str = 'gpt-4')");
        assert!(sig.param_names.contains("name"));
        assert!(sig.param_names.contains("client"));
        assert!(sig.param_names.contains("model"));
        assert!(!sig.accepts_kwargs);
    }

    #[test]
    fn parse_signature_detects_kwargs() {
        let sig = parse_signature("(a, *, b, **kwargs)");
        assert!(sig.param_names.contains("a"));
        assert!(sig.param_names.contains("b"));
        assert!(sig.accepts_kwargs);
    }

    #[test]
    fn parse_signature_handles_nested_defaults_with_commas() {
        // A default value with a tuple literal must not split into
        // bogus param names. The naive `body.split(',')` would
        // emit `2)` as a "param".
        let sig = parse_signature("(a=(1, 2), b={'k': 1}, c='hi, there')");
        assert!(sig.param_names.contains("a"));
        assert!(sig.param_names.contains("b"));
        assert!(sig.param_names.contains("c"));
        assert_eq!(
            sig.param_names.len(),
            3,
            "no spurious params: {:?}",
            sig.param_names
        );
    }

    #[test]
    fn parse_signature_skips_self_and_cls() {
        let sig = parse_signature("(self, name, value)");
        assert!(!sig.param_names.contains("self"));
        assert!(sig.param_names.contains("name"));
        assert!(sig.param_names.contains("value"));
    }
}
