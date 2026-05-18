# Consumer-crate migration guide: `rustpython_ast` → `ruff_python_ast`

This is a checklist of the exact textual transforms required to port a
Typhon consumer crate from `rustpython_ast` / `rustpython_parser` to the
vendored Ruff back-end.

## 0. Cargo.toml

Remove `rustpython-ast` and `rustpython-parser` from `[dependencies]`. Add
`ruff_python_ast` and (if the crate parses) `ruff_python_parser` from
`{ workspace = true }`. `ruff_text_size` if `TextRange` is used directly.
`tyc-syntax` already re-exports `ruff_python_ast` as `tyc_syntax::ast`
and exposes `tyc_syntax::parse_module(&str) -> Result<Parsed<ModModule>, ParseError>`.

## 1. Imports

```text
use rustpython_ast::text_size::TextRange;   →   use ruff_text_size::TextRange;
use rustpython_ast::{Expr, Mod, Stmt, …};   →   use ruff_python_ast::{Expr, Mod, ModModule, Stmt, …};
use rustpython_parser::{parse, Mode};       →   use tyc_syntax::parse_module;
                                                // or `use ruff_python_parser::parse_module;`
```

## 2. Type signatures — drop the `TextRange` generic

| rustpython                       | ruff                       |
|----------------------------------|----------------------------|
| `Mod<TextRange>`                 | `ModModule` (when you've parsed; the bare `Mod` enum still exists but `parse_module` returns `Parsed<ModModule>`) |
| `Stmt<TextRange>`                | `Stmt`                     |
| `Expr<TextRange>`                | `Expr`                     |
| `ExceptHandler<TextRange>`       | `ExceptHandler`            |
| `Arguments<TextRange>`           | `Parameters`               |
| `Arg<TextRange>`                 | `Parameter`                |
| `ArgWithDefault<TextRange>`      | `ParameterWithDefault`     |
| `Keyword<TextRange>`             | `Keyword`                  |
| `Comprehension<TextRange>`       | `Comprehension`            |
| `WithItem<TextRange>`            | `WithItem`                 |
| `TypeParam<TextRange>`           | `TypeParam`                |
| `Pattern<TextRange>`             | `Pattern`                  |
| `MatchCase<TextRange>`           | `MatchCase`                |
| `Alias<TextRange>`               | `Alias`                    |
| `StmtFunctionDef<TextRange>`     | `StmtFunctionDef`          |
| `ExprCall<TextRange>`            | `ExprCall`                 |
| (all `Stmt*<TextRange>` / `Expr*<TextRange>`) | (drop the generic) |

## 3. Parsing entry point

```rust
// rustpython
let module: Mod<TextRange> = parse(&src, Mode::Module, &path)?;
let Mod::Module(m) = &module else { unreachable!() };
for stmt in &m.body { … }

// ruff (via tyc-syntax re-export)
let parsed = tyc_syntax::parse_module(&src)
    .map_err(|e| TycError::parse(&path, &src, e.to_string(), usize::from(e.location.start())))?;
let module: &ModModule = parsed.syntax();          // borrow
let module_owned: ModModule = parsed.into_syntax(); // take ownership
for stmt in &module.body { … }
```

`Parsed<ModModule>` and `ParseError` come from `ruff_python_parser`
(re-exported as `tyc_syntax::Parsed` / `tyc_syntax::ParseError`).
`ParseError.location: TextRange` (use `.start()` for the offset).

If the surrounding code still passes a `Mod` enum around, you can
construct one with `Mod::Module(parsed.into_syntax())` — but prefer
threading `ModModule` directly.

## 4. Constants → typed literal Exprs

This is the biggest mechanical change. Every `Expr::Constant(ExprConstant { value: Constant::… })`
match becomes a match on a dedicated `Expr::*Literal` variant.

| rustpython                                                                 | ruff                                                       |
|----------------------------------------------------------------------------|------------------------------------------------------------|
| `Expr::Constant(c) if matches!(c.value, Constant::Int(_))`                 | `Expr::NumberLiteral(n) if matches!(n.value, Number::Int(_))` |
| `Expr::Constant(c) if matches!(c.value, Constant::Float(_))`               | `Expr::NumberLiteral(n) if matches!(n.value, Number::Float(_))` |
| `Expr::Constant(c) if matches!(c.value, Constant::Complex { .. })`         | `Expr::NumberLiteral(n) if matches!(n.value, Number::Complex { .. })` |
| `Expr::Constant(c) if matches!(c.value, Constant::Str(_))`                 | `Expr::StringLiteral(_)`                                   |
| `Expr::Constant(c) if matches!(c.value, Constant::Bytes(_))`               | `Expr::BytesLiteral(_)`                                    |
| `Expr::Constant(c) if matches!(c.value, Constant::Bool(_))`                | `Expr::BooleanLiteral(_)`                                  |
| `Expr::Constant(c) if matches!(c.value, Constant::None)`                   | `Expr::NoneLiteral(_)`                                     |
| `Expr::Constant(c) if matches!(c.value, Constant::Ellipsis)`               | `Expr::EllipsisLiteral(_)`                                 |

Field shapes:

- `ExprNumberLiteral { value: Number, range, node_index }` where
  `Number = Int(int::Int) | Float(f64) | Complex { real: f64, imag: f64 }`.
  Use `Number::is_int()`, `Number::is_float()` for `matches!` shortcuts.
- `ExprStringLiteral { value: StringLiteralValue, range, node_index }`.
  `value.to_str()` returns the concatenated `&str` of all implicit-concat parts.
  `value.is_implicit_concatenated()` is true when there were multiple parts.
- `ExprBytesLiteral { value: BytesLiteralValue, range, node_index }`.
- `ExprBooleanLiteral { value: bool, range, node_index }` — `Default` available.
- `ExprNoneLiteral { range, node_index }` — `Default` available.
- `ExprEllipsisLiteral { range, node_index }` — `Default` available.

`int::Int` (from `ruff_python_ast::int`) exposes `as_i64()`, `as_u64()`,
`as_u32()`, etc., plus `Display`. `From<u64>` / `From<i64>` for
constructing literals.

## 5. Async folding

There is no `Stmt::AsyncFunctionDef`, `Stmt::AsyncFor`, or
`Stmt::AsyncWith`. The synchronous variants have an `is_async: bool` field.

```rust
// rustpython
Stmt::AsyncFunctionDef(f)  →  Stmt::FunctionDef(f) if f.is_async
Stmt::AsyncFor(f)          →  Stmt::For(f) if f.is_async
Stmt::AsyncWith(w)         →  Stmt::With(w) if w.is_async
```

If the existing code matches both sync and async forms separately, fold
them into a single match arm and branch on `is_async` if behaviour
differs.

## 6. Parameters — `args` field renamed to `parameters`

`StmtFunctionDef` and `StmtLambda` both rename their argument list:

```text
rustpython StmtFunctionDef          ruff StmtFunctionDef
  args: Box<Arguments<TextRange>>     parameters: Box<Parameters>
                                      is_async: bool                (new!)
```

`Parameters` is structurally the same as `Arguments`:

```rust
pub struct Parameters {
    pub posonlyargs: Vec<ParameterWithDefault>,
    pub args: Vec<ParameterWithDefault>,
    pub vararg: Option<Box<Parameter>>,
    pub kwonlyargs: Vec<ParameterWithDefault>,
    pub kwarg: Option<Box<Parameter>>,
    pub range: TextRange,
    pub node_index: AtomicNodeIndex,
}
```

`Parameter` (was `Arg`):

```text
rustpython Arg              ruff Parameter
  arg: Identifier             name: Identifier
  annotation: Option<Box<Expr>>  annotation: Option<Box<Expr>>
  type_comment: Option<String>  (gone — Ruff parses type comments separately)
  range: TextRange            range: TextRange
                              node_index: AtomicNodeIndex
```

`ParameterWithDefault` (was `ArgWithDefault`):

```text
rustpython ArgWithDefault         ruff ParameterWithDefault
  def: Arg                          parameter: Parameter
  default: Option<Box<Expr>>        default: Option<Box<Expr>>
  range: TextRange                  range: TextRange
                                    node_index: AtomicNodeIndex
```

So:

```text
f.args.args                       →  f.parameters.args
f.args.kwonlyargs                 →  f.parameters.kwonlyargs
arg.arg                           →  parameter.name
arg.arg.as_str()                  →  parameter.name.as_str()
arg_with_default.def              →  param_with_default.parameter
arg_with_default.def.arg          →  param_with_default.parameter.name
arg_with_default.def.arg.as_str() →  param_with_default.parameter.name.as_str()
```

## 7. ClassDef arguments — bases/keywords moved into an Option<Arguments>

```text
rustpython StmtClassDef           ruff StmtClassDef
  bases: Vec<Expr<TextRange>>       arguments: Option<Box<Arguments>>
  keywords: Vec<Keyword<TextRange>>   where Arguments { args: Vec<Expr>, keywords: Vec<Keyword>, … }
  range, decorator_list, body…      (same, but body uses Stmt not Stmt<TextRange>)
```

`StmtClassDef` already has helper methods so most call-sites just need:

```rust
// rustpython
for base in &c.bases { … }
for kw in &c.keywords { … }

// ruff
for base in c.bases() { … }       // returns &[Expr]
for kw in c.keywords() { … }      // returns &[Keyword]
```

To build one programmatically, set `arguments: Some(Box::new(Arguments {
args: vec![…], keywords: vec![…], range: …, node_index: AtomicNodeIndex::NONE }))`.

## 8. ExprName.id is now `Name`, not `Identifier`/`String`

`Name` is a small-string-optimised type from `ruff_python_ast::name`.
`name.as_str()` works. Construct with `Name::new(s)` or `Name::from(s)`.

```rust
// rustpython
ExprName { id: Identifier::new("x"), ctx: ExprContext::Load, range }

// ruff
ExprName { id: Name::new("x"), ctx: ExprContext::Load, range, node_index: AtomicNodeIndex::NONE }
```

## 9. Synthesised AST nodes need `range` AND `node_index`

Every ruff AST node has two extra fields compared to rustpython:

- `range: TextRange` — use `TextRange::default()` (== empty range at 0)
  for fully-synthesised nodes; copy from a nearby real node when you can,
  so source maps line up.
- `node_index: AtomicNodeIndex` — use `AtomicNodeIndex::NONE` for
  synthesised nodes. `AtomicNodeIndex` is imported from
  `ruff_python_ast::AtomicNodeIndex`.

Pattern for any synthesised node:

```rust
use ruff_python_ast::{AtomicNodeIndex, …};
use ruff_text_size::TextRange;

Expr::Name(ExprName {
    id: Name::new("foo"),
    ctx: ExprContext::Load,
    range: TextRange::default(),
    node_index: AtomicNodeIndex::NONE,
})
```

## 10. Identifier construction

`Identifier { id: Name, range: TextRange, node_index: AtomicNodeIndex }`.

```rust
// Easiest: use the constructor.
Identifier::new("foo", TextRange::default())
// Manual: fields.
Identifier { id: Name::new("foo"), range: TextRange::default(), node_index: AtomicNodeIndex::NONE }
```

`identifier.as_str()` works (Identifier `Deref`s to `str`).

## 11. ExceptHandler — same nested enum shape

`ExceptHandler::ExceptHandler(ExceptHandlerExceptHandler { … })` exists
in both. Field names are unchanged: `type_`, `name`, `body`, `range`.

## 12. Comprehension is_async: usize → bool

```rust
// rustpython:  c.is_async: usize  (0 or 1)
// ruff:        c.is_async: bool
```

Anywhere `c.is_async == 1` appears, change to `c.is_async`.

## 13. Numeric integer payloads

```rust
// rustpython:  Constant::Int(value)  where value: BigInt
let i: i64 = value.to_i64()?;

// ruff:        Number::Int(value)  where value: int::Int
let i: i64 = value.as_i64()?;       // returns Option<i64>
let s: String = value.to_string();   // Display impl
```

Constructing a literal int:

```rust
ExprNumberLiteral {
    value: Number::Int(int::Int::from(42_u64)),
    range: TextRange::default(),
    node_index: AtomicNodeIndex::NONE,
}
```

## 14. String literal construction

Most of our codebase constructs string literals to inject into the
emitted Python. Build them like this:

```rust
use ruff_python_ast::{
    AtomicNodeIndex, ExprStringLiteral, StringLiteral, StringLiteralFlags,
    StringLiteralValue,
};

let lit = StringLiteral {
    range: TextRange::default(),
    node_index: AtomicNodeIndex::NONE,
    value: Box::from("hello"),
    flags: StringLiteralFlags::empty(),
};
Expr::StringLiteral(ExprStringLiteral {
    range: TextRange::default(),
    node_index: AtomicNodeIndex::NONE,
    value: StringLiteralValue::single(lit),
})
```

(`StringLiteralValue::single(lit)` is the canonical single-part
constructor; use `StringLiteralValue::concatenated(parts)` for
implicit concatenation.)

## 15. `Mod::Module(_)` pattern still works

The `Mod` enum still has `Module(ModModule)` and `Expression(ModExpression)`
variants. Old `let Mod::Module(m) = module else { … }` patterns continue
to work, you just don't need them most of the time because
`parse_module` returns a `Parsed<ModModule>` directly.

## 16. Don't try to `cargo build` in isolation

Multiple crates migrate together. Until every consumer is on the ruff
AST, the workspace will not compile. Verify your changes by:

1. Reading carefully — make sure every site that touched the rustpython
   types now uses the ruff equivalent.
2. Grep for `rustpython_` in your file(s) after the change — there
   should be none.
3. Grep for `<TextRange>` — there should be none in type signatures.
