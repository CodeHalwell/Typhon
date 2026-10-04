# Design note: `?`, `as!`, `|>` and `rescue` as AST nodes

**Status:** proposal — nothing here is implemented · **Date:** 2026-10-03 ·
**Fix-list item:** W7-13 (review `claude-opus-5-5-2026-10-03`, §10.3 and §9.5)

**Preconditions met:** W7-01 to W7-08 landed on `fix/W7-claude`
(`5ff3df3a` … `5664cc79`), plus W7-09 (`|>` slot rules, `42587144`) and
W7-12 (`e4f5319a`). The pipe rule W7-09 fixed is the grammar this note
proposes for the parser.

## 1. Problem

Four operators are expanded as **text** before the source reaches the parser:

| Sugar | Text pass(es) today | Lowers to |
|---|---|---|
| postfix `?` (propagate) | `expand_compound_question_headers_mapped`, `expand_inline_question_ops_mapped` (with `preprocess/eval_order.rs`), `expand_question_ops_mapped` | `t = e` / `if isinstance(t, Err): return t` / `t.value` |
| postfix `?` (nullable type) | `preprocess()`'s `rewrite_optionals` | `T \| None`, recorded as `StrippedOptional` for `tyc fmt` |
| `EXPR as! TYPE` | `expand_checked_casts_mapped` (inside `expand_question_ops_mapped`) | `__typhon_checked_cast__(EXPR, TYPE)` |
| `a \|> f(b)` | `expand_pipes_mapped` (+ `join_pipe_continuations`, `preprocess/pipe_slots.rs`) | `f(a, b)` |
| `EXPR rescue N: H` / `rescue N: H:` block | `expand_rescue_mapped`, `expand_rescue_blocks_mapped` | `try_result(lambda: EXPR, lambda N: H)?` / `try`…`except` |

Each pass changes line shape, so every consumer that reports a position has
to undo the expansion on its own, and each one does it differently:

- **Diagnostics** go through a composed line table (`MappedOut`,
  `compose_line_maps`, `PreprocessResult::line_map`) and
  `Diagnostics::remap_lines`, which moves only the primary span — a secondary
  label below a `?` points outside the file.
- **`tyc fmt`** restored `?` by byte column and panicked inside a multi-byte
  character (W7-01); it now uses a marker character, and still has to
  re-lower its own output to check it did not change the program (W7-03).
- **The LSP** maps columns but not lines (W4-07: wrong hover below every `?`).
- **The VM** reports lines of the expanded buffer (W5-19).
- **Evaluation order**: lifting a `?` operand to a statement above moves it
  ahead of everything evaluated before it. W7-04 hoists the earlier siblings
  textually; a receiver read before the operand and the load of an
  augmented-assignment target are still out of order.
- **Lexical gaps**: each pass re-derives strings, comments, brackets and
  statement starts. W7-02, W7-05, W7-09 and W7-12 were all a pass treating
  string text, a continuation line or an f-string field as something else.
- **Pass-chain drift**: the chain is composed by hand in the CLI, the LSP,
  `tyc-db` and twice in the VM (§9.5), in two different orders.

One cause: the parser does not know these forms, so they never carry their
own source range.

## 2. Proposal

Teach the vendored Ruff fork to parse the four forms into AST nodes with real
`TextRange`s, and lower them in Rust over the AST. Every lowered node
inherits the range of the sugar node it came from, so a position anywhere
downstream — a diagnostic span, a `.py.map` entry, an LSP hover, a VM
traceback — is a source position without a line table.

The fork already carries one Typhon extension: `Mutability` on
`StmtAssign` / `StmtAnnAssign` and the `let` / `mut` soft keywords
(`tyc/vendor/README.md`). This proposal adds five nodes on the same pattern.

### 2.1 Tokens (lexer)

