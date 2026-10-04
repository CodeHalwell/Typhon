//! Regression tests for the 2026-10-03 review's W7 workstream (preprocessor,
//! `tyc fmt`, lowering). Kept in their own integration-test file so they do
//! not collide with concurrent edits to `preprocess.rs`'s inline test module.

use tyc_syntax::preprocess::{postprocess_full, StrippedOptional};

/// W7-01: a stale `?` column must never index a line inside a multi-byte
/// character. `postprocess_full` used to slice `line[col..]` unchecked and
/// panicked with "byte index N is not a char boundary".
#[test]
fn postprocess_skips_a_stale_optional_column_inside_a_multibyte_char() {
    let normalised = "def f(a: int, b: Üü | None) -> int:\n";
    // Column 18 is inside `Ü` (bytes 17..19).
    let optionals = [StrippedOptional {
        line_index: 0,
        python_col: 18,
    }];
    let out = postprocess_full(normalised, &[], &optionals, &[]);
    assert_eq!(out, normalised, "a stale column is skipped, not applied");
}

/// The unshifted case still restores.
#[test]
fn postprocess_restores_an_exact_optional_column() {
    let normalised = "x: int | None = None\n";
    let optionals = [StrippedOptional {
        line_index: 0,
        python_col: 6,
    }];
    let out = postprocess_full(normalised, &[], &optionals, &[]);
    assert_eq!(out, "x: int? = None\n");
}

// ── W7-02: string state across a backslash-continued line ────────────────────

use tyc_syntax::lexmask::{scan_line_kinds, ByteKind, LexMask};
use tyc_syntax::preprocess::{expand_sugar, preprocess};

/// Build the Python the check/build pipeline parses.
fn lower(src: &str) -> String {
    preprocess(&expand_sugar(src, true)).python_source
}

/// Whether the lowered Python parses.
fn parses(src: &str) -> bool {
    tyc_syntax::parse_module(src).is_ok()
}

#[test]
fn lexmask_carries_a_backslash_continued_double_quoted_string() {
    // A backslash before the newline continues a single-line string literal
    // onto the next physical line; the continuation is string text.
    let src = "b = \"a,b\\\n    c,d  e#f\"\nprint(b)\n";
    let mask = LexMask::new(src);
    assert!(
        mask.line_starts_in_string(1),
        "line 2 starts inside the string"
    );
    assert!(
        !mask.line_starts_in_string(2),
        "the string closed on line 2"
    );
    // `#f` on the continuation is string text, not a comment, so the code
    // portion of line 2 runs to the end of the line.
    let line2 = "    c,d  e#f\"\n";
    assert_eq!(mask.line_code_end(1), line2.len() - 1);
}

#[test]
fn lexmask_carries_a_backslash_continued_single_quoted_string() {
    let src = "b = 'x?\\\ny as! z'\nq = 1\n";
    let mask = LexMask::new(src);
    assert!(mask.line_starts_in_string(1));
    assert!(!mask.line_starts_in_string(2));
}

#[test]
fn lexmask_escaped_backslash_at_end_of_line_is_not_a_continuation() {
    // `"a\\"` ends with an *escaped backslash*: the string is closed.
    let src = "b = \"a\\\\\"\nq = 1\n";
    let mask = LexMask::new(src);
    assert!(!mask.line_starts_in_string(1));
}

#[test]
fn scan_line_kinds_threads_a_continued_string_without_terminators() {
    // Callers that thread `in_string` over `str::lines()` (no terminator)
    // must see the same carry.
    let mut state = None;
    let _ = scan_line_kinds("b = \"a,b\\", &mut state);
    assert!(state.is_some(), "the open string is carried");
    let kinds = scan_line_kinds("    c,d  e#f\"", &mut state);
    assert!(state.is_none(), "the string closed");
    assert!(kinds.iter().all(|k| *k == ByteKind::StringText));
}

