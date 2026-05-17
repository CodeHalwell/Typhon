//! Hand-written Python pretty-printer.
//!
//! Covers the Python 3 grammar subset used in Phase 0 round-trip testing.
//! Aim: parse → emit produces output that is semantically equivalent to the
//! input (whitespace and comment differences are acceptable in Phase 0).

use ruff_python_ast::{
    Alias, BoolOp, CmpOp, Comprehension, ExceptHandler, Expr, FStringPart,
    InterpolatedStringElement, Keyword, MatchCase, ModModule, Mutability, Number, Operator,
    Parameter, ParameterWithDefault, Parameters, Pattern, Singleton, Stmt, TypeParam, TypeParams,
    UnaryOp, WithItem,
};
use ruff_text_size::Ranged;

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
}

const INDENT_WIDTH: usize = 4;

impl Emitter {
    pub fn new() -> Self {
        Self {
            output: String::new(),
            indent: 0,
            current_input_offset: 0,
            line_offsets: Vec::new(),
        }
    }

    pub fn finish(self) -> String {
        self.output
    }

    // ── helpers ────────────────────────────────────────────────────────────

    fn write(&mut self, s: &str) {
        self.output.push_str(s);
    }

    fn writeln(&mut self, s: &str) {
        self.output.push_str(s);
        self.line_offsets.push(self.current_input_offset);
        self.output.push('\n');
    }

    fn newline(&mut self) {
        self.line_offsets.push(self.current_input_offset);
        self.output.push('\n');
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
        if u32::from(range.start()) != u32::from(range.end()) {
            self.current_input_offset = u32::from(range.start()) as usize;
        }
        match node {
            // `StmtFunctionDef` collapses sync and async; branch on `is_async`.
            Stmt::FunctionDef(f) => {
                self.newline();
                for decorator in &f.decorator_list {
                    self.fill("@");
                    self.emit_expr(&decorator.expression);
                    self.newline();
                }
                if f.is_async {
                    self.fill("async def ");
                } else {
                    self.fill("def ");
                }
                self.write(f.name.as_str());
                self.emit_type_params(f.type_params.as_deref());
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
                self.newline();
                for decorator in &c.decorator_list {
                    self.fill("@");
                    self.emit_expr(&decorator.expression);
                    self.newline();
                }
                self.fill("class ");
                self.write(c.name.as_str());
                self.emit_type_params(c.type_params.as_deref());
                let bases = c.bases();
                let keywords = c.keywords();
                if !bases.is_empty() || !keywords.is_empty() {
                    self.write("(");
                    let mut first = true;
                    for base in bases {
                        if !first {
                            self.write(", ");
                        }
                        self.emit_expr(base);
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
                match a.mutability {
                    Some(Mutability::Let) => self.write("let "),
                    Some(Mutability::Mut) => self.write("mut "),
                    None => {}
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
                match a.mutability {
                    Some(Mutability::Let) => self.write("let "),
                    Some(Mutability::Mut) => self.write("mut "),
                    None => {}
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
                self.fill("type ");
                self.emit_expr(&t.name);
                self.emit_type_params(t.type_params.as_deref());
                self.write(" = ");
                self.emit_expr(&t.value);
                self.newline();
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

    pub fn emit_expr(&mut self, node: &Expr) {
        match node {
            Expr::BoolOp(b) => {
                let op = match b.op {
                    BoolOp::And => " and ",
                    BoolOp::Or => " or ",
                };
                let mut first = true;
                for val in &b.values {
                    if !first {
                        self.write(op);
                    }
                    self.emit_expr(val);
                    first = false;
                }
            }

            // Ruff renamed `NamedExpr` → `Named` (walrus operator).
            Expr::Named(n) => {
                self.emit_expr(&n.target);
                self.write(" := ");
                self.emit_expr(&n.value);
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
                self.write(op);
                self.emit_expr(&u.operand);
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
            Expr::If(i) => {
                self.emit_expr(&i.body);
                self.write(" if ");
                self.emit_expr(&i.test);
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
                self.emit_expr(&c.left);
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
                    self.emit_expr(right);
                }
            }

            // Ruff bundles positional args and keywords under
            // `ExprCall.arguments: Arguments`.
            Expr::Call(c) => {
                self.emit_expr(&c.func);
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
                self.write("f\"");
                for part in fs.value.iter() {
                    match part {
                        FStringPart::Literal(lit) => {
                            self.write(&escape_python_string(lit.as_str()));
                        }
                        FStringPart::FString(inner) => {
                            for elem in &inner.elements {
                                match elem {
                                    InterpolatedStringElement::Literal(lit) => {
                                        self.write(&escape_python_string(&lit.value));
                                    }
                                    InterpolatedStringElement::Interpolation(interp) => {
                                        self.write("{");
                                        self.emit_expr(&interp.expression);
                                        self.write("}");
                                    }
                                }
                            }
                        }
                    }
                }
                self.write("\"");
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
                                self.write("{");
                                self.emit_expr(&interp.expression);
                                self.write("}");
                            }
                        }
                    }
                }
                self.write("\"");
            }

            // Typed-literal arms — each was previously a `Constant` variant.
            Expr::NumberLiteral(n) => match &n.value {
                Number::Int(i) => {
                    self.write(&i.to_string());
                }
                Number::Float(f) => {
                    self.write(&format!("{}", f));
                }
                Number::Complex { real, imag } => {
                    if *real != 0.0 {
                        self.write(&format!("{}+", real));
                    }
                    self.write(&format!("{}j", imag));
                }
            },

            Expr::StringLiteral(s) => {
                self.write("\"");
                self.write(&escape_python_string(s.value.to_str()));
                self.write("\"");
            }

            Expr::BytesLiteral(b) => {
                self.write("b\"");
                for byte in b.value.bytes() {
                    self.write(&format!("\\x{:02x}", byte));
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
                self.emit_expr(&a.value);
                self.write(".");
                self.write(a.attr.as_str());
            }

            Expr::Subscript(s) => {
                self.emit_expr(&s.value);
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
                self.write("[");
                let mut first = true;
                for p in &seq.patterns {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_pattern(p);
                    first = false;
                }
                self.write("]");
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

/// Escape a string value for emission as a double-quoted Python literal.
///
/// Covers the characters that would otherwise terminate the literal or
/// produce invalid Python: backslash, double quote, the common whitespace
/// escapes (`\n`, `\r`, `\t`), and other ASCII control characters via
/// `\xNN`.
fn escape_python_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x00'..='\x1f' | '\x7f' => {
                out.push_str(&format!("\\x{:02x}", ch as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::emit;
    use tyc_syntax::parse_module;

    fn round_trip(src: &str) -> String {
        let parsed = parse_module(src).expect("parse failed");
        emit(parsed.syntax())
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
}