| Form | Token | Notes |
|---|---|---|
| `?` | `TokenKind::Question` | Already exists for IPython mode (`lexer.rs`: `'?' if self.mode == Mode::Ipython`); in Python mode `?` lexes as an error token today. Must also lex inside f-string replacement fields (W7-12 (d) is this case). |
| `\|>` | new `TokenKind::PipeGreater` | `\|` immediately followed by `>` is never valid Python, so the token is unambiguous. |
| `as!` | new `TokenKind::AsBang` | `as` immediately followed by `!` (not `!=`). Inside an f-string field a bare `!` starts a conversion; lexing `as!` as one token before that check keeps `f"{x as! int}"` a cast. |
| `rescue` | soft keyword | Like `let` / `mut` / `match`: a keyword only where the grammar below expects it, an identifier everywhere else. |

All four are gated by a new `ParseOptions` flag (`typhon_sugar`), on for
`tyc_syntax::parse_module` and off where the fork parses emitted or plain
Python (`tyc_syntax::ast_equiv`, the W7-04 `eval_order` probes, `tyc migrate`
input), so a `?` in a `.py` file stays a syntax error.

### 2.2 Grammar and precedence

The precedences are the ones the text passes implement today — the parser
must accept exactly what they accept (§4):

- **Postfix `?`** is a trailer, at the level of call, attribute and
  subscript: `primary: primary '?' | …`. `a + b?` is `a + (b?)`;
  `f()?.x` is `(f()?).x`. One node serves both meanings (§3.1).
- **`as!`** takes the whole expression in its slot as its left operand
  (`a + b as! int` casts `a + b`) and a *type expression* as its right
  operand (dotted name, optional subscript, `|` chain), so `x as! int + 1`
  is `(x as! int) + 1`.
- **`|>`** binds looser than every other expression operator, including a
  conditional expression and `lambda` (W7-09): `not 0 |> add(0)` is
  `add(not 0, 0)`. Grammatically it sits between `expression` and the
  element rules that use it: `pipe: expression ('|>' target)*`, wherever
  call arguments, display and comprehension elements, dict keys and values,
  subscripts, slice bounds, keyword arguments, defaults, `return` /
  assignment values, compound-statement headers and f-string fields accept
  an `expression` — exactly the slots `preprocess/pipe_slots.rs` splits on.
  `target` must be a call or a dotted name; anything else is a parse error
  at the `|>` token. The relative precedence of `|>` and `as!` must be
  pinned against the current chain (pipes run first) before step 2.
- **Postfix `rescue`** (v1 scope, unchanged): `EXPR rescue NAME ':' expr`
  only as the value of an assignment, `return`, expression statement, or an
  `if` / `while` / `assert` header. **Block `rescue`**: `rescue NAME ':'
  expr ':' block` at statement start.

### 2.3 Nodes (`ruff_python_ast`)

```text
ExprQuestion     { range, node_index, value: Box<Expr> }
ExprCheckedCast  { range, node_index, value: Box<Expr>, target: Box<Expr> }
ExprPipe         { range, node_index, value: Box<Expr>, call: Box<Expr> }
ExprRescue       { range, node_index, value: Box<Expr>, name: Identifier, handler: Box<Expr> }
StmtRescue       { range, node_index, name: Identifier, handler: Box<Expr>, body: Vec<Stmt> }
```

Each node touches `nodes.rs`, `generated.rs` (the `Expr` / `Stmt` enums,
`ExprRef`, `AnyNodeRef`, `NodeKind`, `walk_expr` / `walk_stmt`, the
source-order visitor and the transformer) and `comparable.rs`. `generated.rs`
says it is produced by upstream's `generate.py` from `ast.toml`, neither of
which is vendored; the `Mutability` field was added by hand. Vendor both
files at the `UPSTREAM` SHA first, so the five nodes are five `ast.toml`
entries and an upstream sync re-generates instead of re-applying ~50 hand
edits.

## 3. Lowering

### 3.1 Where

The pipeline is `tyc-syntax → tyc-resolve → tyc-types → tyc-analyse →
tyc-desugar → tyc-emit`, and the checker runs on the AST *before* desugar.
Two placements:

- **(A) Lower immediately after parsing.** A sugar-lowering module (in
  `tyc-desugar`, called from `tyc_syntax::parse_module`'s callers through one
  function — the same entry point W5-20's canonical chain uses) turns the
  five nodes into plain Python AST whose nodes carry the sugar node's range.
  Resolve, types, analyse, the VM and the emitter see the shapes they see
  today. The code that recognises the lowered shape (`__typhon_q_*`,
  `__typhon_Err__`, `checked_cast`, `try_result` — 40 matching lines in
  `tyc-types`, 35 in `tyc-vm`, 27 in `tyc-desugar`, 11 in the CLI, 8 in
  `tyc-diagnostics`, 4 in `tyc-resolve`) keeps working if the lowering keeps
  today's temporary names.
- **(B) Type the nodes natively.** `tyc-types` gets one arm per node
  (`e?` : `T` for `e: Result[T, E]`, with `E` checked against the enclosing
  function — today recovered from the temp shape), and desugar lowers late.

Do (A) first — it removes the position problem on its own, because the
positions are real — and move individual checks to (B) where the node makes
them simpler. Under (A) only the parser and the lowering ever see the new
nodes; under (B) the exhaustive `match`es on `Expr` outside the fork — the
six that name `Expr::IpyEscapeCommand` (`tyc-analyse` 2, `tyc-emit` 1,
`tyc-resolve` 1, `tyc-syntax` 1, `tyc-vm` 1) — fail to compile until they
handle the new nodes, and every wildcard match must be audited, since a
wildcard silently ignores a node it should have descended into.

### 3.2 `?`: type or value

`ExprQuestion` is classified after parsing, on the AST, replacing the text
heuristics (`question_is_type_position`, `q_contexts` heads,
`question_is_parameter_annotation`). The rules, each a port of a current one:

1. In an annotation (parameter, return, `AnnAssign`), a `type` / `newtype`
   value, a type-parameter bound, or the right side of `as!`: **nullable**,
   lowered to `value | None`.
2. Outside any function (module or class body): **nullable** — a
   propagation lowers to `return`, so it is never that operator there
   (`Provider = Callable[[str, int?], int]`).
3. In a function body, in value position: **propagation**.

`?` inside a comprehension, a lambda, an `and` / `or` operand or a
conditional-expression branch stays `tyc::invalid_question_op` (today's
rule). The AST makes lowering those to control flow possible later — a
widening, so allowed by the compatibility rule — but it is not part of this
change.

### 3.3 Propagation, in evaluation order

For a statement holding propagations `P1 … Pn`, walk its expression tree in
CPython evaluation order. Before `Pk`'s guard, spill to a temporary every
operand evaluated before `Pk` that is not a literal — names and attribute
reads included, which is what closes the W7-04 residuals (`d[k] += f()?`
loads `d[k]` first; `obj.m(f()?)` reads `obj.m` first). Then:

```text
t = <Pk operand>
if isinstance(t, __typhon_Err__): return t
… t.value in place of Pk …
```

Compound headers keep the shapes `expand_compound_question_headers` emits:
an `elif c?:` guard goes into the `else` branch, a `while c?:` guard is
re-evaluated each iteration, `for x in it?:` / `with a? as b:` / `assert x?`
hoist above the statement. Every emitted statement carries the range of the
`?` node it serves.

### 3.4 The other three

- `ExprCheckedCast` → `Call(__typhon_checked_cast__, [value, target])`.
- `ExprPipe` → the target call with `value` inserted as the first positional
  argument (or `target(value)` for a bare callable). No hoisting: the docs
  define `|>` as this rewrite.
- `ExprRescue` → `Call(try_result, [Lambda(value), Lambda(name, handler)])`
  wrapped in an `ExprQuestion` and lowered by §3.3; `StmtRescue` →
  `Try(body, [ExceptHandler(Exception as name, [Return(Err(handler))])])`.

## 4. Compatibility

The additive rule applies: every program the text passes accept must parse
to the same emitted Python, and every program that checks today must still
check. Each migration step is gated on:

1. **Emitted-AST equivalence over the corpus.** The W7 verification
   harness (build every unit in `examples/`, `examples/apps/` and `stress/`
   with the old and new binary, parse both `build/**/*.py`, compare ASTs)
   ran on 1481 units / 1222 building for every W7 commit. Committed as
   `scripts/emitted-ast.py equiv OLD NEW` (see
   [`differential-testing.md`](../differential-testing.md#4-emitted-ast-equivalence-harness)).
   Any diff must be a case where the text lowering was wrong
   (the W7-04 residuals in step 4), listed in the changelog.
2. **Check-result equivalence**: no unit that checks before fails after.
3. The VM ↔ CPython differential.

Parse-error wording and position change for malformed sugar (they now come
from the parser, at the token). That is not a compatibility break: the
program was already rejected.

## 5. Migration steps

Each step is one PR and leaves the tree shippable.

0. **Groundwork.** W5-20's single canonical chain (so removing a pass is one
   edit, not five); vendor `ast.toml` / `generate.py`; commit the
   equivalence harness (done: `scripts/emitted-ast.py`).
1. **Fork.** Tokens, nodes, grammar, `ParseOptions::typhon_sugar` (default
   off); parser snapshot tests for every form and every slot; no consumer
   change. Update `tyc/vendor/README.md` ("Typhon-specific extensions",
   "Syncing with upstream").
2. **`|>`.** Turn the flag on for `|>`; lower `ExprPipe`; drop
   `expand_pipes_mapped` from the chain and delete `pipe_slots.rs` and
   `join_pipe_continuations`. Gate: corpus AST identical; W7-09 tests pass
   unchanged.
3. **`as!`.** Lower `ExprCheckedCast`; drop `expand_checked_casts_mapped`.
4. **`?`.** Classifier (§3.2) and lowering (§3.3); drop
   `expand_compound_question_headers_mapped`,
   `expand_inline_question_ops_mapped` (with `eval_order.rs` and
   `fstring_fields.rs`' `?` use), the `?` half of
   `expand_question_ops_mapped`, `rewrite_optionals`, `StrippedOptional` and
   the fmt marker character. Expected corpus diffs: the evaluation-order
   residuals only. Land behind an internal switch that runs both lowerings
   and compares, for one release.
5. **`rescue`.** Postfix and block; drop `expand_rescue_mapped` and
   `expand_rescue_blocks_mapped`. `?` and `rescue` together remove the
   remaining reason `tyc fmt` re-lowers `?`.
6. **Clean-up.** Remove the line-table plumbing these passes alone needed;
   `Diagnostics::remap_lines` stays for the statement-shaped passes below.
   Update `docs/architecture.md`, `docs/vm.md` and the skill reference.

## 6. Out of scope

The statement-shaped sugar stays textual for now: `with`-chains, `gather`,
`go`, `guard`, `lazy` (imports and lets), typed `let` unpacking, `comptime`,
`pub`, `unsafe`, `freeze`, `frozen`, `newtype`, and the `enum` / `model` /
`interface` / `impl` / `extend` / `class!` / `plain class` headers. They are
line-prefix keywords, which the fork can take as soft keywords the way it
takes `let` / `mut`; that is the natural follow-up and needs the same
groundwork (step 0).

## 7. Costs

- **Fork divergence.** Five nodes and four tokens on top of `Mutability`.
  With `generate.py` vendored the AST side is mechanical on an upstream
  sync; the grammar side is one trailer, one expression-level loop, one
  infix operator and one soft keyword in `parser/expression.rs` /
  `statement.rs`.
- **Checker work** only under (B), and only where it pays.
- **The LSP** gains real ranges for these forms (hover, go-to-definition and
  semantic tokens for `?` / `|>`); its column-mapping code for them goes.