#[test]
fn sugar_inside_a_backslash_continued_string_is_left_alone() {
    // `?`, `as!` and `rescue` on the continuation line of a single-quoted
    // string were rewritten as code: `tyc::parse` on valid source, or a
    // spurious module-level-rescue error.
    for src in [
        "def f() -> str:\n    let s: str = \"what\\\n x? y\"\n    return s\n",
        "def f() -> str:\n    let s: str = \"what\\\n x as! int y\"\n    return s\n",
        "def f() -> str:\n    let s: str = \"what\\\n rescue e: oops\"\n    return s\n",
        "S: str = 'a\\\nrescue e: b'\n",
    ] {
        let out = lower(src);
        assert!(
            parses(&out),
            "lowered source must parse:\n{src}\n---\n{out}"
        );
        assert!(
            !out.contains("__typhon") && !out.contains("try_result"),
            "string content was rewritten as code:\n{out}"
        );
    }
}

// ── W7-04: inline `?` keeps Python evaluation order ──────────────────────────

use tyc_syntax::preprocess::expand_inline_question_ops;

/// Line index of the first line containing `needle`.
fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("`{needle}` not found in:\n{text}"))
}

#[test]
fn earlier_keyword_argument_is_hoisted_before_the_lifted_operand() {
    let src = "def node(self) -> Result[Node, str]:\n    return Ok(Node(start=self.pos, text=self.word()?))\n";
    let out = expand_inline_question_ops(src);
    let ev = line_of(&out, "__typhon_ev_0__ = self.pos");
    let qi = line_of(&out, "__typhon_qi_0__ = self.word()");
    assert!(ev < qi, "self.pos must be read before word() runs:\n{out}");
    assert!(out.contains("start=__typhon_ev_0__"), "{out}");
}

#[test]
fn earlier_positional_argument_and_list_element_are_hoisted() {
    let out = expand_inline_question_ops(
        "def run() -> Result[int, str]:\n    return Ok(combine(first(), second()?))\n",
    );
    assert!(
        line_of(&out, "= first()") < line_of(&out, "= second()"),
        "{out}"
    );
    let out = expand_inline_question_ops(
        "def f() -> Result[int, str]:\n    let xs: list[str] = [side(\"a\"), counter(\"b\")?, side(\"c\")]\n    return Ok(0)\n",
    );
    assert!(
        line_of(&out, "= side(\"a\")") < line_of(&out, "= counter(\"b\")"),
        "{out}"
    );
    // The element after the operand stays where it is (evaluated after it).
    assert!(out.contains("side(\"c\")]"), "{out}");
}

#[test]
fn assignment_value_is_hoisted_before_a_target_operand() {
    // Python evaluates the right-hand side before the subscript target.
    let out = expand_inline_question_ops(
        "def f(d: dict[str, int]) -> Result[int, str]:\n    d[k()?] = v()?\n    return Ok(0)\n",
    );
    assert!(
        line_of(&out, "__typhon_ev_0__ = v()?") < line_of(&out, "= k()"),
        "{out}"
    );
    assert!(out.contains("] = __typhon_ev_0__"), "{out}");
}

#[test]
fn trivial_siblings_and_name_or_module_receivers_are_left_alone() {
    for src in [
        "def f(a: int) -> Result[int, str]:\n    return Ok(add(a, parse(s)?))\n",
        "def f(out: list[int]) -> Result[int, str]:\n    out.append(parse(s)?)\n    return Ok(0)\n",
        "import os\ndef f() -> Result[str, str]:\n    return Ok(os.path.join(\"a\", parse(s)?))\n",
        "def f() -> Result[int, str]:\n    return Ok(g([], {}, 1, \"s\", None, lambda x: x, parse(s)?))\n",
        "def f() -> Result[int, str]:\n    let x: int = parse(s)?\n    return Ok(x)\n",
        // A local target no call can rebind keeps its augmented assignment.
        "def f() -> Result[int, str]:\n    mut total: int = 0\n    total += parse(s)?\n    return Ok(total)\n",
    ] {
        let out = expand_inline_question_ops(src);
        assert!(
            !out.contains("__typhon_ev_"),
            "nothing needed hoisting:\n{src}\n---\n{out}"
        );
    }
}

