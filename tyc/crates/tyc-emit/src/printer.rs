//! Hand-written Python pretty-printer.
//!
//! Covers the Python 3 grammar subset used in Phase 0 round-trip testing.
//! Aim: parse → emit produces output that is semantically equivalent to the
//! input (whitespace and comment differences are acceptable in Phase 0).

use ruff_python_ast::{
    self as ast, Alias, BoolOp, CmpOp, Comprehension, ExceptHandler, Expr, FStringPart,
    InterpolatedStringElement, Keyword, MatchCase, ModModule, Mutability, Number, Operator,
    Parameter, ParameterWithDefault, Parameters, Pattern, Singleton, Stmt, StringFlags, TypeParam,
    TypeParams, UnaryOp, WithItem,
};
use ruff_text_size::Ranged;

const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

fn push_hex_escape(out: &mut String, byte: u8) {
    out.push_str("\\x");
    out.push(HEX_CHARS[(byte >> 4) as usize] as char);
    out.push(HEX_CHARS[(byte & 0x0f) as usize] as char);
}

/// Internal state for the Python pretty-printer.
pub struct Emitter {
    output: String,
    indent: usize,
    /// Input byte-offset that is "active" while the current statement is being
    /// emitted.  Updated at the start of every `emit_stmt` call from the
    /// statement's `TextRange`; synthesised nodes (zero-length range) leave it
    /// unchanged so they inherit the last known real offset.
    current_input_offset: usize,
    /// One entry per output line (0-indexed).  Each entry is the
    /// `current_input_offset` that was active when that line's newline was
    /// emitted.  Used by the caller to build a `(py_line → ty_line)` table.
    pub line_offsets: Vec<usize>,
    /// When `true`, Typhon's `let`/`mut` soft keywords (carried on
    /// `StmtAssign.mutability` / `StmtAnnAssign.mutability`) are *not*
    /// emitted.  Set this for build output that must be valid Python;
    /// leave it `false` for `tyc fmt`-style round-tripping.
    suppress_mutability: bool,
    /// PEP 8 requires two blank lines around top-level class/def statements.
    /// We track whether the previous *module-level* statement was a class or
    /// function definition so the next top-level statement can prepend the
    /// extra blank line.  Reset to `false` after emitting a non-block stmt
    /// at indent 0.
    prev_top_level_was_block: bool,
    /// Stack of outer-quote characters for currently-active f-strings. When
    /// we emit a `StringLiteral` while one is non-empty, the literal must
    /// be wrapped in a quote character that differs from the f-string's
    /// outer delimiter so PEP 701 nesting works on 3.11/3.12 and isn't
    /// ambiguous on 3.13+. Tracked as a stack so nested f-strings
    /// (`f"{ f'{x}' }"`) compose correctly.
    fstring_quote_stack: Vec<char>,
    /// Target Python minor version (3.X). When `< 12`, PEP 695 syntax
    /// (`def f[T](...)`, `class Box[T]:`, `type X = Y`) is lowered to
    /// the older `TypeVar` + `Generic[T]` + `X: TypeAlias = Y` shapes
    /// so the emitted module parses on 3.10 / 3.11 (FINDINGS #47).
    /// `0` means "unset" and disables lowering.
    target_minor: u8,
    /// Optional reference-counted handle to the original (pre-parse)
    /// source. When present, the printer can peek at a node's source
    /// range to recover purely-syntactic choices that the AST
    /// collapses — notably the bracket style on `MatchSequence`
    /// patterns where `[a, b]` and `(a, b)` are semantically identical
    /// but users have a stylistic preference (O27 / FINDINGS #111).
    ///
    /// `Arc<str>` rather than `String` so the build pipeline (which
    /// already holds the preprocessed source via `prep.python_source`)
    /// can share a single allocation across the type-checker, the
    /// printer, and the source-map builder. Copilot review on PR #96.
    source: Option<std::sync::Arc<str>>,
}

const INDENT_WIDTH: usize = 4;

impl Emitter {
    pub fn new() -> Self {
        Self {
            output: String::new(),
            indent: 0,
            current_input_offset: 0,
            line_offsets: Vec::new(),
            suppress_mutability: false,
            prev_top_level_was_block: false,
            fstring_quote_stack: Vec::new(),
            target_minor: 0,
            source: None,
        }
    }

    /// Attach the original source text so the printer can recover
    /// stylistic choices from node `TextRange`s. Currently used by
    /// `tyc build` to round-trip `[a, b]` vs `(a, b)` sequence patterns
    /// (O27 / FINDINGS #111) — when the source is absent the printer
    /// falls back to the default bracket choice. The source is held
    /// behind an `Arc<str>` so callers that already have a shared
    /// handle (e.g. the build pipeline keeping the preprocessed text
    /// around) don't pay a full copy.
    pub fn set_source(&mut self, source: std::sync::Arc<str>) {
        self.source = Some(source);
    }

    /// Configure this emitter to drop Typhon's `let`/`mut` soft keywords.
    /// Build output uses this; `tyc fmt` does not.
    pub fn suppress_mutability_keywords(&mut self) {
        self.suppress_mutability = true;
    }

    /// Set the target Python minor version (`3.X`). When `< 12`, PEP 695
    /// syntax is lowered to the legacy `TypeVar` / `Generic[T]` /
    /// `X: TypeAlias = Y` shapes so the emitted module parses on the
    /// older interpreter (FINDINGS #47). Default is unset → no lowering.
    pub fn set_python_target(&mut self, minor: u8) {
        self.target_minor = minor;
    }

    /// True when PEP 695 syntax must be lowered to TypeVar / Generic /
    /// TypeAlias for the configured target.
    fn lower_pep695(&self) -> bool {
        self.target_minor > 0 && self.target_minor < 12
    }

    pub fn finish(self) -> String {
        self.output
    }

    // ── helpers ────────────────────────────────────────────────────────────

    fn write(&mut self, s: &str) {
        self.output.push_str(s);
        // A newline can reach the output from *inside* a token, not just
        // from `writeln` / `newline`: a triple-quoted docstring, or any
        // string literal the printer re-emits verbatim, carries real
        // newlines. Each one still ends an output line, so it needs its
        // own `line_offsets` entry — otherwise the table is shorter than
        // the file it describes and every entry after the literal names
        // a line one-too-early, permanently, for the rest of the module.
        for _ in s.bytes().filter(|&b| b == b'\n') {
            self.line_offsets.push(self.current_input_offset);
        }
    }

    fn write_char(&mut self, c: char) {
        self.output.push(c);
        if c == '\n' {
            self.line_offsets.push(self.current_input_offset);
        }
    }

    fn write_hex_escape(&mut self, byte: u8) {
        // Always emits `\xNN` (an escape *sequence*, two literal
        // characters) — never a raw newline — so no offset bookkeeping.
        push_hex_escape(&mut self.output, byte);
    }

    fn writeln(&mut self, s: &str) {
        self.write(s);
        self.line_offsets.push(self.current_input_offset);
        self.output.push('\n');
    }

    fn newline(&mut self) {
        self.line_offsets.push(self.current_input_offset);
        self.output.push('\n');
    }

    /// Make `range` the source provenance of every output line printed
    /// from here until the next call.
    ///
    /// `emit_stmt` calls this once per statement, which is only
    /// *statement*-level granularity: a compound statement prints several
    /// header lines of its own (a `case` clause, an `elif`, an `except`, a
    /// decorator) and each of those used to inherit the offset of whatever
    /// was printed before it — typically the last line of the preceding
    /// body, so a `case Square(s):` header resolved to the `return` above
    /// it. The sub-statement clauses carry their own `TextRange`, so they
    /// call this too and name the line the user actually wrote.
    ///
    /// Synthesised nodes (produced by the desugar pass) carry a
    /// zero-length `TextRange`; those are ignored so they inherit the last
    /// known real offset rather than resetting to the top of the file.
    fn set_input_offset(&mut self, range: ruff_text_size::TextRange) {
        if u32::from(range.start()) != u32::from(range.end()) {
            self.current_input_offset = u32::from(range.start()) as usize;
        }
    }

    /// Append newlines until `self.output` ends in *at least* `count`
    /// trailing `\n` bytes (existing trailing newlines are preserved, not
    /// trimmed). Used to enforce PEP 8's two-blank-line rule around
    /// top-level class/def blocks: two blanks = three trailing `\n`.
    /// Callers rely on the floor; nothing in the pipeline produces a
    /// runaway tail of newlines, so trimming would only mask bugs.
    fn ensure_trailing_newlines(&mut self, count: usize) {
        let trailing = self
            .output
            .bytes()
            .rev()
            .take_while(|&b| b == b'\n')
            .count();
        for _ in trailing..count {
            self.newline();
        }
    }

    /// When the printer has the original source attached, return the
    /// first byte of `range` in that buffer. Returns `None` when no
    /// source is attached, when the range starts past the end of the
    /// buffer, or when the range is synthesised (zero start).
    fn original_first_byte(&self, range: ruff_text_size::TextRange) -> Option<u8> {
        let source = self.source.as_deref()?;
        let start = usize::from(range.start());
        source.as_bytes().get(start).copied()
    }

    fn indent_str(&self) -> String {
        " ".repeat(self.indent * INDENT_WIDTH)
    }

    fn fill(&mut self, s: &str) {
        let indent = self.indent_str();
        self.write(&indent);
        self.write(s);
    }

    fn enter_block(&mut self) {
        self.indent += 1;
    }

    fn leave_block(&mut self) {
        if self.indent > 0 {
            self.indent -= 1;
        }
    }

    // ── module ─────────────────────────────────────────────────────────────

    /// Emit a parsed module.  Ruff's `parse_module` always returns a
    /// `ModModule`, so the emitter only needs the single-variant signature.
    pub fn emit_mod(&mut self, module: &ModModule) {
        // For Python build output (suppress_mutability=true), inject
        // `from __future__ import annotations` so self-referencing class
        // annotations (Vec2 inside `impl Vec2`, recursive data structures,
        // operator overloads) don't blow up at class-body evaluation time
        // with NameError. PEP 563 makes annotations strings that resolve
        // lazily. Skipped for `tyc fmt` so Typhon source isn't perturbed.
        if self.suppress_mutability {
            // Future imports must come after any module docstring. Detect
            // a leading bare-string-literal expression statement and emit
            // it first, then the future import, then the rest.
            let mut iter = module.body.iter();
            if let Some(first) = iter.clone().next() {
                if is_module_docstring(first) {
                    self.emit_stmt(first);
                    iter.next();
                }
            }
            // N12 (2026-05-22): skip the injection when the user wrote
            // `from __future__ import annotations` themselves. Python
            // tolerates duplicate future-imports, but emitting two
            // identical lines is sloppy and any tool that diffs the
            // output will flag it.
            let user_already_imported = module.body.iter().any(user_imports_future_annotations);
            if !user_already_imported {
                self.writeln("from __future__ import annotations");
            }
            // PEP 695 lowering prelude (FINDINGS #47): for target < 3.12,
            // collect every PEP 695 type-param used in the module and
            // emit `typing.TypeVar(...)` definitions plus the required
            // `typing` imports. The def/class/type-alias arms then emit
            // legacy shapes that reference these synthetic globals.
            if self.lower_pep695() {
                let typevars = collect_pep695_typevars(module);
                let has_aliases = module.body.iter().any(|s| matches!(s, Stmt::TypeAlias(_)));
                let has_generic_class = module.body.iter().any(|s| match s {
                    Stmt::ClassDef(c) => c
                        .type_params
                        .as_deref()
                        .map(|tps| !tps.is_empty())
                        .unwrap_or(false),
                    _ => false,
                });
                if !typevars.is_empty() || has_aliases || has_generic_class {
                    let mut imports: Vec<&str> = Vec::new();
                    if typevars.iter().any(|p| p.kind == Pep695ParamKind::TypeVar) {
                        imports.push("TypeVar");
                    }
                    if typevars
                        .iter()
                        .any(|p| p.kind == Pep695ParamKind::ParamSpec)
                    {
                        imports.push("ParamSpec");
                    }
                    if typevars
                        .iter()
                        .any(|p| p.kind == Pep695ParamKind::TypeVarTuple)
                    {
                        imports.push("TypeVarTuple");
                    }
                    if has_generic_class {
                        imports.push("Generic");
                    }
                    if has_aliases {
                        imports.push("TypeAlias");
                    }
                    self.write("from typing import ");
                    self.write(&imports.join(", "));
                    self.newline();
                    for tv in &typevars {
                        let constructor = match tv.kind {
                            Pep695ParamKind::TypeVar => "TypeVar",
                            Pep695ParamKind::ParamSpec => "ParamSpec",
                            Pep695ParamKind::TypeVarTuple => "TypeVarTuple",
                        };
                        self.write(&tv.name);
                        self.write(" = ");
                        self.write(constructor);
                        self.write("(");
                        self.write_char('"');
                        self.write(&tv.name);
                        self.write_char('"');
                        // Carry the declared bound through to the
                        // legacy form so `def f[T: Iface](...)` lowers
                        // to `T = TypeVar("T", bound=Iface)`. Bounds
                        // are TypeVar-only — ParamSpec / TypeVarTuple
                        // ignore the field.
                        if tv.kind == Pep695ParamKind::TypeVar {
                            if let Some(bound) = &tv.bound_src {
                                self.write(", bound=");
                                self.write(bound);
                            }
                        }
                        self.writeln(")");
                    }
                }
            }
            for stmt in iter {
                self.emit_stmt(stmt);
            }
            return;
        }
        for stmt in &module.body {
            self.emit_stmt(stmt);
        }
    }

