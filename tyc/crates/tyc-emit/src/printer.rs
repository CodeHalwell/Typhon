//! Hand-written Python pretty-printer.
//!
//! Covers the Python 3 grammar subset used in Phase 0 round-trip testing.
//! Aim: parse → emit produces output that is semantically equivalent to the
//! input (whitespace and comment differences are acceptable in Phase 0).

use rustpython_ast::{
    text_size::TextRange, Alias, ArgWithDefault, BoolOp, CmpOp, Comprehension, ExceptHandler,
    Expr, Keyword, MatchCase, Mod, Operator, Pattern, Stmt, UnaryOp, WithItem,
};

/// Internal state for the Python pretty-printer.
pub struct Emitter {
    output: String,
    indent: usize,
    /// When true, inject `@dataclass(slots=True)` before bare class defs.
    inject_dataclass: bool,
    /// When true, add `BaseModel` as the base class on bare class defs.
    inject_pydantic: bool,
}

const INDENT_WIDTH: usize = 4;

impl Emitter {
    pub fn new() -> Self {
        Self {
            output: String::new(),
            indent: 0,
            inject_dataclass: false,
            inject_pydantic: false,
        }
    }

    /// Create an emitter configured for compiled Typhon output.
    pub fn new_compiled(options: &crate::EmitOptions) -> Self {
        Self {
            output: String::new(),
            indent: 0,
            inject_dataclass: options.inject_dataclass,
            inject_pydantic: options.inject_pydantic,
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
        self.output.push('\n');
    }

    fn newline(&mut self) {
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

    pub fn emit_mod(&mut self, node: &Mod) {
        match node {
            Mod::Module(m) => {
                for stmt in &m.body {
                    self.emit_stmt(stmt);
                }
            }
            Mod::Interactive(i) => {
                for stmt in &i.body {
                    self.emit_stmt(stmt);
                }
            }
            Mod::Expression(e) => {
                self.emit_expr(&e.body);
                self.newline();
            }
            Mod::FunctionType(_) => {}
        }
    }

    // ── statements ─────────────────────────────────────────────────────────

    pub fn emit_stmt(&mut self, node: &Stmt<TextRange>) {
        match node {
            Stmt::FunctionDef(f) => {
                self.newline();
                for decorator in &f.decorator_list {
                    self.fill("@");
                    self.emit_expr(decorator);
                    self.newline();
                }
                self.fill("def ");
                self.write(f.name.as_str());
                self.write("(");
                self.emit_arguments(&f.args);
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

            Stmt::AsyncFunctionDef(f) => {
                self.newline();
                for decorator in &f.decorator_list {
                    self.fill("@");
                    self.emit_expr(decorator);
                    self.newline();
                }
                self.fill("async def ");
                self.write(f.name.as_str());
                self.write("(");
                self.emit_arguments(&f.args);
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
                // Emit existing decorators first.
                for decorator in &c.decorator_list {
                    self.fill("@");
                    self.emit_expr(decorator);
                    self.newline();
                }
                // Inject @dataclass(slots=True) if the class is not already decorated.
                if self.inject_dataclass && !self.has_dataclass_decorator(&c.decorator_list) {
                    self.fill("@dataclass(slots=True)");
                    self.newline();
                }
                self.fill("class ");
                self.write(c.name.as_str());
                // When emitting for pydantic, add BaseModel as the base class
                // if the class doesn't already extend it.
                let inject_base_model = self.inject_pydantic
                    && c.bases.is_empty()
                    && c.keywords.is_empty()
                    && !self.has_base_model_base(&c.bases);
                if inject_base_model {
                    self.write("(BaseModel)");
                } else if !c.bases.is_empty() || !c.keywords.is_empty() {
                    self.write("(");
                    let mut first = true;
                    for base in &c.bases {
                        if !first {
                            self.write(", ");
                        }
                        self.emit_expr(base);
                        first = false;
                    }
                    for kw in &c.keywords {
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

            Stmt::Assign(a) => {
                self.fill("");
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

            Stmt::AnnAssign(a) => {
                self.fill("");
                self.emit_expr(&a.target);
                self.write(": ");
                self.emit_expr(&a.annotation);
                if let Some(val) = &a.value {
                    self.write(" = ");
                    self.emit_expr(val);
                }
                self.newline();
            }

            Stmt::For(f) => {
                self.fill("for ");
                self.emit_expr(&f.target);
                self.write(" in ");
                self.emit_expr(&f.iter);
                self.writeln(":");
                self.enter_block();
                for stmt in &f.body {
                    self.emit_stmt(stmt);
                }
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

            Stmt::AsyncFor(f) => {
                self.fill("async for ");
                self.emit_expr(&f.target);
                self.write(" in ");
                self.emit_expr(&f.iter);
                self.writeln(":");
                self.enter_block();
                for stmt in &f.body {
                    self.emit_stmt(stmt);
                }
                self.leave_block();
            }

            Stmt::While(w) => {
                self.fill("while ");
                self.emit_expr(&w.test);
                self.writeln(":");
                self.enter_block();
                for stmt in &w.body {
                    self.emit_stmt(stmt);
                }
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

            Stmt::If(i) => {
                self.fill("if ");
                self.emit_expr(&i.test);
                self.writeln(":");
                self.enter_block();
                for stmt in &i.body {
                    self.emit_stmt(stmt);
                }
                self.leave_block();
                // Emit elif/else clauses from the orelse list.
                if i.orelse.len() == 1 {
                    if let Stmt::If(elif) = &i.orelse[0] {
                        self.fill("elif ");
                        self.emit_expr(&elif.test);
                        self.writeln(":");
                        self.enter_block();
                        for s in &elif.body {
                            self.emit_stmt(s);
                        }
                        self.leave_block();
                        for s in &elif.orelse {
                            self.emit_stmt(s);
                        }
                        return;
                    }
                }
                if !i.orelse.is_empty() {
                    self.fill("else:");
                    self.newline();
                    self.enter_block();
                    for stmt in &i.orelse {
                        self.emit_stmt(stmt);
                    }
                    self.leave_block();
                }
            }

            Stmt::With(w) => {
                self.fill("with ");
                for (idx, item) in w.items.iter().enumerate() {
                    if idx > 0 {
                        self.write(", ");
                    }
                    self.emit_with_item(item);
                }
                self.writeln(":");
                self.enter_block();
                for stmt in &w.body {
                    self.emit_stmt(stmt);
                }
                self.leave_block();
            }

            Stmt::AsyncWith(w) => {
                self.fill("async with ");
                for (idx, item) in w.items.iter().enumerate() {
                    if idx > 0 {
                        self.write(", ");
                    }
                    self.emit_with_item(item);
                }
                self.writeln(":");
                self.enter_block();
                for stmt in &w.body {
                    self.emit_stmt(stmt);
                }
                self.leave_block();
            }

            Stmt::Match(m) => {
                self.fill("match ");
                self.emit_expr(&m.subject);
                self.writeln(":");
                self.enter_block();
                for case in &m.cases {
                    self.emit_match_case(case);
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

            Stmt::Try(t) => {
                self.fill("try:");
                self.newline();
                self.enter_block();
                for stmt in &t.body {
                    self.emit_stmt(stmt);
                }
                self.leave_block();
                for handler in &t.handlers {
                    self.emit_except_handler(handler, false);
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

            Stmt::TryStar(t) => {
                self.fill("try:");
                self.newline();
                self.enter_block();
                for stmt in &t.body {
                    self.emit_stmt(stmt);
                }
                self.leave_block();
                for handler in &t.handlers {
                    self.emit_except_handler(handler, true);
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

            Stmt::ImportFrom(i) => {
                self.fill("from ");
                if let Some(level) = i.level {
                    let dots: String = ".".repeat(level.to_usize());
                    self.write(&dots);
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
                self.write(" = ");
                self.emit_expr(&t.value);
                self.newline();
            }
        }
    }

    // ── expressions ────────────────────────────────────────────────────────

    pub fn emit_expr(&mut self, node: &Expr<TextRange>) {
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

            Expr::NamedExpr(n) => {
                self.emit_expr(&n.target);
                self.write(" := ");
                self.emit_expr(&n.value);
            }

            Expr::BinOp(b) => {
                self.emit_expr(&b.left);
                self.write(" ");
                self.write(op_symbol(&b.op));
                self.write(" ");
                self.emit_expr(&b.right);
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
                self.write("lambda ");
                self.emit_arguments(&l.args);
                self.write(": ");
                self.emit_expr(&l.body);
            }

            Expr::IfExp(i) => {
                self.emit_expr(&i.body);
                self.write(" if ");
                self.emit_expr(&i.test);
                self.write(" else ");
                self.emit_expr(&i.orelse);
            }

            Expr::Dict(d) => {
                self.write("{");
                let mut first = true;
                for (key, val) in d.keys.iter().zip(d.values.iter()) {
                    if !first {
                        self.write(", ");
                    }
                    if let Some(k) = key {
                        self.emit_expr(k);
                        self.write(": ");
                        self.emit_expr(val);
                    } else {
                        self.write("**");
                        self.emit_expr(val);
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

            Expr::DictComp(d) => {
                self.write("{");
                self.emit_expr(&d.key);
                self.write(": ");
                self.emit_expr(&d.value);
                for gen in &d.generators {
                    self.emit_comprehension(gen);
                }
                self.write("}");
            }

            Expr::GeneratorExp(g) => {
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

            Expr::Call(c) => {
                self.emit_expr(&c.func);
                self.write("(");
                let mut first = true;
                for arg in &c.args {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_expr(arg);
                    first = false;
                }
                for kw in &c.keywords {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_keyword(kw);
                    first = false;
                }
                self.write(")");
            }

            Expr::FormattedValue(f) => {
                // Part of an f-string — emit the inner expression.
                self.emit_expr(&f.value);
            }

            Expr::JoinedStr(j) => {
                // f-string literal reconstruction.
                self.write("f\"");
                for part in &j.values {
                    match part {
                        Expr::Constant(c) => {
                            if let rustpython_ast::Constant::Str(s) = &c.value {
                                self.write(s.as_str());
                            }
                        }
                        Expr::FormattedValue(fv) => {
                            self.write("{");
                            self.emit_expr(&fv.value);
                            self.write("}");
                        }
                        _ => {}
                    }
                }
                self.write("\"");
            }

            Expr::Constant(c) => {
                self.emit_constant(&c.value);
            }

            Expr::Attribute(a) => {
                self.emit_expr(&a.value);
                self.write(".");
                self.write(a.attr.as_str());
            }

            Expr::Subscript(s) => {
                self.emit_expr(&s.value);
                self.write("[");
                self.emit_expr(&s.slice);
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
        }
    }

    // ── helpers ────────────────────────────────────────────────────────────

    fn emit_constant(&mut self, c: &rustpython_ast::Constant) {
        use rustpython_ast::Constant;
        match c {
            Constant::None => self.write("None"),
            Constant::Bool(b) => self.write(if *b { "True" } else { "False" }),
            Constant::Str(s) => {
                self.write("\"");
                let escaped = s.as_str().replace('\\', "\\\\").replace('"', "\\\"");
                self.write(&escaped);
                self.write("\"");
            }
            Constant::Bytes(b) => {
                self.write("b\"");
                for byte in b {
                    self.write(&format!("\\x{:02x}", byte));
                }
                self.write("\"");
            }
            Constant::Int(i) => {
                self.write(&i.to_string());
            }
            Constant::Tuple(elems) => {
                self.write("(");
                let mut first = true;
                for elem in elems {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_constant(elem);
                    first = false;
                }
                self.write(")");
            }
            Constant::Float(f) => {
                self.write(&format!("{}", f));
            }
            Constant::Complex { real, imag } => {
                if *real != 0.0 {
                    self.write(&format!("{}+", real));
                }
                self.write(&format!("{}j", imag));
            }
            Constant::Ellipsis => self.write("..."),
        }
    }

    fn emit_arguments(&mut self, args: &rustpython_ast::Arguments<TextRange>) {
        let mut first = true;

        // Positional-only args (before /)
        for arg in &args.posonlyargs {
            if !first {
                self.write(", ");
            }
            self.emit_arg_with_default(arg);
            first = false;
        }
        if !args.posonlyargs.is_empty() {
            self.write(", /");
        }

        // Regular args (each carries its own optional default)
        for arg in &args.args {
            if !first {
                self.write(", ");
            }
            self.emit_arg_with_default(arg);
            first = false;
        }

        // *args
        if let Some(vararg) = &args.vararg {
            if !first {
                self.write(", ");
            }
            self.write("*");
            self.emit_plain_arg(vararg);
            first = false;
        } else if !args.kwonlyargs.is_empty() {
            if !first {
                self.write(", ");
            }
            self.write("*");
            first = false;
        }

        // Keyword-only args (each carries its own optional default)
        for arg in &args.kwonlyargs {
            if !first {
                self.write(", ");
            }
            self.emit_arg_with_default(arg);
            first = false;
        }

        // **kwargs
        if let Some(kwarg) = &args.kwarg {
            if !first {
                self.write(", ");
            }
            self.write("**");
            self.emit_plain_arg(kwarg);
        }
    }

    /// Emit an `ArgWithDefault` — the arg name, optional annotation, and
    /// optional default value.
    fn emit_arg_with_default(&mut self, arg: &ArgWithDefault<TextRange>) {
        self.emit_plain_arg(&arg.def);
        if let Some(default) = &arg.default {
            self.write(" = ");
            self.emit_expr(default);
        }
    }

    /// Emit a plain `Arg` (name + optional annotation, no default).
    fn emit_plain_arg(&mut self, arg: &rustpython_ast::Arg<TextRange>) {
        self.write(arg.arg.as_str());
        if let Some(ann) = &arg.annotation {
            self.write(": ");
            self.emit_expr(ann);
        }
    }

    fn emit_alias(&mut self, alias: &Alias<TextRange>) {
        self.write(alias.name.as_str());
        if let Some(asname) = &alias.asname {
            self.write(" as ");
            self.write(asname.as_str());
        }
    }

    fn emit_keyword(&mut self, kw: &Keyword<TextRange>) {
        if let Some(arg) = &kw.arg {
            self.write(arg.as_str());
            self.write("=");
        } else {
            self.write("**");
        }
        self.emit_expr(&kw.value);
    }

    fn emit_comprehension(&mut self, gen: &Comprehension<TextRange>) {
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

    fn emit_with_item(&mut self, item: &WithItem<TextRange>) {
        self.emit_expr(&item.context_expr);
        if let Some(var) = &item.optional_vars {
            self.write(" as ");
            self.emit_expr(var);
        }
    }

    fn emit_except_handler(&mut self, handler: &ExceptHandler<TextRange>, star: bool) {
        match handler {
            ExceptHandler::ExceptHandler(h) => {
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
                for stmt in &h.body {
                    self.emit_stmt(stmt);
                }
                self.leave_block();
            }
        }
    }

    fn emit_match_case(&mut self, case: &MatchCase<TextRange>) {
        self.fill("case ");
        self.emit_pattern(&case.pattern);
        if let Some(guard) = &case.guard {
            self.write(" if ");
            self.emit_expr(guard);
        }
        self.writeln(":");
        self.enter_block();
        for stmt in &case.body {
            self.emit_stmt(stmt);
        }
        self.leave_block();
    }

    fn emit_pattern(&mut self, pattern: &Pattern<TextRange>) {
        match pattern {
            Pattern::MatchValue(v) => self.emit_expr(&v.value),
            Pattern::MatchSingleton(s) => self.emit_constant(&s.value),
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
            Pattern::MatchClass(c) => {
                self.emit_expr(&c.cls);
                self.write("(");
                let mut first = true;
                for p in &c.patterns {
                    if !first {
                        self.write(", ");
                    }
                    self.emit_pattern(p);
                    first = false;
                }
                for (key, p) in c.kwd_attrs.iter().zip(c.kwd_patterns.iter()) {
                    if !first {
                        self.write(", ");
                    }
                    self.write(key.as_str());
                    self.write("=");
                    self.emit_pattern(p);
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

impl Emitter {
    /// Returns true if `expr` refers to any form of the `dataclass` symbol:
    /// bare name, dotted access (`dataclasses.dataclass`), or call.
    fn is_dataclass_expr(expr: &Expr<TextRange>) -> bool {
        match expr {
            Expr::Name(n) => n.id.as_str() == "dataclass",
            Expr::Attribute(a) => a.attr.as_str() == "dataclass",
            Expr::Call(c) => Self::is_dataclass_expr(&c.func),
            _ => false,
        }
    }

    fn has_dataclass_decorator(&self, decorators: &[Expr<TextRange>]) -> bool {
        decorators.iter().any(|d| Self::is_dataclass_expr(d))
    }

    fn has_base_model_base(&self, bases: &[Expr<TextRange>]) -> bool {
        bases.iter().any(|b| match b {
            Expr::Name(n) => n.id.as_str() == "BaseModel",
            Expr::Attribute(a) => a.attr.as_str() == "BaseModel",
            _ => false,
        })
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

#[cfg(test)]
mod tests {
    use crate::emit;
    use rustpython_parser::{parse, Mode};

    fn round_trip(src: &str) -> String {
        let ast = parse(src, Mode::Module, "<test>").expect("parse failed");
        emit(&ast)
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
}