#[test]
fn a_dotted_receiver_is_read_before_the_operand() {
    // `parse` may rebind `self.items`; Python appends to the list it read.
    let out = expand_inline_question_ops(
        "def f(self) -> Result[int, str]:\n    self.items.append(parse(s)?)\n    return Ok(0)\n",
    );
    assert!(
        line_of(&out, "__typhon_ev_0__ = self.items") < line_of(&out, "= parse(s)"),
        "{out}"
    );
    assert!(out.contains("__typhon_ev_0__.append("), "{out}");
}

#[test]
fn a_name_the_operand_can_rebind_is_read_before_it() {
    // `global x` anywhere in the file: `g` may rebind `x`.
    let out = expand_inline_question_ops(
        "mut x: int = 1\ndef g() -> Result[int, str]:\n    global x\n    x = 2\n    return Ok(1)\ndef h() -> Result[int, str]:\n    return Ok(f(x, g()?))\n",
    );
    assert!(
        line_of(&out, "__typhon_ev_0__ = x") < line_of(&out, "= g()"),
        "{out}"
    );
    assert!(out.contains("f(__typhon_ev_0__, "), "{out}");
    // A walrus in the statement rebinds its target.
    let out = expand_inline_question_ops(
        "def h(y: int) -> Result[int, str]:\n    return Ok(f(y, g(y := 3)?))\n",
    );
    assert!(
        line_of(&out, "__typhon_ev_0__ = y") < line_of(&out, "= g(y := 3)"),
        "{out}"
    );
}

#[test]
fn an_augmented_assignment_loads_its_target_before_the_operand() {
    let out = expand_inline_question_ops(
        "def f(self) -> Result[int, str]:\n    self.pos += self.advance()?\n    return Ok(0)\n",
    );
    let load = line_of(&out, "__typhon_ev_0__ = self.pos");
    assert!(load < line_of(&out, "= self.advance()"), "{out}");
    assert!(out.contains("    __typhon_ev_0__ += __typhon_qi_"), "{out}");
    assert!(
        line_of(&out, "self.pos = __typhon_ev_0__") > line_of(&out, "__typhon_ev_0__ +="),
        "{out}"
    );
    // A subscript target: container and index evaluated once, in order.
    let out = expand_inline_question_ops(
        "def f(d: dict[int, int]) -> Result[int, str]:\n    d[key()] *= val()?\n    return Ok(0)\n",
    );
    assert!(
        line_of(&out, "__typhon_ev_0__ = key()")
            < line_of(&out, "__typhon_ev_1__ = d[__typhon_ev_0__]"),
        "{out}"
    );
    assert!(
        line_of(&out, "__typhon_ev_1__ = d[__typhon_ev_0__]") < line_of(&out, "= val()"),
        "{out}"
    );
    assert!(
        out.contains("d[__typhon_ev_0__] = __typhon_ev_1__"),
        "{out}"
    );
    // A global name target.
    let out = expand_inline_question_ops(
        "mut total: int = 0\ndef h() -> Result[int, str]:\n    global total\n    total += g()?\n    return Ok(total)\n",
    );
    assert!(
        line_of(&out, "__typhon_ev_0__ = total") < line_of(&out, "= g()"),
        "{out}"
    );
    assert!(out.contains("total = __typhon_ev_0__"), "{out}");
}