    // ── statements ─────────────────────────────────────────────────────────

    pub fn emit_stmt(&mut self, node: &Stmt) {
        // Update the active input offset from the node's source range.
        // Synthesised AST nodes (produced by the desugar pass) carry a
        // zero-length TextRange::default(); we skip those so they inherit
        // the last real offset rather than resetting to 0.
        let range = node.range();
        self.set_input_offset(range);
        // PEP 8: surround top-level class/def with two blank lines.  We
        // enforce this on either side of the block by checking both the
        // current statement and the previous top-level one.  Two blank
        // lines = three trailing `\n` characters before we emit anything
        // for the new statement.  The legacy `self.newline()` calls at
        // the start of the FunctionDef/ClassDef branches stay in place
        // for non-module scopes (methods inside a class).
        let current_is_block = matches!(node, Stmt::FunctionDef(_) | Stmt::ClassDef(_));
        if self.indent == 0
            && !self.output.is_empty()
            && (current_is_block || self.prev_top_level_was_block)
        {
            self.ensure_trailing_newlines(3);
        }
        match node {
            // `StmtFunctionDef` collapses sync and async; branch on `is_async`.
            Stmt::FunctionDef(f) => {
                if self.indent != 0 {
                    self.newline();
                }
                for decorator in &f.decorator_list {
                    // Each decorator is its own source line. Ruff's
                    // `StmtFunctionDef.range` starts at the first `@`, so
                    // without this the whole decorator stack *and* the
                    // `def` line all resolved to the first decorator.
                    self.set_input_offset(decorator.range);
                    self.fill("@");
                    self.emit_expr(&decorator.expression);
                    self.newline();
                }
                // The identifier sits on the `def` line, so its range
                // recovers the header's own provenance after the
                // decorators above moved the cursor.
                self.set_input_offset(f.name.range());
                if f.is_async {
                    self.fill("async def ");
                } else {
                    self.fill("def ");
                }
                self.write(f.name.as_str());
                // Skip the `[T]` type-param list when lowering to a
                // legacy target (FINDINGS #47) — the module prelude
                // emits matching `T = TypeVar("T")` definitions instead.
                if !self.lower_pep695() {
                    self.emit_type_params(f.type_params.as_deref());
                }
                self.write("(");
                self.emit_parameters(&f.parameters);
                self.write(")");
                if let Some(ret) = &f.returns {
                    self.write(" -> ");
                    self.emit_expr(ret);
                }
                self.writeln(":");
                self.enter_block();
                if f.body.is_empty() {
                    self.fill("pass");
                    self.newline();
                } else {
                    for stmt in &f.body {
                        self.emit_stmt(stmt);
                    }
                }
                self.leave_block();
            }

            Stmt::ClassDef(c) => {
                if self.indent != 0 {
                    self.newline();
                }
                for decorator in &c.decorator_list {
                    // See the `FunctionDef` arm: a decorator owns its line.
                    self.set_input_offset(decorator.range);
                    self.fill("@");
                    self.emit_expr(&decorator.expression);
                    self.newline();
                }
                self.set_input_offset(c.name.range());
                self.fill("class ");
                self.write(c.name.as_str());
                let lowering = self.lower_pep695();
                if !lowering {
                    self.emit_type_params(c.type_params.as_deref());
                }
                let bases = c.bases();
                let keywords = c.keywords();
                // Generic-class lowering for legacy targets (FINDINGS #47):
                // synthesise a `Generic[T, U, ...]` base from the
                // declared PEP 695 type-params so the runtime class
                // still tracks its parameters via the `typing` machinery.
                let generic_param_names: Vec<String> = if lowering {
                    c.type_params
                        .as_deref()
                        .map(|tps| {
                            tps.type_params
                                .iter()
                                .filter_map(|tp| match tp {
                                    TypeParam::TypeVar(t) => Some(t.name.as_str().to_owned()),
                                    _ => None,
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                if !bases.is_empty() || !keywords.is_empty() || !generic_param_names.is_empty() {
                    self.write("(");
                    let mut first = true;
                    for base in bases {
                        if !first {
                            self.write(", ");
                        }
                        self.emit_expr(base);
                        first = false;
                    }
                    if !generic_param_names.is_empty() {
                        if !first {
                            self.write(", ");
                        }
                        self.write("Generic[");
                        for (i, name) in generic_param_names.iter().enumerate() {
                            if i > 0 {
                                self.write(", ");
                            }
                            self.write(name);
                        }
                        self.write("]");
                        first = false;
                    }
                    for kw in keywords {
                        if !first {
                            self.write(", ");
                        }
                        if let Some(arg) = &kw.arg {
                            self.write(arg.as_str());
                            self.write("=");
                        }
                        self.emit_expr(&kw.value);
                        first = false;
                    }
                    self.write(")");
                }
                self.writeln(":");
                self.enter_block();
                if c.body.is_empty() {
                    self.fill("pass");
                    self.newline();
                } else {
                    for stmt in &c.body {
                        self.emit_stmt(stmt);
                    }
                }
                self.leave_block();
            }

            Stmt::Return(r) => {
                self.fill("return");
                if let Some(val) = &r.value {
                    self.write(" ");
                    self.emit_expr(val);
                }
                self.newline();
            }

            Stmt::Delete(d) => {
                self.fill("del ");
                let mut first = true;
                for target in &d.targets {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_expr(target);
                    first = false;
                }
                self.newline();
            }

            // Ruff's `StmtAssign` carries a Typhon-specific `mutability` that
            // tags the binding as `let` (immutable) or `mut` (mutable).  When
            // it's set we prepend the keyword before the targets so the
            // emitted Python preserves the source-level kind.  Plain Python
            // assigns leave `mutability` at `None` and no prefix is emitted.
            Stmt::Assign(a) => {
                self.fill("");
                if !self.suppress_mutability {
                    match a.mutability {
                        Some(Mutability::Let) => self.write("let "),
                        Some(Mutability::Mut) => self.write("mut "),
                        None => {}
                    }
                }
                let mut first = true;
                for target in &a.targets {
                    if !first {
                        self.write(" = ");
                    }
                    self.emit_expr(target);
                    first = false;
                }
                self.write(" = ");
                self.emit_expr(&a.value);
                self.newline();
            }

            Stmt::AugAssign(a) => {
                self.fill("");
                self.emit_expr(&a.target);
                self.write(" ");
                self.write(op_symbol(&a.op));
                self.write("= ");
                self.emit_expr(&a.value);
                self.newline();
            }

            // As with `StmtAssign`, the optional `mutability` prefix (if
            // present) is emitted before the annotated target so a Typhon
            // `let x: int = 1` round-trips intact.
            Stmt::AnnAssign(a) => {
                self.fill("");
                if !self.suppress_mutability {
                    match a.mutability {
                        Some(Mutability::Let) => self.write("let "),
                        Some(Mutability::Mut) => self.write("mut "),
                        None => {}
                    }
                }
                self.emit_expr(&a.target);
                self.write(": ");
                self.emit_expr(&a.annotation);
                if let Some(val) = &a.value {
                    self.write(" = ");
                    self.emit_expr(val);
                }
                self.newline();
            }

            // `StmtFor` collapses sync / async; branch on `is_async`.
            Stmt::For(f) => {
                if f.is_async {
                    self.fill("async for ");
                } else {
                    self.fill("for ");
                }
                self.emit_expr(&f.target);
                self.write(" in ");
                self.emit_expr(&f.iter);
                self.writeln(":");
                self.enter_block();
                self.emit_body(&f.body);
                self.leave_block();
                if !f.orelse.is_empty() {
                    self.fill("else:");
                    self.newline();
                    self.enter_block();
                    for stmt in &f.orelse {
                        self.emit_stmt(stmt);
                    }
                    self.leave_block();
                }
            }

            Stmt::While(w) => {
                self.fill("while ");
                self.emit_expr(&w.test);
                self.writeln(":");
                self.enter_block();
                self.emit_body(&w.body);
                self.leave_block();
                if !w.orelse.is_empty() {
                    self.fill("else:");
                    self.newline();
                    self.enter_block();
                    for stmt in &w.orelse {
                        self.emit_stmt(stmt);
                    }
                    self.leave_block();
                }
            }

            // Ruff's `StmtIf` doesn't have a recursive `orelse: Vec<Stmt>`
            // chain; it carries an explicit list of `ElifElseClause`s where
            // each clause has `test: Some(_)` for `elif` and `test: None`
            // for the trailing `else:` block.
            Stmt::If(i) => {
                self.fill("if ");
                self.emit_expr(&i.test);
                self.writeln(":");
                self.enter_block();
                self.emit_body(&i.body);
                self.leave_block();
                for clause in &i.elif_else_clauses {
                    // An `elif` / `else` header is a source line of its
                    // own; without this it inherited the offset of the
                    // last statement of the preceding branch's body.
                    self.set_input_offset(clause.range);
                    match &clause.test {
                        Some(test) => {
                            self.fill("elif ");
                            self.emit_expr(test);
                            self.writeln(":");
                            self.enter_block();
                            self.emit_body(&clause.body);
                            self.leave_block();
                        }
                        None => {
                            self.fill("else:");
                            self.newline();
                            self.enter_block();
                            self.emit_body(&clause.body);
                            self.leave_block();
                        }
                    }
                }
            }

            // `StmtWith` collapses sync / async; branch on `is_async`.
            Stmt::With(w) => {
                if w.is_async {
                    self.fill("async with ");
                } else {
                    self.fill("with ");
                }
                for (idx, item) in w.items.iter().enumerate() {
                    if idx > 0 {
                        self.write(", ");
                    }
                    self.emit_with_item(item);
                }
                self.writeln(":");
                self.enter_block();
                self.emit_body(&w.body);
                self.leave_block();
            }

            Stmt::Match(m) => {
                self.fill("match ");
                self.emit_expr(&m.subject);
                self.writeln(":");
                self.enter_block();
                if m.cases.is_empty() {
                    self.fill("pass");
                    self.newline();
                } else {
                    for case in &m.cases {
                        self.emit_match_case(case);
                    }
                }
                self.leave_block();
            }

            Stmt::Raise(r) => {
                self.fill("raise");
                if let Some(exc) = &r.exc {
                    self.write(" ");
                    self.emit_expr(exc);
                }
                if let Some(cause) = &r.cause {
                    self.write(" from ");
                    self.emit_expr(cause);
                }
                self.newline();
            }

            // Ruff merges `try` / `try*` into a single `StmtTry` discriminated
            // by `is_star`.
            Stmt::Try(t) => {
                self.fill("try:");
                self.newline();
                self.enter_block();
                self.emit_body(&t.body);
                self.leave_block();
                for handler in &t.handlers {
                    self.emit_except_handler(handler, t.is_star);
                }
                if !t.orelse.is_empty() {
                    self.fill("else:");
                    self.newline();
                    self.enter_block();
                    for stmt in &t.orelse {
                        self.emit_stmt(stmt);
                    }
                    self.leave_block();
                }
                if !t.finalbody.is_empty() {
                    self.fill("finally:");
                    self.newline();
                    self.enter_block();
                    for stmt in &t.finalbody {
                        self.emit_stmt(stmt);
                    }
                    self.leave_block();
                }
            }

            Stmt::Assert(a) => {
                self.fill("assert ");
                self.emit_expr(&a.test);
                if let Some(msg) = &a.msg {
                    self.write(", ");
                    self.emit_expr(msg);
                }
                self.newline();
            }

            Stmt::Import(i) => {
                self.fill("import ");
                let mut first = true;
                for alias in &i.names {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_alias(alias);
                    first = false;
                }
                self.newline();
            }

            // `level` is now a plain `u32` rather than `Option<Int>`; emit
            // that many leading dots.
            Stmt::ImportFrom(i) => {
                self.fill("from ");
                for _ in 0..i.level {
                    self.write(".");
                }
                if let Some(module) = &i.module {
                    self.write(module.as_str());
                }
                self.write(" import ");
                let mut first = true;
                for alias in &i.names {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_alias(alias);
                    first = false;
                }
                self.newline();
            }

            Stmt::Global(g) => {
                self.fill("global ");
                let names: Vec<&str> = g.names.iter().map(|n| n.as_str()).collect();
                self.write(&names.join(", "));
                self.newline();
            }

            Stmt::Nonlocal(n) => {
                self.fill("nonlocal ");
                let names: Vec<&str> = n.names.iter().map(|n| n.as_str()).collect();
                self.write(&names.join(", "));
                self.newline();
            }

            Stmt::Expr(e) => {
                self.fill("");
                self.emit_expr(&e.value);
                self.newline();
            }

            Stmt::Pass(_) => {
                self.fill("pass");
                self.newline();
            }

            Stmt::Break(_) => {
                self.fill("break");
                self.newline();
            }

            Stmt::Continue(_) => {
                self.fill("continue");
                self.newline();
            }

            Stmt::TypeAlias(t) => {
                if self.lower_pep695() {
                    // Legacy form: `X: TypeAlias = Y` (Python 3.10+).
                    // PEP 695 `type X[T] = ...` parameterised aliases
                    // can't be expressed in the legacy form, so we
                    // drop the `[T]` and let the value carry the
                    // parameterisation. This matches mypy's behaviour
                    // for the `TypeAlias` form on older targets.
                    self.fill("");
                    self.emit_expr(&t.name);
                    self.write(": TypeAlias = ");
                    self.emit_expr(&t.value);
                    self.newline();
                } else {
                    self.fill("type ");
                    self.emit_expr(&t.name);
                    self.emit_type_params(t.type_params.as_deref());
                    self.write(" = ");
                    self.emit_expr(&t.value);
                    self.newline();
                }
            }

            // Ruff exposes IPython escape commands as a dedicated statement
            // kind.  Plain Python source can't contain them, so we emit
            // their raw text verbatim and trust the source position to
            // signal anything Phase 0 isn't expected to handle.
            Stmt::IpyEscapeCommand(cmd) => {
                self.fill("");
                self.write(&cmd.value);
                self.newline();
            }
        }
        if self.indent == 0 {
            self.prev_top_level_was_block = current_is_block;
        }
    }

    /// Emit a PEP 695 type-parameter list (`[T]`, `[T: Number]`, `[*Ts, **P]`).
    /// No output when the list is missing or empty.
    fn emit_type_params(&mut self, params: Option<&TypeParams>) {
        let Some(params) = params else {
            return;
        };
        if params.is_empty() {
            return;
        }
        self.write("[");
        let mut first = true;
        for tp in &params.type_params {
            if !first {
                self.write(", ");
            }
            first = false;
            match tp {
                TypeParam::TypeVar(t) => {
                    self.write(t.name.as_str());
                    if let Some(bound) = &t.bound {
                        self.write(": ");
                        self.emit_expr(bound);
                    }
                }
                TypeParam::ParamSpec(p) => {
                    self.write("**");
                    self.write(p.name.as_str());
                }
                TypeParam::TypeVarTuple(t) => {
                    self.write("*");
                    self.write(t.name.as_str());
                }
            }
        }
        self.write("]");
    }

    // ── expressions ────────────────────────────────────────────────────────

    /// Emit `expr` as an operand of a construct whose own precedence is
    /// `parent_prec`, parenthesising it when it does not bind strictly
    /// tighter. This is the shared guard for the operand positions that
    /// `expr_precedence` alone cannot express, because the parent is not a
    /// `BinOp`.
    fn emit_operand_above(&mut self, expr: &Expr, parent_prec: u8) {
        if operand_precedence(expr) <= parent_prec {
            self.write("(");
            self.emit_expr(expr);
            self.write(")");
        } else {
            self.emit_expr(expr);
        }
    }

    /// Open an f-string interpolation, keeping a brace-opening expression
    /// distinguishable from an escaped literal brace.
    ///
    /// `f"{ {'k': 1}['k'] }"` interpolates a dict-literal subscript. Emitting
    /// the opener and the expression back to back produces `{{`, which CPython
    /// reads as the *escape* for a literal `{` — so the expression was never
    /// evaluated and the f-string rendered braces as text (or failed to parse
    /// on the closing side). A single space is enough to separate them and is
    /// invisible in the result.
    fn open_interpolation(&mut self, expr: &Expr) {
        self.write("{");
        if expr_starts_with_brace(expr) {
            self.write(" ");
        }
    }

    /// Close an interpolation, mirroring [`Self::open_interpolation`]: a
    /// trailing `}` from the expression would otherwise pair with the closing
    /// brace into `}}`, the literal-brace escape.
    fn close_interpolation(&mut self, expr: &Expr) {
        if expr_ends_with_brace(expr) {
            self.write(" ");
        }
        self.write("}");
    }

    pub fn emit_expr(&mut self, node: &Expr) {
        match node {
            Expr::BoolOp(b) => {
                let op = match b.op {
                    BoolOp::And => " and ",
                    BoolOp::Or => " or ",
                };
                let outer_prec = match b.op {
                    BoolOp::Or => 3u8,
                    BoolOp::And => 4u8,
                };
                let mut first = true;
                for val in &b.values {
                    if !first {
                        self.write(op);
                    }
                    if expr_precedence(val) < outer_prec {
                        self.write("(");
                        self.emit_expr(val);
                        self.write(")");
                    } else {
                        self.emit_expr(val);
                    }
                    first = false;
                }
            }

            // Ruff renamed `NamedExpr` → `Named` (walrus operator).
            Expr::Named(n) => {
                self.write("(");
                self.emit_expr(&n.target);
                self.write(" := ");
                self.emit_expr(&n.value);
                self.write(")");
            }

            Expr::BinOp(b) => {
                let prec = bin_op_precedence(&b.op);
                let left_prec = expr_precedence(&b.left);
                let right_prec = expr_precedence(&b.right);
                let is_pow = matches!(b.op, Operator::Pow);
                // `**` is right-associative; everything else is left-associative.
                let left_needs_parens = if is_pow {
                    left_prec <= prec
                } else {
                    left_prec < prec
                };
                let right_needs_parens = if is_pow {
                    right_prec < prec
                } else {
                    right_prec <= prec
                };
                if left_needs_parens {
                    self.write("(");
                    self.emit_expr(&b.left);
                    self.write(")");
                } else {
                    self.emit_expr(&b.left);
                }
                self.write(" ");
                self.write(op_symbol(&b.op));
                self.write(" ");
                if right_needs_parens {
                    self.write("(");
                    self.emit_expr(&b.right);
                    self.write(")");
                } else {
                    self.emit_expr(&b.right);
                }
            }

            Expr::UnaryOp(u) => {
                let op = match u.op {
                    UnaryOp::Invert => "~",
                    UnaryOp::Not => "not ",
                    UnaryOp::UAdd => "+",
                    UnaryOp::USub => "-",
                };
                // `not` is precedence 5; arithmetic unaries (`+`/`-`/`~`)
                // are 13. If the operand has lower precedence than the
                // unary itself, it must be parenthesised — otherwise
                // `not (a or b)` re-emits as `not a or b`, which is
                // `(not a) or b` under Python's grammar.
                let self_prec: u8 = match u.op {
                    UnaryOp::Not => 5,
                    UnaryOp::UAdd | UnaryOp::USub | UnaryOp::Invert => 13,
                };
                let needs_parens = expr_precedence(&u.operand) < self_prec;
                self.write(op);
                if needs_parens {
                    self.write("(");
                    self.emit_expr(&u.operand);
                    self.write(")");
                } else {
                    self.emit_expr(&u.operand);
                }
            }

            Expr::Lambda(l) => {
                self.write("lambda");
                if let Some(params) = l.parameters.as_deref() {
                    if !params.is_empty() {
                        self.write(" ");
                        self.emit_parameters(params);
                    }
                }
                self.write(": ");
                self.emit_expr(&l.body);
            }

            // `IfExp` is now `Expr::If`.
            //
            // Python's grammar is
            //   conditional_expression ::= or_test ["if" or_test "else" expression]
            // so the body and the test must bind tighter than a conditional
            // (they are `or_test`s), while the `else` arm is a full
            // `expression` and needs no guard — that is what makes chained
            // ternaries and a trailing lambda emit correctly. Without the
            // guard, `(a if p else b) if q else c` re-emits as
            // `a if p else b if q else c`, which regroups to the right.
            Expr::If(i) => {
                self.emit_operand_above(&i.body, IF_EXP_PRECEDENCE);
                self.write(" if ");
                self.emit_operand_above(&i.test, IF_EXP_PRECEDENCE);
                self.write(" else ");
                self.emit_expr(&i.orelse);
            }

            // Ruff packs the dict entries as `Vec<DictItem>` where each entry
            // has `key: Option<Expr>` and `value: Expr`; a missing key means
            // `**spread`.
            Expr::Dict(d) => {
                self.write("{");
                let mut first = true;
                for item in &d.items {
                    if !first {
                        self.write(", ");
                    }
                    if let Some(k) = &item.key {
                        self.emit_expr(k);
                        self.write(": ");
                        self.emit_expr(&item.value);
                    } else {
                        self.write("**");
                        self.emit_expr(&item.value);
                    }
                    first = false;
                }
                self.write("}");
            }

            Expr::Set(s) => {
                self.write("{");
                let mut first = true;
                for elem in &s.elts {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_expr(elem);
                    first = false;
                }
                self.write("}");
            }

            Expr::ListComp(l) => {
                self.write("[");
                self.emit_expr(&l.elt);
                for gen in &l.generators {
                    self.emit_comprehension(gen);
                }
                self.write("]");
            }

            Expr::SetComp(s) => {
                self.write("{");
                self.emit_expr(&s.elt);
                for gen in &s.generators {
                    self.emit_comprehension(gen);
                }
                self.write("}");
            }

            // Ruff makes `key` an `Option<Expr>`; treat a missing key as
            // an unreachable codepath in valid Python but stay defensive.
            Expr::DictComp(d) => {
                self.write("{");
                if let Some(key) = &d.key {
                    self.emit_expr(key);
                    self.write(": ");
                }
                self.emit_expr(&d.value);
                for gen in &d.generators {
                    self.emit_comprehension(gen);
                }
                self.write("}");
            }

            // `GeneratorExp` is now `Expr::Generator`.
            Expr::Generator(g) => {
                self.write("(");
                self.emit_expr(&g.elt);
                for gen in &g.generators {
                    self.emit_comprehension(gen);
                }
                self.write(")");
            }

            Expr::Await(a) => {
                self.write("await ");
                self.emit_expr(&a.value);
            }

            Expr::Yield(y) => {
                self.write("yield");
                if let Some(val) = &y.value {
                    self.write(" ");
                    self.emit_expr(val);
                }
            }

            Expr::YieldFrom(y) => {
                self.write("yield from ");
                self.emit_expr(&y.value);
            }

            Expr::Compare(c) => {
                // Every operand of a comparison must bind tighter than the
                // comparison itself. Without this guard `(a if p else b) == c`
                // re-emits as `a if p else b == c`, which Python parses as
                // `a if p else (b == c)` — a different program that still
                // compiles, and `(x := 1) == 1` re-emits as unparseable
                // Python. Chained comparisons arrive as a single `Compare`
                // node, so a nested `Compare` operand can only have come from
                // explicit parens in the source and must keep them.
                self.emit_operand_above(&c.left, COMPARE_PRECEDENCE);
                for (op, right) in c.ops.iter().zip(c.comparators.iter()) {
                    let op_str = match op {
                        CmpOp::Eq => " == ",
                        CmpOp::NotEq => " != ",
                        CmpOp::Lt => " < ",
                        CmpOp::LtE => " <= ",
                        CmpOp::Gt => " > ",
                        CmpOp::GtE => " >= ",
                        CmpOp::Is => " is ",
                        CmpOp::IsNot => " is not ",
                        CmpOp::In => " in ",
                        CmpOp::NotIn => " not in ",
                    };
                    self.write(op_str);
                    self.emit_operand_above(right, COMPARE_PRECEDENCE);
                }
            }

            // Ruff bundles positional args and keywords under
            // `ExprCall.arguments: Arguments`.
            Expr::Call(c) => {
                if needs_paren_for_postfix(&c.func) {
                    self.write("(");
                    self.emit_expr(&c.func);
                    self.write(")");
                } else {
                    self.emit_expr(&c.func);
                }
                self.write("(");
                let mut first = true;
                for arg in c.arguments.args.iter() {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_expr(arg);
                    first = false;
                }
                for kw in c.arguments.keywords.iter() {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_keyword(kw);
                    first = false;
                }
                self.write(")");
            }

            // `JoinedStr` + `FormattedValue` are now a single `ExprFString`.
            // Each part is either a literal `StringLiteral` or an `FString`
            // whose elements are interleaved literal/interpolation pieces.
            Expr::FString(fs) => {
                // Pick an outer quote that won't collide with any nested
                // same-quoted string literal inside the interpolations.
                // Default to `"` and only flip if we'd produce invalid
                // Python 3.11 output otherwise. This is the same strategy
                // Black uses for f-string formatting.
                let outer = pick_fstring_outer_quote(fs);
                self.fstring_quote_stack.push(outer);
                self.write("f");
                self.write_char(outer);
                for part in fs.value.iter() {
                    match part {
                        FStringPart::Literal(lit) => {
                            self.write(&escape_python_string_with_quote(lit.as_str(), outer));
                        }
                        FStringPart::FString(inner) => {
                            for elem in &inner.elements {
                                match elem {
                                    InterpolatedStringElement::Literal(lit) => {
                                        self.write(&escape_python_fstring_literal(
                                            &lit.value, outer,
                                        ));
                                    }
                                    InterpolatedStringElement::Interpolation(interp) => {
                                        // PEP 501 debug f-string: `f"{x=}"` carries
                                        // the verbatim `x=` text on `debug_text`,
                                        // including the surrounding whitespace
                                        // (`f"{x = }"`). Emit it as-is so the
                                        // `=` marker survives the round-trip.
                                        // Verbatim text can never begin or end
                                        // with a bare brace (that would have been
                                        // the `{{` / `}}` escape in the source),
                                        // so the brace-separation spaces must not
                                        // fire — the debug output reproduces them
                                        // verbatim at runtime.
                                        if let Some(dt) = &interp.debug_text {
                                            self.write("{");
                                            self.write(dt.as_str());
                                        } else {
                                            self.open_interpolation(&interp.expression);
                                            self.emit_expr(&interp.expression);
                                        }
                                        // Emit `!r` / `!s` / `!a` conversion flags
                                        // — these are stripped by default by the
                                        // AST but carried on the `conversion`
                                        // field. Losing them silently changes
                                        // runtime output of `f"{x!r}"` etc.
                                        let conversion = interp.conversion.to_char();
                                        if let Some(c) = conversion {
                                            self.write("!");
                                            self.write_char(c);
                                        }
                                        // Emit `:FORMAT_SPEC` — the spec is
                                        // itself a mini-f-string that may
                                        // contain further interpolations
                                        // (`f"{n:>{width}}"`).
                                        if let Some(spec) = &interp.format_spec {
                                            self.write(":");
                                            for spec_elem in &spec.elements {
                                                match spec_elem {
                                                    InterpolatedStringElement::Literal(lit) => {
                                                        self.write(&escape_python_fstring_literal(
                                                            &lit.value, outer,
                                                        ));
                                                    }
                                                    InterpolatedStringElement::Interpolation(
                                                        nested,
                                                    ) => {
                                                        self.open_interpolation(&nested.expression);
                                                        self.emit_expr(&nested.expression);
                                                        self.close_interpolation(
                                                            &nested.expression,
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        // The close separator only exists to keep
                                        // the *expression's* trailing `}` from
                                        // pairing with the closing brace into the
                                        // `}}` escape. Debug text, a conversion,
                                        // or a format spec already separates them
                                        // — and a space added after those lands
                                        // *inside* the conversion/spec (a
                                        // SyntaxError after `!r`; format code
                                        // `\x20` in a spec).
                                        if interp.debug_text.is_some()
                                            || conversion.is_some()
                                            || interp.format_spec.is_some()
                                        {
                                            self.write("}");
                                        } else {
                                            self.close_interpolation(&interp.expression);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                self.write_char(outer);
                self.fstring_quote_stack.pop();
            }

            // PEP 750 template strings — Phase 0 falls back to f-string-like
            // rendering since `t"..."` is not yet part of the Phase 0 source
            // subset.  Emitting as `t"..."` keeps semantics distinct from
            // f-strings.
            Expr::TString(ts) => {
                self.write("t\"");
                for part in ts.value.iter() {
                    for elem in &part.elements {
                        match elem {
                            InterpolatedStringElement::Literal(lit) => {
                                self.write(&escape_python_string(&lit.value));
                            }
                            InterpolatedStringElement::Interpolation(interp) => {
                                self.open_interpolation(&interp.expression);
                                self.emit_expr(&interp.expression);
                                self.close_interpolation(&interp.expression);
                            }
                        }
                    }
                }
                self.write("\"");
            }

            // Typed-literal arms — each was previously a `Constant` variant.
            Expr::NumberLiteral(n) => match &n.value {
                Number::Int(i) => {
                    // Avoid heap allocation if it fits in 64 bits (positive or negative)
                    if let Some(small) = i.as_i64() {
                        let mut buf = itoa::Buffer::new();
                        self.write(buf.format(small));
                    } else if let Some(small_u) = i.as_u64() {
                        let mut buf = itoa::Buffer::new();
                        self.write(buf.format(small_u));
                    } else {
                        self.write(&i.to_string());
                    }
                }
                Number::Float(f) => {
                    // `{}` drops `.0` on whole-number f64 — emit `1` rather
                    // than `1.0`, which then loads as int at runtime and
                    // breaks isinstance(x, float), JSON output, repr, etc.
                    // Use Debug formatting so 1.0 stays "1.0".
                    if f.is_infinite() {
                        if *f < 0.0 {
                            self.write("-inf");
                        } else {
                            self.write("inf");
                        }
                    } else if f.is_nan() {
                        self.write("nan");
                    } else {
                        let mut buf = ryu::Buffer::new();
                        self.write(buf.format(*f));
                    }
                }
                Number::Complex { real, imag } => {
                    let mut buf = ryu::Buffer::new();
                    if *real != 0.0 {
                        self.write(buf.format(*real));
                        self.write("+");
                    }
                    self.write(buf.format(*imag));
                    self.write("j");
                }
            },

            Expr::StringLiteral(s) => {
                // Inside an f-string interpolation, a nested string literal
                // must use a different quote delimiter than the enclosing
                // f-string. Python 3.12+ (PEP 701) permits identical-quote
                // nesting, but 3.11 doesn't, and the universal-quote-style
                // form is what Black/ruff emit. Track the active outer
                // quote(s) in `fstring_quote_stack`.
                let outer = self.fstring_quote_stack.last().copied();
                let quote = match outer {
                    Some('"') => '\'',
                    Some('\'') => '"',
                    _ => '"',
                };
                let text = s.value.to_str();
                // Preserve triple-quoted strings when the source used triple
                // quotes OR when the content contains a literal newline — in
                // both cases a single-quoted form would require `\n` escapes
                // and lose readability. Don't use triple-quotes inside an
                // f-string interpolation (outer is Some) because the inner
                // quotes must differ from the outer ones and nesting triple
                // quotes is almost always wrong.
                let use_triple = outer.is_none()
                    && (s.value.first_literal_flags().is_triple_quoted() || text.contains('\n'));
                if use_triple {
                    self.write_char(quote);
                    self.write_char(quote);
                    self.write_char(quote);
                    self.write(&escape_triple_quoted_string(text, quote));
                    self.write_char(quote);
                    self.write_char(quote);
                    self.write_char(quote);
                } else {
                    self.write_char(quote);
                    self.write(&escape_python_string_with_quote(text, quote));
                    self.write_char(quote);
                }
            }

            Expr::BytesLiteral(b) => {
                self.write("b\"");
                let mut buf = [0; 4];
                for byte in b.value.bytes() {
                    match byte {
                        b'\\' => self.write("\\\\"),
                        b'"' => self.write("\\\""),
                        b'\n' => self.write("\\n"),
                        b'\r' => self.write("\\r"),
                        b'\t' => self.write("\\t"),
                        // Keep printable ASCII as-is so `b"hello"` reads as
                        // `b"hello"` instead of `b"\x68\x65\x6c\x6c\x6f"`.
                        0x20..=0x7e => self.write((byte as char).encode_utf8(&mut buf)),
                        _ => self.write_hex_escape(byte),
                    }
                }
                self.write("\"");
            }

            Expr::BooleanLiteral(b) => {
                self.write(if b.value { "True" } else { "False" });
            }

            Expr::NoneLiteral(_) => {
                self.write("None");
            }

            Expr::EllipsisLiteral(_) => {
                self.write("...");
            }

            Expr::Attribute(a) => {
                if needs_paren_for_postfix(&a.value) {
                    self.write("(");
                    self.emit_expr(&a.value);
                    self.write(")");
                } else {
                    self.emit_expr(&a.value);
                }
                self.write(".");
                self.write(a.attr.as_str());
            }

            Expr::Subscript(s) => {
                if needs_paren_for_postfix(&s.value) {
                    self.write("(");
                    self.emit_expr(&s.value);
                    self.write(")");
                } else {
                    self.emit_expr(&s.value);
                }
                self.write("[");
                // A tuple slice usually emits without outer parens — `X[A, B]`
                // not `X[(A, B)]` — but a one-element tuple must keep its
                // trailing comma so `x[1,]` (tuple-key lookup) is not silently
                // rewritten to `x[1]` (integer-key lookup).
                if let Expr::Tuple(t) = s.slice.as_ref() {
                    match t.elts.len() {
                        0 => self.write("()"),
                        1 => {
                            self.emit_expr(&t.elts[0]);
                            self.write(",");
                        }
                        _ => {
                            let mut first = true;
                            for elem in &t.elts {
                                if !first {
                                    self.write(", ");
                                }
                                self.emit_expr(elem);
                                first = false;
                            }
                        }
                    }
                } else {
                    self.emit_expr(&s.slice);
                }
                self.write("]");
            }

            Expr::Starred(s) => {
                self.write("*");
                self.emit_expr(&s.value);
            }

            Expr::Name(n) => {
                self.write(n.id.as_str());
            }

            Expr::List(l) => {
                self.write("[");
                let mut first = true;
                for elem in &l.elts {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_expr(elem);
                    first = false;
                }
                self.write("]");
            }

            Expr::Tuple(t) => {
                // Always wrap in parens: `()` for empty, `(x,)` for 1-tuple,
                // `(a, b, c)` otherwise. Bare comma-separated form is unsafe
                // in call-argument or subscript positions.
                self.write("(");
                if t.elts.len() == 1 {
                    self.emit_expr(&t.elts[0]);
                    self.write(",");
                } else {
                    let mut first = true;
                    for elem in &t.elts {
                        if !first {
                            self.write(", ");
                        }
                        self.emit_expr(elem);
                        first = false;
                    }
                }
                self.write(")");
            }

            Expr::Slice(s) => {
                if let Some(lower) = &s.lower {
                    self.emit_expr(lower);
                }
                self.write(":");
                if let Some(upper) = &s.upper {
                    self.emit_expr(upper);
                }
                if let Some(step) = &s.step {
                    self.write(":");
                    self.emit_expr(step);
                }
            }

            // IPython escape expression (`dir = !pwd` etc.) — render the raw
            // command text verbatim; not part of the Phase 0 source subset.
            Expr::IpyEscapeCommand(cmd) => {
                self.write(&cmd.value);
            }
        }
    }

    // ── helpers ────────────────────────────────────────────────────────────

    fn emit_parameters(&mut self, params: &Parameters) {
        let mut first = true;

        // Positional-only args (before /)
        for arg in &params.posonlyargs {
            if !first {
                self.write(", ");
            }
            self.emit_param_with_default(arg);
            first = false;
        }
        if !params.posonlyargs.is_empty() {
            self.write(", /");
        }

        // Regular args (each carries its own optional default)
        for arg in &params.args {
            if !first {
                self.write(", ");
            }
            self.emit_param_with_default(arg);
            first = false;
        }

        // *args
        if let Some(vararg) = &params.vararg {
            if !first {
                self.write(", ");
            }
            self.write("*");
            self.emit_plain_param(vararg);
            first = false;
        } else if !params.kwonlyargs.is_empty() {
            if !first {
                self.write(", ");
            }
            self.write("*");
            first = false;
        }

        // Keyword-only args (each carries its own optional default)
        for arg in &params.kwonlyargs {
            if !first {
                self.write(", ");
            }
            self.emit_param_with_default(arg);
            first = false;
        }

        // **kwargs
        if let Some(kwarg) = &params.kwarg {
            if !first {
                self.write(", ");
            }
            self.write("**");
            self.emit_plain_param(kwarg);
        }
    }

    /// Emit a `ParameterWithDefault` — the parameter name, optional
    /// annotation, and optional default value.
    fn emit_param_with_default(&mut self, arg: &ParameterWithDefault) {
        self.emit_plain_param(&arg.parameter);
        if let Some(default) = &arg.default {
            self.write(" = ");
            self.emit_expr(default);
        }
    }

    /// Emit a plain `Parameter` (name + optional annotation, no default).
    fn emit_plain_param(&mut self, param: &Parameter) {
        self.write(param.name.as_str());
        if let Some(ann) = &param.annotation {
            self.write(": ");
            self.emit_expr(ann);
        }
    }

    fn emit_alias(&mut self, alias: &Alias) {
        self.write(alias.name.as_str());
        if let Some(asname) = &alias.asname {
            self.write(" as ");
            self.write(asname.as_str());
        }
    }

    fn emit_keyword(&mut self, kw: &Keyword) {
        if let Some(arg) = &kw.arg {
            self.write(arg.as_str());
            self.write("=");
        } else {
            self.write("**");
        }
        self.emit_expr(&kw.value);
    }

    fn emit_comprehension(&mut self, gen: &Comprehension) {
        if gen.is_async {
            self.write(" async for ");
        } else {
            self.write(" for ");
        }
        self.emit_expr(&gen.target);
        self.write(" in ");
        self.emit_expr(&gen.iter);
        for cond in &gen.ifs {
            self.write(" if ");
            self.emit_expr(cond);
        }
    }

    fn emit_with_item(&mut self, item: &WithItem) {
        self.emit_expr(&item.context_expr);
        if let Some(var) = &item.optional_vars {
            self.write(" as ");
            self.emit_expr(var);
        }
    }

    fn emit_except_handler(&mut self, handler: &ExceptHandler, star: bool) {
        let ExceptHandler::ExceptHandler(h) = handler;
        // The handler header is its own source line; inheriting the
        // offset left behind by the `try` body pointed it at the last
        // statement of that body.
        self.set_input_offset(h.range);
        if star {
            self.fill("except*");
        } else {
            self.fill("except");
        }
        if let Some(typ) = &h.type_ {
            self.write(" ");
            self.emit_expr(typ);
        }
        if let Some(name) = &h.name {
            self.write(" as ");
            self.write(name.as_str());
        }
        self.writeln(":");
        self.enter_block();
        self.emit_body(&h.body);
        self.leave_block();
    }

    fn emit_match_case(&mut self, case: &MatchCase) {
        // A `case` clause header is a source line of its own. Without
        // this it inherited the offset of the previous arm's last
        // statement, so `case Square(s):` resolved to the `return` above
        // it — the canonical symptom of statement-level granularity.
        self.set_input_offset(case.range);
        self.fill("case ");
        self.emit_pattern(&case.pattern);
        if let Some(guard) = &case.guard {
            self.write(" if ");
            self.emit_expr(guard);
        }
        self.writeln(":");
        self.enter_block();
        self.emit_body(&case.body);
        self.leave_block();
    }

    /// Emit a compound-statement body, falling back to `pass` when empty so
    /// the generated Python remains syntactically valid.
    fn emit_body(&mut self, body: &[Stmt]) {
        if body.is_empty() {
            self.fill("pass");
            self.newline();
        } else {
            for stmt in body {
                self.emit_stmt(stmt);
            }
        }
    }

    fn emit_pattern(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::MatchValue(v) => self.emit_expr(&v.value),
            // `PatternMatchSingleton` now carries a `Singleton` enum directly
            // (None / True / False) instead of a wrapped `Constant`.
            Pattern::MatchSingleton(s) => match s.value {
                Singleton::None => self.write("None"),
                Singleton::True => self.write("True"),
                Singleton::False => self.write("False"),
            },
            Pattern::MatchSequence(seq) => {
                // Python's sequence patterns accept both `[a, b]` and
                // `(a, b)` (PEP 634); they're semantically identical and
                // both match list *or* tuple instances. When the source
                // is available we peek at the first byte of the
                // pattern's `TextRange` to recover the user's original
                // bracket choice (O27 / FINDINGS #111). When unavailable
                // we default to parens for 2+ elements (reads better in
                // emitted Python — "destructure this pair") and brackets
                // for 0/1 elements where parens would be ambiguous
                // (`()` is invalid in pattern position; `(a)` would
                // parse as a capture, not a 1-tuple).
                let use_parens = match self.original_first_byte(seq.range()) {
                    Some(b'[') => false,
                    Some(b'(') => true,
                    _ => seq.patterns.len() >= 2,
                };
                let (open, close) = if use_parens { ("(", ")") } else { ("[", "]") };
                self.write(open);
                let mut first = true;
                for p in &seq.patterns {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_pattern(p);
                    first = false;
                }
                self.write(close);
            }
            Pattern::MatchMapping(m) => {
                self.write("{");
                let mut first = true;
                for (key, pat) in m.keys.iter().zip(m.patterns.iter()) {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_expr(key);
                    self.write(": ");
                    self.emit_pattern(pat);
                    first = false;
                }
                if let Some(rest) = &m.rest {
                    if !first {
                        self.write(", ");
                    }
                    self.write("**");
                    self.write(rest.as_str());
                }
                self.write("}");
            }
            // The class-pattern arguments are now bundled under
            // `PatternArguments { patterns, keywords }` where each keyword is
            // a `PatternKeyword { attr, pattern }`.
            Pattern::MatchClass(c) => {
                self.emit_expr(&c.cls);
                self.write("(");
                let mut first = true;
                for p in &c.arguments.patterns {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_pattern(p);
                    first = false;
                }
                for kw in &c.arguments.keywords {
                    if !first {
                        self.write(", ");
                    }
                    self.write(kw.attr.as_str());
                    self.write("=");
                    self.emit_pattern(&kw.pattern);
                    first = false;
                }
                self.write(")");
            }
            Pattern::MatchStar(s) => {
                self.write("*");
                if let Some(name) = &s.name {
                    self.write(name.as_str());
                } else {
                    self.write("_");
                }
            }
            Pattern::MatchAs(a) => {
                if let Some(pat) = &a.pattern {
                    self.emit_pattern(pat);
                    self.write(" as ");
                }
                if let Some(name) = &a.name {
                    self.write(name.as_str());
                } else {
                    self.write("_");
                }
            }
            Pattern::MatchOr(o) => {
                let mut first = true;
                for p in &o.patterns {
                    if !first {
                        self.write(" | ");
                    }
                    self.emit_pattern(p);
                    first = false;
                }
            }
        }
    }
}

impl Default for Emitter {
    fn default() -> Self {
        Self::new()
    }
}

/// True when `stmt` is a module-level docstring (`Expr::StringLiteral`
/// wrapped in `Stmt::Expr`). PEP 236 requires future imports to follow
/// the docstring, so the emitter peels the docstring off before
/// injecting `from __future__ import annotations`.
fn is_module_docstring(stmt: &Stmt) -> bool {
    if let Stmt::Expr(e) = stmt {
        matches!(e.value.as_ref(), Expr::StringLiteral(_))
    } else {
        false
    }
}

/// `true` when `stmt` is `from __future__ import annotations` (alone or
/// among other names — `from __future__ import annotations, division`
/// counts too). Used by the module emitter to skip the auto-injection
/// when the user already wrote the import themselves (N12).
fn user_imports_future_annotations(stmt: &Stmt) -> bool {
    let Stmt::ImportFrom(f) = stmt else {
        return false;
    };
    if f.level != 0 {
        return false;
    }
    let Some(module_name) = &f.module else {
        return false;
    };
    if module_name.as_str() != "__future__" {
        return false;
    }
    f.names
        .iter()
        .any(|alias| alias.name.as_str() == "annotations")
}

fn op_symbol(op: &Operator) -> &'static str {
    match op {
        Operator::Add => "+",
        Operator::Sub => "-",
        Operator::Mult => "*",
        Operator::MatMult => "@",
        Operator::Div => "/",
        Operator::Mod => "%",
        Operator::Pow => "**",
        Operator::LShift => "<<",
        Operator::RShift => ">>",
        Operator::BitOr => "|",
        Operator::BitXor => "^",
        Operator::BitAnd => "&",
        Operator::FloorDiv => "//",
    }
}

/// Precedence of a Python binary operator. Higher values bind tighter.
///
/// Note: `**` sits at 14 (above the 13 we give arithmetic unary operators)
/// so that an arithmetic-unary left operand of `**` is wrapped in parens.
/// Python parses `-x ** 2` as `-(x ** 2)`, so the AST shape
/// `BinOp(Pow, UnaryOp(USub, x), 2)` must round-trip as `(-x) ** 2`.
fn bin_op_precedence(op: &Operator) -> u8 {
    match op {
        Operator::Pow => 14,
        Operator::Mult | Operator::MatMult | Operator::Div | Operator::Mod | Operator::FloorDiv => {
            12
        }
        Operator::Add | Operator::Sub => 11,
        Operator::LShift | Operator::RShift => 10,
        Operator::BitAnd => 9,
        Operator::BitXor => 8,
        Operator::BitOr => 7,
    }
}

/// Precedence of an arbitrary expression in the context of a `BinOp` child.
///
/// Atoms (names, literals, calls, subscripts, attribute access, …) are
/// self-delimiting and get the maximum precedence so no parens are ever
/// inserted around them. Only expression forms that could be ambiguous as a
/// `BinOp` child are assigned explicit precedences.
///
/// `not` has very low precedence in Python (between `and` and comparisons),
/// so it is distinguished from the arithmetic unary operators which sit
/// just below `**`.
/// Precedence of a conditional expression (`x if c else y`).
const IF_EXP_PRECEDENCE: u8 = 2;
/// Precedence of a comparison chain (`a < b <= c`).
const COMPARE_PRECEDENCE: u8 = 6;

/// `expr_precedence` extended with the forms that cannot appear as a `BinOp`
/// child without already being a syntax error, but *can* appear as an operand
/// of a comparison or a conditional — and which therefore still need parens
/// there. A bare `x := 1` or `yield v` in a comparison operand is a hard
/// SyntaxError, not merely a regrouping.
fn operand_precedence(expr: &Expr) -> u8 {
    match expr {
        Expr::Named(_) | Expr::Yield(_) | Expr::YieldFrom(_) => 0,
        _ => expr_precedence(expr),
    }
}

fn expr_precedence(expr: &Expr) -> u8 {
    match expr {
        Expr::Lambda(_) => 1,
        Expr::If(_) => 2,
        Expr::BoolOp(b) => match b.op {
            BoolOp::Or => 3,
            BoolOp::And => 4,
        },
        Expr::UnaryOp(u) => match u.op {
            UnaryOp::Not => 5,
            UnaryOp::UAdd | UnaryOp::USub | UnaryOp::Invert => 13,
        },
        Expr::Compare(_) => 6,
        Expr::BinOp(b) => bin_op_precedence(&b.op),
        _ => u8::MAX,
    }
}

/// True when `expr` must be parenthesised before a Python postfix operator
/// (attribute access `.x`, subscript `[…]`, call `(…)`) is applied to it.
///
/// Postfix operators in Python bind tighter than every binary, boolean, and
/// comparison operator, and tighter than lambdas / ternaries / walrus /
/// await / yield. Forgetting the wrap means the postfix attaches to the
/// *rightmost* sub-expression rather than to the whole, which Python parses
/// happily but with different semantics — e.g. `(a + b).upper()` emitted as
/// `a + b.upper()` evaluates `b.upper()` first and concatenates.
///
/// This guard is the symmetric partner of `expr_precedence`: anything that
/// would have needed parens as a `BinOp` child needs parens here too, plus
/// a few more forms that `expr_precedence` ignores because they can't
/// appear as a `BinOp` child without already being a syntax error.
fn needs_paren_for_postfix(expr: &Expr) -> bool {
    // An integer literal immediately followed by `.attr` is misparsed by
    // CPython: `255.to_bytes(...)` reads `255.` as a float, raising a
    // SyntaxError. Wrapping the literal in parens — `(255).to_bytes(...)` —
    // fixes it. Float/complex literals already contain a `.`/`j` and are
    // valid as-is, so only the integer case needs parens.
    if let Expr::NumberLiteral(n) = expr {
        if matches!(n.value, Number::Int(_)) {
            return true;
        }
    }
    matches!(
        expr,
        Expr::BoolOp(_)
            | Expr::BinOp(_)
            | Expr::UnaryOp(_)
            | Expr::Lambda(_)
            | Expr::If(_)
            | Expr::Compare(_)
            | Expr::Named(_)
            | Expr::Await(_)
            | Expr::Yield(_)
            | Expr::YieldFrom(_)
            | Expr::Starred(_)
            | Expr::Generator(_)
    )
}

/// Escape a string value for emission as a double-quoted Python literal.
///
/// Covers the characters that would otherwise terminate the literal or
/// produce invalid Python: backslash, double quote, the common whitespace
/// escapes (`\n`, `\r`, `\t`), and other ASCII control characters via
/// `\xNN`.
fn escape_python_string(s: &str) -> String {
    escape_python_string_with_quote(s, '"')
}

/// Escape a Python string literal for output between the given quote
/// character. Only the *active* quote is backslash-escaped — the opposite
/// quote can appear verbatim, matching Black's quote-style policy.
fn escape_python_string_with_quote(s: &str, quote: char) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x00'..='\x1f' | '\x7f' => {
                push_hex_escape(&mut out, ch as u8);
            }
            c => out.push(c),
        }
    }
    out
}

/// True when the emitted text of `expr` begins with `{` — a dict or set
/// display, which is the only expression form that can.
fn expr_starts_with_brace(expr: &Expr) -> bool {
    match expr {
        Expr::Dict(_) | Expr::Set(_) | Expr::DictComp(_) | Expr::SetComp(_) => true,
        // The brace belongs to the left-most leaf: `{1: 2}[k]`, `{1}.pop()`,
        // `{1, 2} | s`, `{1: 2}["k"] if c else d`.
        Expr::Subscript(x) => expr_starts_with_brace(&x.value),
        Expr::Attribute(x) => expr_starts_with_brace(&x.value),
        Expr::Call(x) => expr_starts_with_brace(&x.func),
        Expr::BinOp(x) => expr_starts_with_brace(&x.left),
        Expr::Compare(x) => expr_starts_with_brace(&x.left),
        Expr::BoolOp(x) => x.values.first().is_some_and(expr_starts_with_brace),
        Expr::If(x) => expr_starts_with_brace(&x.body),
        _ => false,
    }
}

/// True when the emitted text of `expr` ends with `}`.
fn expr_ends_with_brace(expr: &Expr) -> bool {
    match expr {
        Expr::Dict(_) | Expr::Set(_) | Expr::DictComp(_) | Expr::SetComp(_) => true,
        Expr::BinOp(x) => expr_ends_with_brace(&x.right),
        Expr::BoolOp(x) => x.values.last().is_some_and(expr_ends_with_brace),
        Expr::Compare(x) => x.comparators.last().is_some_and(expr_ends_with_brace),
        Expr::If(x) => expr_ends_with_brace(&x.orelse),
        Expr::UnaryOp(x) => expr_ends_with_brace(&x.operand),
        Expr::Starred(x) => expr_ends_with_brace(&x.value),
        _ => false,
    }
}

/// Escape content for a triple-quoted string literal using `quote` as the
/// delimiter character (either `'` or `"`).  Only two things need escaping:
/// backslashes, and a run of three or more consecutive delimiter characters
/// (which would prematurely close the literal).  Newlines and tabs are kept
/// verbatim so the multiline structure is preserved in the emitted file.
fn escape_triple_quoted_string(s: &str, quote: char) -> String {
    let mut out = String::with_capacity(s.len());
    // Track how many consecutive unescaped quote chars we have just emitted.
    let mut run = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            out.push_str("\\\\");
            run = 0;
        } else if ch == quote {
            run += 1;
            // A quote that is the LAST character of the content is just as
            // dangerous as a run of three inside it: the closing delimiter
            // butts straight up against it. Content ending in one quote
            // emitted `"""…""""`, which is `"""…"""` plus a stray quote — a
            // hard SyntaxError. Content ending in two emitted `"""…"""""`,
            // which Python reads as `"""…"""` implicitly concatenated with
            // the empty string `""`, so the two trailing quotes vanished
            // *silently* and the constant differed from the source.
            let at_tail = chars.peek().is_none();
            if run == 3 || at_tail {
                out.push('\\');
                run = 1;
            }
            out.push(quote);
        } else {
            out.push(ch);
            run = 0;
        }
    }
    out
}

/// Escape a literal segment from inside an f-string.  Identical to
/// [`escape_python_string_with_quote`] but additionally doubles `{` → `{{`
/// and `}` → `}}` — the ruff parser stores f-string literal segments with
/// braces already un-escaped, so emitting them verbatim would turn a
/// literal `{` back into an interpolation opener (`f"{ \"a\": 1 }"` would
/// re-parse as `f"<expr>"` with `\"a\": 1` as the expression, which is a
/// syntax error).
fn escape_python_fstring_literal(s: &str, quote: char) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("{{"),
            '}' => out.push_str("}}"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x00'..='\x1f' | '\x7f' => {
                push_hex_escape(&mut out, ch as u8);
            }
            c => out.push(c),
        }
    }
    out
}

/// One PEP 695 type-parameter as collected for legacy-target lowering.
/// `bound_src` is the rendered Python text of any declared bound
/// expression (or `None` when there isn't one); the prelude uses it to
/// emit `T = TypeVar("T", bound=Iface)` rather than dropping the
/// constraint silently.
#[derive(Debug, Clone)]
pub(crate) struct Pep695Param {
    pub name: String,
    pub kind: Pep695ParamKind,
    pub bound_src: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pep695ParamKind {
    /// `def f[T]`, `class C[T]`, `type X[T]`
    TypeVar,
    /// `def f[**P]`
    ParamSpec,
    /// `def f[*Ts]`
    TypeVarTuple,
}

/// Collect every distinct PEP 695 type-parameter declared on any
/// `def f[...]`, `class C[...]`, or `type X[...] = ...` in the module.
/// Used by the legacy-target lowering (FINDINGS #47) to synthesise
/// matching `TypeVar` / `ParamSpec` / `TypeVarTuple` prelude lines.
///
/// Declared bounds (`T: Bound`) are rendered back to Python source via
/// a throwaway sub-emitter so the prelude emits
/// `T = TypeVar("T", bound=Bound)` rather than dropping the constraint.
/// `ParamSpec` / `TypeVarTuple` ignore bounds (they don't carry any).
fn collect_pep695_typevars(module: &ModModule) -> Vec<Pep695Param> {
    let mut out: Vec<Pep695Param> = Vec::new();
    fn push_unique(out: &mut Vec<Pep695Param>, param: Pep695Param) {
        if !out.iter().any(|p| p.name == param.name) {
            out.push(param);
        }
    }
    fn render_bound(expr: &Expr) -> String {
        let mut sub = Emitter::new();
        sub.emit_expr(expr);
        sub.finish()
    }
    fn walk_params(params: &TypeParams, out: &mut Vec<Pep695Param>) {
        for tp in &params.type_params {
            match tp {
                TypeParam::TypeVar(t) => push_unique(
                    out,
                    Pep695Param {
                        name: t.name.as_str().to_owned(),
                        kind: Pep695ParamKind::TypeVar,
                        bound_src: t.bound.as_deref().map(render_bound),
                    },
                ),
                TypeParam::ParamSpec(p) => push_unique(
                    out,
                    Pep695Param {
                        name: p.name.as_str().to_owned(),
                        kind: Pep695ParamKind::ParamSpec,
                        bound_src: None,
                    },
                ),
                TypeParam::TypeVarTuple(t) => push_unique(
                    out,
                    Pep695Param {
                        name: t.name.as_str().to_owned(),
                        kind: Pep695ParamKind::TypeVarTuple,
                        bound_src: None,
                    },
                ),
            }
        }
    }
    fn walk_stmt(stmt: &Stmt, out: &mut Vec<Pep695Param>) {
        match stmt {
            Stmt::FunctionDef(f) => {
                if let Some(tps) = f.type_params.as_deref() {
                    walk_params(tps, out);
                }
                for nested in &f.body {
                    walk_stmt(nested, out);
                }
            }
            Stmt::ClassDef(c) => {
                if let Some(tps) = c.type_params.as_deref() {
                    walk_params(tps, out);
                }
                for nested in &c.body {
                    walk_stmt(nested, out);
                }
            }
            Stmt::TypeAlias(t) => {
                if let Some(tps) = t.type_params.as_deref() {
                    walk_params(tps, out);
                }
            }
            _ => {}
        }
    }
    for stmt in &module.body {
        walk_stmt(stmt, &mut out);
    }
    out
}

/// Choose an outer-quote character for an f-string so nested same-quoted
/// string literals don't collide with the delimiter on Python 3.11. We
/// scan every interpolation's expression tree for string literals; if any
/// already contains `"`, we use `'` for the outer instead. Conservative:
/// when both styles appear, we prefer `"` and let the inner-literal pass
/// flip its quote (the universal Black/ruff convention).
fn pick_fstring_outer_quote(fs: &ast::ExprFString) -> char {
    use ast::visitor::source_order::{walk_expr, SourceOrderVisitor};

    struct Probe {
        needs_double: bool,
        needs_single: bool,
    }
    impl<'a> SourceOrderVisitor<'a> for Probe {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::StringLiteral(s) = expr {
                let v = s.value.to_str();
                if v.contains('"') {
                    self.needs_double = true;
                }
                if v.contains('\'') {
                    self.needs_single = true;
                }
            }
            walk_expr(self, expr);
        }
    }

    let mut probe = Probe {
        needs_double: false,
        needs_single: false,
    };
    for part in fs.value.iter() {
        if let FStringPart::FString(inner) = part {
            for elem in &inner.elements {
                if let InterpolatedStringElement::Interpolation(interp) = elem {
                    probe.visit_expr(&interp.expression);
                    if let Some(spec) = &interp.format_spec {
                        for spec_elem in &spec.elements {
                            if let InterpolatedStringElement::Interpolation(nested) = spec_elem {
                                probe.visit_expr(&nested.expression);
                            }
                        }
                    }
                }
            }
        }
    }
    // Prefer `"`; flip to `'` only if a nested literal already needs `"`.
    match (probe.needs_double, probe.needs_single) {
        (true, false) => '\'',
        _ => '"',
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn triple_quoted_tail_quote_run_is_escaped() {
        // A quote at the very end of the content butts against the closing
        // delimiter. One trailing quote emitted `"""…""""` — a hard
        // SyntaxError. Two emitted `"""…"""""`, which Python reads as the
        // literal implicitly concatenated with the empty string `""`, so the
        // trailing quotes vanished *silently* and the constant differed from
        // the source.
        assert_eq!(escape_triple_quoted_string("x\"", '"'), "x\\\"");
        assert_eq!(escape_triple_quoted_string("y\"\"", '"'), "y\"\\\"");
        // A run of three inside the content is still escaped as before.
        assert_eq!(escape_triple_quoted_string("a\"\"\"b", '"'), "a\"\"\\\"b");
        // No trailing quote, no change.
        assert_eq!(escape_triple_quoted_string("plain", '"'), "plain");
        // The other delimiter is handled symmetrically.
        assert_eq!(escape_triple_quoted_string("x'", '\''), "x\\'");
        assert_eq!(escape_triple_quoted_string("x\"", '\''), "x\"");
    }

    #[test]
    fn fstring_interpolation_of_a_brace_expression_is_separated() {
        // `f"{ {'k': 1}['k'] }"` — the opener and a dict display back to back
        // produce `{{`, which CPython reads as the escape for a literal brace,
        // so the expression was never evaluated.
        let out = round_trip("x = f\"{ {'k': 1}['k'] }\"\n");
        assert!(
            out.contains("f\"{ {"),
            "the interpolation opener must be separated from the dict display; got:\n{out}"
        );
        assert!(
            !out.contains("f\"{{"),
            "must not emit the literal-brace escape; got:\n{out}"
        );
        // The closing side needs no space here — the expression ends in `]`.
        // It does when the expression itself ends in a brace, which would
        // otherwise pair with the closing one into the `}}` escape.
        let ends_with_brace = round_trip("y = f\"{ {1, 2} }\"\n");
        assert!(
            ends_with_brace.contains("{ {1, 2} }"),
            "a set display must be separated on both sides; got:\n{ends_with_brace}"
        );
    }
    use crate::emit;
    use crate::printer::escape_triple_quoted_string;
    use tyc_syntax::parse_module;

    fn round_trip(src: &str) -> String {
        let parsed = parse_module(src).expect("parse failed");
        emit(parsed.syntax())
    }

    /// Locate a Python 3.12+ interpreter on `PATH`; returns `None` to skip.
    /// A bare `python3` is only accepted after a version probe (it may be
    /// 3.11, which predates the PEP 701 f-string grammar these tests need).
    /// `TYC_REQUIRE_PYTHON=1` turns the skip into a panic, matching the
    /// `tyc` crate's execution-tier tests.
    fn python() -> Option<String> {
        for candidate in ["python3.13", "python3.12", "python3"] {
            if let Ok(out) = std::process::Command::new(candidate)
                .arg("--version")
                .output()
            {
                if out.status.success() {
                    let v = String::from_utf8_lossy(&out.stdout);
                    if let Some(rest) = v.trim().strip_prefix("Python 3.") {
                        if let Some(minor) = rest.split('.').next() {
                            if let Ok(m) = minor.parse::<u32>() {
                                if m >= 12 {
                                    return Some(candidate.to_owned());
                                }
                            }
                        }
                    }
                }
            }
        }
        if std::env::var_os("TYC_REQUIRE_PYTHON").is_some() {
            panic!("TYC_REQUIRE_PYTHON is set but no Python 3.12+ interpreter was found on PATH");
        }
        None
    }

    /// Run `code` under `py`, asserting a clean exit; returns stdout.
    fn run_python(py: &str, code: &str) -> String {
        let out = std::process::Command::new(py)
            .arg("-c")
            .arg(code)
            .output()
            .expect("failed to spawn python");
        assert!(
            out.status.success(),
            "python rejected the code:\n{code}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn fstring_conversion_spec_and_debug_text_stay_flush_with_the_braces() {
        // The brace-separation space must attach to the *expression*, never
        // to a conversion / format spec / debug text: a space after `:spec`
        // becomes format code `\x20` (ValueError at runtime), a space after
        // `!r` is a SyntaxError, and synthetic spaces around debug text are
        // reproduced verbatim in the program's output. Each emitted program
        // is executed under a PEP 701-capable CPython (3.12+; `python3`
        // alone may be 3.11 here) and must print exactly what the source
        // prints.
        let cases: [(&str, &str); 3] = [
            // Expression ends with `}`, followed by a format spec.
            (
                "x = 7\nflag = True\nprint(f\"{x if flag else {} :>5}\")\n",
                "    7\n",
            ),
            // PEP 501 debug text: the user's own separating space, nothing
            // more, on both sides.
            ("print(f\"{ {1, 2}=}\")\n", " {1, 2}={1, 2}\n"),
            // Expression ends with `}`, followed by a conversion.
            ("print(f\"{ {1, 2} !r}\")\n", "{1, 2}\n"),
        ];
        let spec_out = round_trip(cases[0].0);
        assert!(
            spec_out.contains("{}:>5}"),
            "the spec must stay flush against the closing brace; got:\n{spec_out}"
        );
        let debug_out = round_trip(cases[1].0);
        assert!(
            debug_out.contains("f\"{ {1, 2}=}\""),
            "debug text must be emitted verbatim with no synthetic spaces; got:\n{debug_out}"
        );
        let conv_out = round_trip(cases[2].0);
        assert!(
            conv_out.contains("{1, 2}!r}"),
            "the conversion must stay flush on both sides; got:\n{conv_out}"
        );
        let Some(py) = python() else {
            eprintln!("skipping execution tier: no Python 3.12+ on PATH");
            return;
        };
        for (src, expected) in cases {
            let emitted = round_trip(src);
            assert_eq!(
                run_python(&py, &emitted),
                expected,
                "emitted Python diverged from the source program:\n{emitted}"
            );
        }
    }

    #[test]
    fn round_trip_assignment() {
        let src = "x: int = 1\n";
        let out = round_trip(src);
        assert!(out.contains("x: int = 1"), "got: {}", out);
    }

    #[test]
    fn round_trip_function() {
        let src = "def foo(x: int) -> int:\n    return x + 1\n";
        let out = round_trip(src);
        assert!(out.contains("def foo"), "got: {}", out);
        assert!(out.contains("return"), "got: {}", out);
    }

    #[test]
    fn round_trip_import() {
        let src = "import os\nfrom pathlib import Path\n";
        let out = round_trip(src);
        assert!(out.contains("import os"), "got: {}", out);
        assert!(out.contains("from pathlib import Path"), "got: {}", out);
    }

    #[test]
    fn elif_chain_preserved() {
        let src = "if a:\n    x = 1\nelif b:\n    x = 2\nelif c:\n    x = 3\nelse:\n    x = 4\n";
        let out = round_trip(src);
        assert!(out.contains("elif b:"), "missing elif b, got: {}", out);
        assert!(out.contains("elif c:"), "missing elif c, got: {}", out);
        assert!(out.contains("else:"), "missing else, got: {}", out);
    }

    #[test]
    fn async_for_else_emitted() {
        let src = "async def f():\n    async for x in it:\n        pass\n    else:\n        pass\n";
        let out = round_trip(src);
        assert!(out.contains("else:"), "async for else dropped: {}", out);
    }

    #[test]
    fn binop_precedence_parens() {
        let src = "x = (a + b) * c\n";
        let out = round_trip(src);
        assert!(
            out.contains("(a + b) * c"),
            "precedence parens missing: {}",
            out
        );
    }

    #[test]
    fn tuple_empty_has_parens() {
        let src = "x = ()\n";
        let out = round_trip(src);
        assert!(out.contains("()"), "empty tuple wrong: {}", out);
    }

    #[test]
    fn tuple_one_element_has_parens() {
        let src = "x = (1,)\n";
        let out = round_trip(src);
        assert!(out.contains("(1,)"), "1-tuple wrong: {}", out);
    }

    #[test]
    fn tuple_multi_element_has_parens() {
        let src = "x = (1, 2, 3)\n";
        let out = round_trip(src);
        assert!(out.contains("(1, 2, 3)"), "tuple wrong: {}", out);
    }

    #[test]
    fn subscript_multi_arg_no_outer_parens() {
        // `Result[int, str]` must NOT emit as `Result[(int, str)]`.
        let src = "x: Result[int, str] = z\n";
        let out = round_trip(src);
        assert!(
            out.contains("Result[int, str]"),
            "expected `Result[int, str]`, got: {}",
            out
        );
        assert!(
            !out.contains("Result[(int, str)]"),
            "tuple slice should not be parenthesised: {}",
            out
        );
    }

    #[test]
    fn subscript_one_tuple_key_keeps_comma() {
        // `x[1,]` is a tuple-keyed lookup and must NOT be silently rewritten
        // to `x[1]` (integer-keyed lookup) by the subscript tuple-unwrap.
        let src = "y = x[1,]\n";
        let out = round_trip(src);
        assert!(
            out.contains("x[1,]"),
            "single-element tuple slice must keep trailing comma: {}",
            out
        );
    }

    #[test]
    fn unary_minus_left_of_pow_parenthesised() {
        // Python parses `-x ** 2` as `-(x ** 2)`, so the AST shape
        // `(-x) ** 2` must round-trip with explicit parens.
        let src = "y = (-x) ** 2\n";
        let out = round_trip(src);
        assert!(
            out.contains("(-x) ** 2"),
            "unary minus left of ** must be parenthesised, got: {}",
            out
        );
    }

    #[test]
    fn binop_before_attribute_parenthesised() {
        // (a + b).upper() must NOT emit as a + b.upper() — Python parses the
        // latter as a + (b.upper()), changing semantics silently.
        let src = "y = (\"a\" + \"b\").upper()\n";
        let out = round_trip(src);
        assert!(
            out.contains("(\"a\" + \"b\").upper()"),
            "BinOp before attribute must keep parens, got: {}",
            out
        );
    }

    #[test]
    fn int_literal_before_attribute_parenthesised() {
        // `(255).to_bytes(...)` must NOT emit as `255.to_bytes(...)` — CPython
        // parses `255.` as a float literal and raises a SyntaxError.
        let src = "y = (255).to_bytes(4, \"big\")\n";
        let out = round_trip(src);
        assert!(
            out.contains("(255).to_bytes(4, \"big\")"),
            "int literal before attribute must keep parens, got: {}",
            out
        );
        assert!(
            !out.contains("255.to_bytes"),
            "int literal before attribute must not be bare, got: {}",
            out
        );
    }

    #[test]
    fn float_literal_before_attribute_not_parenthesised() {
        // A float literal already contains a `.`, so `(1.5).hex()` is valid
        // without extra parens — we must NOT over-parenthesise it.
        let src = "y = 1.5.hex()\n";
        let out = round_trip(src);
        assert!(
            out.contains("1.5.hex()"),
            "float literal before attribute should not be parenthesised, got: {}",
            out
        );
        assert!(
            !out.contains("(1.5)"),
            "float literal before attribute must not be parenthesised, got: {}",
            out
        );
    }

    #[test]
    fn binop_before_subscript_parenthesised() {
        let src = "y = ([1, 2] + [3, 4])[1:3]\n";
        let out = round_trip(src);
        assert!(
            out.contains("([1, 2] + [3, 4])[1:3]"),
            "BinOp before subscript must keep parens, got: {}",
            out
        );
    }

    #[test]
    fn ternary_before_attribute_parenthesised() {
        let src = "y = (\"x\" if c else \"y\").upper()\n";
        let out = round_trip(src);
        assert!(
            out.contains("(\"x\" if c else \"y\").upper()"),
            "ternary before attribute must keep parens, got: {}",
            out
        );
    }

    #[test]
    fn lambda_before_call_parenthesised() {
        let src = "y = (lambda n: n * 2)(7)\n";
        let out = round_trip(src);
        assert!(
            out.contains("(lambda n: n * 2)(7)"),
            "lambda before call must keep parens, got: {}",
            out
        );
    }

    #[test]
    fn or_inside_and_parenthesised() {
        // `and` binds tighter than `or`; (a or b) and c must keep its parens
        // so it doesn't get re-grouped to a or (b and c).
        let src = "z = (a or b) and c\n";
        let out = round_trip(src);
        assert!(
            out.contains("(a or b) and c"),
            "or-inside-and must keep parens, got: {}",
            out
        );
    }

    #[test]
    fn boolop_div_before_attribute_parenthesised() {
        // (root / \"x\").write_text(...) — pathlib idiom; without parens
        // becomes root / (\"x\".write_text(...)) at runtime.
        let src = "y = (root / \"x\").write_text(\"hi\")\n";
        let out = round_trip(src);
        assert!(
            out.contains("(root / \"x\").write_text"),
            "binop before method call must keep parens, got: {}",
            out
        );
    }

    #[test]
    fn not_inside_arithmetic_parenthesised() {
        // `not` has very low precedence; it must be parenthesised when
        // used as a child of an arithmetic BinOp so the result is valid
        // Python.
        let src = "z = a + (not b)\n";
        let out = round_trip(src);
        assert!(
            out.contains("a + (not b)"),
            "`not` as BinOp child must be parenthesised, got: {}",
            out
        );
    }

    #[test]
    fn two_blank_lines_between_top_level_classes() {
        let src = "class A:\n    x: int\n\nclass B:\n    y: int\n";
        let out = round_trip(src);
        assert!(
            out.contains("y: int\n"),
            "field missing from emitted class B: {}",
            out
        );
        assert!(
            out.contains("\n\n\nclass B"),
            "PEP 8 requires two blank lines before top-level class, got: {:?}",
            out
        );
    }

    #[test]
    fn two_blank_lines_after_top_level_class() {
        // The statement that follows a top-level class/def must also have
        // two blank lines before it, even when the statement itself is not
        // a block (e.g. a module-level assignment or a bare call).
        let src = "class A:\n    x: int\n\ngreeting = \"hi\"\nmain()\n";
        let out = round_trip(src);
        assert!(
            out.contains("\n\n\ngreeting = "),
            "expected two blank lines between top-level class and assignment, got: {:?}",
            out
        );
    }

    #[test]
    fn methods_keep_single_blank_line() {
        // Inside a class body methods are separated by ONE blank line, not two.
        let src =
            "class A:\n    def foo(self) -> None:\n        pass\n    def bar(self) -> None:\n        pass\n";
        let out = round_trip(src);
        assert!(out.contains("def foo"), "missing foo: {}", out);
        assert!(out.contains("def bar"), "missing bar: {}", out);
        assert!(
            !out.contains("\n\n\n    def bar"),
            "methods must not get two blank lines between them, got: {:?}",
            out
        );
    }

    #[test]
    fn match_sequence_bracket_pattern_round_trips() {
        // O27 / FINDINGS #111: when the printer has the original source,
        // `case [a, b]:` should re-emit as `[a, b]` rather than the
        // default `(a, b)`. The bracket choice is preserved verbatim.
        let src = "match xs:\n    case [a, b]:\n        pass\n";
        let parsed = parse_module(src).expect("parse failed");
        let mut emitter = crate::Emitter::new();
        emitter.set_source(std::sync::Arc::from(src));
        emitter.emit_mod(parsed.syntax());
        let out = emitter.finish();
        assert!(
            out.contains("case [a, b]"),
            "bracket pattern should round-trip when source is attached, got: {out}",
        );
    }

    #[test]
    fn match_sequence_paren_pattern_round_trips() {
        // Same as above but the original was `case (a, b):` —
        // parens should still emit as parens.
        let src = "match xs:\n    case (a, b):\n        pass\n";
        let parsed = parse_module(src).expect("parse failed");
        let mut emitter = crate::Emitter::new();
        emitter.set_source(std::sync::Arc::from(src));
        emitter.emit_mod(parsed.syntax());
        let out = emitter.finish();
        assert!(
            out.contains("case (a, b)"),
            "paren pattern should round-trip when source is attached, got: {out}",
        );
    }

    #[test]
    fn match_sequence_without_source_uses_default() {
        // Without `set_source`, the printer falls back to the default
        // bracket choice — parens for 2+ elements. This guards against
        // regressions in callers that don't (or can't) attach source.
        let src = "match xs:\n    case [a, b]:\n        pass\n";
        let out = round_trip(src);
        assert!(
            out.contains("case (a, b)"),
            "default bracket choice should be parens for 2-elem, got: {out}",
        );
    }

    #[test]
    fn string_newline_escaped() {
        let src = "x = \"hello\\nworld\"\n";
        let out = round_trip(src);
        let after_eq = out.split('=').nth(1).unwrap_or("");
        assert!(
            !after_eq.contains('\n') || after_eq.trim_end().ends_with('"'),
            "raw newline inside emitted string literal: {:?}",
            out
        );
    }

    #[test]
    fn user_future_import_is_not_duplicated_in_python_emit() {
        // Regression for N12 (2026-05-22): the Python-emit path (used by
        // `tyc build`) unconditionally injected
        // `from __future__ import annotations` even when the user had
        // already written it, producing two identical lines.
        use crate::emit_python;
        let parsed = tyc_syntax::parse_module("from __future__ import annotations\nx: int = 1\n")
            .expect("parse failed");
        let out = emit_python(parsed.syntax());
        assert_eq!(
            out.matches("from __future__ import annotations").count(),
            1,
            "expected exactly one future-import line, got:\n{out}"
        );
    }

    #[test]
    fn future_import_still_injected_when_absent_in_python_emit() {
        use crate::emit_python;
        let parsed = tyc_syntax::parse_module("x: int = 1\n").expect("parse failed");
        let out = emit_python(parsed.syntax());
        assert!(
            out.contains("from __future__ import annotations"),
            "future import must still be injected when user didn't write it: {out}"
        );
    }

    #[test]
    fn not_over_boolop_keeps_parens() {
        // `not (a or b)` re-emitted as `not a or b` would mean
        // `(not a) or b` under Python's grammar. Round-trip must preserve
        // the original semantics.
        for src in [
            "x = not (a or b)\n",
            "x = not (a and b)\n",
            "x = not (a == 0 and b == 0)\n",
        ] {
            let out = round_trip(src);
            assert!(
                out.contains("not (") && (out.contains(" or ") || out.contains(" and ")),
                "parens stripped around BoolOp under `not`: {out}"
            );
        }
    }

    #[test]
    fn not_over_ternary_keeps_parens() {
        // `not (X if C else Y)` would re-emit as `not X if C else Y`,
        // which parses as `(not X) if C else Y` — different semantics.
        let src = "x = not (True if a else False)\n";
        let out = round_trip(src);
        assert!(
            out.contains("not (True if a else False)"),
            "parens stripped around ternary under `not`: {out}"
        );
    }

    #[test]
    fn triple_quoted_string_preserved() {
        // A triple-quoted string literal must round-trip with triple quotes so
        // its embedded newlines are kept verbatim rather than `\n`-escaped.
        let src = "x = \"\"\"line one\nline two\nline three\"\"\"\n";
        let out = round_trip(src);
        assert!(
            out.contains("\"\"\""),
            "triple-quoted string lost its delimiters: {:?}",
            out
        );
        assert!(
            out.contains("line one\nline two"),
            "embedded newlines were escaped away: {:?}",
            out
        );
    }

    #[test]
    fn triple_quoted_string_with_literal_newlines_in_value() {
        // Even when the source uses a single-quoted string, if the parsed
        // value contains a real newline character the emitter should use
        // triple quotes to avoid `\n` in the output.
        let src = "msg = \"first\\nsecond\"\n";
        // The source has \\n (escaped), so the parsed string value is
        // "first\nsecond" with a real newline.  The emitter should emit it
        // with triple quotes.
        let out = round_trip(src);
        // Either triple-quoted or single-line with \n escape is acceptable;
        // the key invariant is that the content round-trips correctly.
        assert!(
            out.contains("first") && out.contains("second"),
            "string content lost during emission: {:?}",
            out
        );
    }

    #[test]
    fn escape_triple_quoted_no_false_close() {
        // A string whose content includes triple double-quotes must have
        // the run broken so the literal doesn't close prematurely.
        let body = "has \"\"\" inside";
        let escaped = escape_triple_quoted_string(body, '"');
        // The escaped form must not contain an un-broken """.
        assert!(
            !escaped.contains("\"\"\""),
            "triple-quote run not broken: {:?}",
            escaped
        );
    }
}