#[test]
fn a_call_to_a_shadowed_builtin_is_hoisted_like_any_call() {
    // A user `list()` / `super()` is an ordinary call, which Python runs
    // before the operand (PR #493 review: the spelling alone made it
    // trivial, so `q()` ran first).
    let out = expand_inline_question_ops(
        "def list() -> int:\n    return 1\ndef run() -> Result[int, str]:\n    return Ok(combine(list(), q()?))\n",
    );
    assert!(
        line_of(&out, "__typhon_ev_0__ = list()") < line_of(&out, "= q()"),
        "{out}"
    );
    let out = expand_inline_question_ops(
        "def super() -> int:\n    return 1\ndef run() -> Result[int, str]:\n    print(super(), q()?)\n    return Ok(0)\n",
    );
    assert!(
        line_of(&out, "__typhon_ev_0__ = super()") < line_of(&out, "= q()"),
        "{out}"
    );
    // Bound any other way — a parameter, an assignment, an import, a star
    // import, a class — likewise.
    for src in [
        "def run(list: Maker) -> Result[int, str]:\n    return Ok(combine(list(), q()?))\n",
        "dict = make\ndef run() -> Result[int, str]:\n    return Ok(combine(dict(), q()?))\n",
        "from helpers import set\ndef run() -> Result[int, str]:\n    return Ok(combine(set(), q()?))\n",
        "from helpers import *\ndef run() -> Result[int, str]:\n    return Ok(combine(tuple(), q()?))\n",
        "class frozenset:\n    pass\ndef run() -> Result[int, str]:\n    return Ok(combine(frozenset(), q()?))\n",
    ] {
        let out = expand_inline_question_ops(src);
        assert!(
            line_of(&out, "__typhon_ev_0__ = ") < line_of(&out, "= q()"),
            "{src}\n---\n{out}"
        );
    }
    // The builtins themselves stay in place, also when the file names them
    // in a comment, a string, an attribute or a type.
    for src in [
        "def run(xs: list[int]) -> Result[int, str]:\n    # list = the old one\n    let s: str = \"list = 1\"\n    obj.list = 3\n    return Ok(combine(list(), q()?))\n",
        "class C(B):\n    def m(self) -> Result[int, str]:\n        return Ok(combine(super(), q()?))\n",
    ] {
        let out = expand_inline_question_ops(src);
        assert!(
            !out.contains("__typhon_ev_"),
            "nothing needed hoisting:\n{src}\n---\n{out}"
        );
    }
}

#[test]
fn unmodelled_shapes_are_left_exactly_as_before() {
    // An operand under a conditional is not reordered (the checker rejects
    // the placement; the lowering must not invent an order for it).
    let src = "def f(c: bool) -> Result[int, str]:\n    return Ok(g(h(), parse(s)? if c else 0))\n";
    let out = expand_inline_question_ops(src);
    assert!(!out.contains("__typhon_ev_"), "{out}");
}

#[test]
fn multi_line_statement_hoists_a_sibling_from_an_earlier_line() {
    let src = concat!(
        "def node(self) -> Result[Node, str]:\n",
        "    return Ok(Node(\n",
        "        start=self.pos,\n",
        "        text=self.word()?,\n",
        "    ))\n",
    );
    let out = expand_inline_question_ops(src);
    assert!(
        line_of(&out, "__typhon_ev_0__ = self.pos") < line_of(&out, "= self.word()"),
        "{out}"
    );
    assert!(parses(&lower(src)), "{}", lower(src));
}

#[test]
fn hoisting_keeps_the_line_map_on_the_statement() {
    // Every emitted line still maps to a line of the statement it came from.
    let src = "def f() -> Result[int, str]:\n    return Ok(combine(first(), second()?))\n";
    let (out, map) = tyc_syntax::preprocess::expand_inline_question_ops_mapped(src);
    for (i, line) in out.lines().enumerate() {
        if line.contains("first()") || line.contains("second()") || line.contains("combine") {
            assert_eq!(map[i], 1, "line {i} `{line}` maps to {}", map[i]);
        }
    }
}

#[test]
fn an_operand_before_a_hoisted_sibling_is_hoisted_too() {
    // `f(a()?, b(), c()?)`: hoists land above every lift, so `a()` has to be
    // hoisted with `b()` or it would run after it.
    let out = expand_inline_question_ops(
        "def f() -> Result[int, str]:\n    return Ok(add3(a()?, b(), c()?))\n",
    );
    assert!(
        line_of(&out, "= a()?") < line_of(&out, "= b()"),
        "a() must stay before b():\n{out}"
    );
    assert!(line_of(&out, "= b()") < line_of(&out, "= c()"), "{out}");
    // Two operands with nothing between them keep the plain in-place lift.
    let out = expand_inline_question_ops(
        "def f() -> Result[int, str]:\n    return Ok(add(parse(s)?, parse(t)?))\n",
    );
    assert!(!out.contains("__typhon_ev_"), "{out}");
}

// ── W7-05: `gather` as an identifier ─────────────────────────────────────────

use tyc_syntax::preprocess::expand_gather_blocks;

#[test]
fn gather_named_binding_outside_an_async_body_is_not_a_gather_block() {
    for src in [
        // A class attribute (was lowered to `async with` in a class body).
        "class Settings:\n    gather: bool = False\nprint(Settings())\n",
        // A module-level binding (was a bogus `unknown_name`).
        "gather: int = 3\nprint(gather)\n",
        // A parameter on its own continuation line (was `tyc::parse`).
        "def f(\n    x: int,\n    gather: bool = False,\n) -> None:\n    pass\n",
        // A sync function body: `gather:` can only lower inside `async def`.
        "def f() -> None:\n    gather: int = 3\n",
        // A class nested in an async function is still a class body.
        "async def f() -> None:\n    class S:\n        gather: bool = False\n",
    ] {
        let out = expand_gather_blocks(src);
        assert_eq!(out, src, "must be left alone:\n{src}");
    }
}

#[test]
fn gather_blocks_inside_async_bodies_still_lower() {
    for src in [
        "async def load() -> int:\n    gather: a = f1(); b = f2()\n    return a + b\n",
        "async def load() -> int:\n    gather:\n        a = f1()\n        b = f2()\n    return a + b\n",
        "impl X:\n    async def m(self) -> int:\n        if True:\n            gather: a = f1(); b = f2()\n        return 0\n",
        "async def load() -> int:\n    gather(strategy=\"best-effort\"): a = f1(); b = f2()\n    return 0\n",
    ] {
        let out = expand_gather_blocks(src);
        assert!(
            out.contains("asyncio.TaskGroup()") || out.contains("asyncio.gather("),
            "gather block must lower:\n{src}\n---\n{out}"
        );
    }
}

// ── W7-08: CRLF line endings ─────────────────────────────────────────────────

#[test]
fn crlf_source_lowers_like_its_lf_twin() {
    let lf = "let s: str = \"\"\"a\nb\"\"\"\nprint(repr(s))\nlet t: str = f\"\"\"x\n{1 + 1}\ny\"\"\"\nlet u: bytes = b\"\"\"p\nq\"\"\"\n";
    let crlf = lf.replace('\n', "\r\n");
    let lowered = lower(&crlf);
    assert!(!lowered.contains('\r'), "{lowered:?}");
    assert_eq!(lowered, lower(lf));
    // Line numbering is unchanged: the same number of lines either way.
    let (expanded, map) = tyc_syntax::preprocess::expand_sugar_mapped(&crlf, true);
    assert_eq!(expanded.lines().count(), lf.lines().count());
    assert_eq!(map.len(), lf.lines().count());
}

#[test]
fn crlf_multiline_string_parses_to_lf_contents() {
    let src = "let s: str = \"\"\"a\r\nb\"\"\"\r\n";
    let module = tyc_syntax::parse_module(&lower(src))
        .expect("parses")
        .into_syntax();
    let tyc_syntax::ast::Stmt::AnnAssign(a) = &module.body[0] else {
        panic!("expected an annotated assignment");
    };
    let Some(tyc_syntax::ast::Expr::StringLiteral(lit)) = a.value.as_deref() else {
        panic!("expected a string literal");
    };
    assert_eq!(lit.value.to_str(), "a\nb");
}

// ── W7-10: with-chain `else` block that declares a name ─────────────────────

use tyc_syntax::preprocess::expand_with_chains;

#[test]
fn with_chain_else_copies_sit_in_exclusive_branches_when_they_declare() {
    let src = concat!(
        "def total(a: str, b: str) -> int:\n",
        "    with x = parse_int(a)?, y = parse_int(b)?:\n",
        "        return x + y\n",
        "    else err:\n",
        "        let msg: str = f\"failed: {err}\"\n",
        "        return -1\n",
    );
    let out = expand_with_chains(src);
    // Copy 0 under the first guard; copy 1 nested in the first guard's
    // `else:`, under the second guard; the success body innermost.
    let expected_tail = concat!(
        "    __typhon_with_0__ = parse_int(a)\n",
        "    if isinstance(__typhon_with_0__, __typhon_Err__):\n",
        "        let __typhon_with_err_0__ = __typhon_with_0__.error\n",
        "        let msg: str = f\"failed: {__typhon_with_err_0__}\"\n",
        "        return -1\n",
        "    else:\n",
        "        let x = __typhon_with_0__.value\n",
        "        __typhon_with_1__ = parse_int(b)\n",
        "        if isinstance(__typhon_with_1__, __typhon_Err__):\n",
        "            let __typhon_with_err_1__ = __typhon_with_1__.error\n",
        "            let msg: str = f\"failed: {__typhon_with_err_1__}\"\n",
        "            return -1\n",
        "        else:\n",
        "            let y = __typhon_with_1__.value\n",
        "            return x + y\n",
    );
    assert!(out.ends_with(expected_tail), "{out}");
    assert!(parses(&lower(src)), "{}", lower(src));
}

#[test]
fn with_chain_without_an_else_declaration_keeps_the_flat_ladder() {
    for src in [
        // `else` without a declaration.
        "def f(a: str, b: str) -> Result[int, str]:\n    with x = p(a)?, y = p(b)?:\n        return Ok(x + y)\n    else err:\n        return Err(err)\n",
        // A single binding never duplicated anything.
        "def f(a: str) -> int:\n    with x = p(a)?:\n        return x\n    else err:\n        let m: str = err\n        return -1\n",
        // No `else` at all.
        "def f(a: str, b: str) -> Result[int, str]:\n    with x = p(a)?, y = p(b)?:\n        return Ok(x + y)\n",
    ] {
        let out = expand_with_chains(src);
        assert!(
            !out.lines().any(|l| l.trim() == "else:"),
            "flat ladder expected:\n{src}\n---\n{out}"
        );
    }
}

#[test]
fn with_chain_nested_form_leaves_string_content_alone() {
    let src = concat!(
        "def f(a: str, b: str) -> str:\n",
        "    with x = p(a)?, y = p(b)?:\n",
        "        let banner: str = \"\"\"sum\n",
        "  of two\"\"\"\n",
        "        return banner\n",
        "    else err:\n",
        "        freeze let m: list[str] = [err]\n",
        "        return \"\"\"failed\n",
        "  here\"\"\"\n",
    );
    let out = expand_with_chains(src);
    // String continuation lines keep their exact leading whitespace.
    assert_eq!(out.matches("\n  of two\"\"\"\n").count(), 1, "{out}");
    assert_eq!(out.matches("\n  here\"\"\"\n").count(), 2, "{out}");
    // The success body moved in one unit; its string opener with it.
    assert!(
        out.contains("\n            let banner: str = \"\"\"sum\n"),
        "{out}"
    );
}

// ── W7-09: `|>` precedence in every expression position ─────────────────────

use tyc_syntax::preprocess::expand_pipes;

fn pipes(src: &str) -> String {
    expand_pipes(&format!("{src}\n")).trim_end().to_owned()
}

#[test]
fn pipe_binds_looser_than_every_operator_at_statement_level() {
    // Unchanged: the left operand is everything to the left in the slot.
    assert_eq!(pipes("x = not 0 |> add(0)"), "x = add(not 0, 0)");
    assert_eq!(pipes("x = 1 < 2 |> f()"), "x = f(1 < 2)");
    assert_eq!(pipes("x = a + b |> f()"), "x = f(a + b)");
    assert_eq!(pipes("x = a if c else b |> f()"), "x = f(a if c else b)");
    assert_eq!(pipes("return a and b |> f()"), "return f(a and b)");
}

#[test]
fn pipe_slots_end_at_commas_colons_keywords_and_brackets() {
    // Tuple elements and call arguments: the comma ends the slot (a pipe
    // used to swallow `g(a, b |> f())` into `g(f(a, b))`).
    assert_eq!(pipes("x = a |> f(), b |> g()"), "x = f(a), g(b)");
    assert_eq!(pipes("return a |> f(), b"), "return f(a), b");
    assert_eq!(pipes("print(a, b |> f())"), "print(a, f(b))");
    assert_eq!(pipes("t = (a, b |> f())"), "t = (a, f(b))");
    // List / set displays, subscripts and slices.
    assert_eq!(pipes("xs = [a |> f(), b]"), "xs = [f(a), b]");
    assert_eq!(pipes("s = {a |> f()}"), "s = {f(a)}");
    assert_eq!(pipes("v = xs[i |> f()]"), "v = xs[f(i)]");
    assert_eq!(pipes("v = xs[i |> f():j]"), "v = xs[f(i):j]");
    // Dict keys and values.
    assert_eq!(pipes("d = {k |> f(): v |> g()}"), "d = {f(k): g(v)}");
    // Keyword arguments and parameter defaults.
    assert_eq!(pipes("y = h(key=v |> f())"), "y = h(key=f(v))");
    assert_eq!(
        pipes("def m(x: int = a |> f()) -> int:"),
        "def m(x: int = f(a)) -> int:"
    );
    // Lambda bodies inside an argument.
    assert_eq!(
        pipes("ys = map(lambda v: v |> f(), xs)"),
        "ys = map(lambda v: f(v), xs)"
    );
    // Comprehensions: element, iterable and condition are separate slots;
    // a conditional expression before the first `for` stays one slot.
    assert_eq!(
        pipes("ys = [x |> f() for x in xs |> g() if x |> h()]"),
        "ys = [f(x) for x in g(xs) if h(x)]"
    );
    assert_eq!(
        pipes("ys = [a if c else b |> f() for a in xs]"),
        "ys = [f(a if c else b) for a in xs]"
    );
    assert_eq!(
        pipes("d = {k: v |> f() for k, v in items}"),
        "d = {k: f(v) for k, v in items}"
    );
    assert_eq!(
        pipes("n = sum(x |> f() for x in xs)"),
        "n = sum(f(x) for x in xs)"
    );
    // Comparisons and `not` stay inside the slot, as at statement level.
    assert_eq!(
        pipes("ys = [not x |> f() for x in xs]"),
        "ys = [f(not x) for x in xs]"
    );
}

#[test]
fn pipe_in_statement_headers_and_fstring_fields() {
    assert_eq!(pipes("if x |> f():"), "if f(x):");
    assert_eq!(pipes("elif x |> f():"), "elif f(x):");
    assert_eq!(pipes("while x |> f():"), "while f(x):");
    assert_eq!(pipes("for y in xs |> f():"), "for y in f(xs):");
    assert_eq!(pipes("assert x |> f(), msg"), "assert f(x), msg");
    assert_eq!(pipes("s = f\"{x |> g()}\""), "s = f\"{g(x)}\"");
    assert_eq!(
        pipes("s = f\"a{x |> g():>10}b{y!r}\""),
        "s = f\"a{g(x):>10}b{y!r}\""
    );
    assert_eq!(pipes("s = f'{x |> h(\", \")}'"), "s = f'{h(x, \", \")}'");
    assert_eq!(pipes("s = f\"{x |> g()=}\""), "s = f\"{g(x)=}\"");
    // Plain strings are never touched.
    assert_eq!(pipes("s = \"{x |> g()}\""), "s = \"{x |> g()}\"");
}

#[test]
fn pipe_with_a_non_call_right_operand_is_still_left_for_the_parser() {
    for src in [
        "x = a |> f() + 1",
        "ys = [a |> 3]",
        "x = a |> f() if c else b",
    ] {
        assert!(pipes(src).contains("|>"), "{src} -> {}", pipes(src));
    }
}

#[test]
fn every_pipe_position_lowers_to_parseable_python() {
    let src = concat!(
        "def f(x: int) -> int:\n    return x\n",
        "def h(x: int, y: int = 0) -> int:\n    return x + y\n",
        "xs: list[int] = [1, 2]\n",
        "a = [x |> f() for x in xs if (x |> f()) > 0]\n",
        "b = {x |> f(): x |> h(1) for x in xs}\n",
        "c = (xs[0] |> f(), xs[1] |> h(y=2))\n",
        "d = f\"{xs[0] |> f():>4}\"\n",
    );
    assert!(parses(&lower(src)), "{}", lower(src));
}

// ── W7-12: low-severity preprocessor gaps ────────────────────────────────────

/// (a) CPython refuses a 201st open bracket; so does the parser `tyc check`
/// and `tyc run` use.
#[test]
fn bracket_nesting_past_cpython_limit_is_a_parse_error() {
    let nest = |d: usize| format!("x = {}{}\nprint(len(x))\n", "[".repeat(d), "]".repeat(d));
    assert!(parses(&lower(&nest(200))));
    let err = tyc_syntax::parse_module(&lower(&nest(201))).expect_err("201 levels");
    assert!(
        err.to_string().contains("too many nested parentheses"),
        "{err}"
    );
}

/// (b) `go` / `comptime` starting a continuation line inside brackets are
/// names, not statement keywords.
#[test]
fn go_and_comptime_on_a_continuation_line_are_names() {
    for kw in ["go", "comptime"] {
        for indent in ["", "    "] {
            let src = format!(
                "def f(*args: int) -> int:\n    return sum(args)\n\n\
                 mut {kw}: int = 2\n\
                 mut total: int = f(1,\n{indent}{kw} + 10)\n\
                 total = f(\n{indent}{kw} * 3,\n)\n"
            );
            let out = lower(&src);
            assert!(!out.contains("spawn"), "{kw}/{indent:?}:\n{out}");
            assert!(
                out.contains(&format!("{indent}{kw} + 10)"))
                    && out.contains(&format!("{indent}{kw} * 3,")),
                "{kw}/{indent:?}:\n{out}"
            );
            assert!(parses(&out), "{out}");
            let prep = preprocess(&expand_sugar(&src, true));
            assert!(
                prep.comptime_bindings.is_empty(),
                "{kw}: {:?}",
                prep.comptime_bindings
            );
        }
    }
    // A real `go` statement is still a spawn.
    let out = lower("async def w() -> None:\n    pass\n\nasync def m() -> None:\n    go w()\n");
    assert!(out.contains("typhon_runtime.tasks.spawn(w())"), "{out}");
}

/// (c) A declaration-only `def` keeps a trailing comment outside the `: ...`
/// the preprocessor appends, and the line is recorded for `tyc fmt`.
#[test]
fn bodiless_def_with_a_trailing_comment_parses_and_is_recorded() {
    let src = "interface Shape:\n    def area(self) -> float  # the area\n    async def load(self) -> int\n";
    let prep = preprocess(src);
    assert!(
        prep.python_source
            .contains("    def area(self) -> float: ...  # the area\n"),
        "{}",
        prep.python_source
    );
    assert!(prep
        .python_source
        .contains("    async def load(self) -> int: ...\n"));
    assert_eq!(prep.bodiless_def_lines, vec![1, 2]);
    assert!(parses(&prep.python_source));
}

/// (d) A `?` in a replacement field on a continuation line of a
/// triple-quoted f-string lifts above the statement.
#[test]
fn question_in_a_continued_triple_quoted_fstring_field_lifts() {
    let src = concat!(
        "def parse(s: str) -> Result[int, str]:\n    return Ok(int(s))\n\n",
        "def show(s: str) -> Result[str, str]:\n",
        "    let text: str = f\"\"\"value:\n",
        "{parse(s)?:>3} and {{literal}} {s!r}\n",
        "{parse(s + s)? * 2} done\"\"\"\n",
        "    return Ok(text)\n",
    );
    let out = lower(src);
    assert!(parses(&out), "{out}");
    assert!(!out.contains(")?"), "{out}");
    // Both guards sit above the statement, in field order, at its indent.
    let first = out.find("= parse(s)\n").expect("first lift");
    let second = out.find("= parse(s + s)\n").expect("second lift");
    let stmt = out.find("    let text: str = f\"\"\"").expect("statement");
    assert!(first < second && second < stmt, "{out}");
    assert!(out.contains(".value:>3} and {{literal}} {s!r}\n"), "{out}");
    assert!(out.contains(".value * 2} done\"\"\"\n"), "{out}");
}
