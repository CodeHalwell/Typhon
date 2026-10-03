//! End-to-end regression tests for the 2026-10-03 review's W7 workstream
//! (preprocessor / lowering). Each program prints its side effects; the
//! expected output beside it was sequenced by hand from Python's evaluation
//! rules — NOT taken from either execution surface, because a lowering bug
//! corrupts the VM and CPython identically and a VM ↔ CPython comparison
//! cannot see it.

use std::path::Path;
use std::process::Command;

fn tyc() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_tyc"));
    c.env("TYC_NO_SYNC", "1").env("TYC_NO_INTROSPECT", "1");
    c
}

/// A Python 3.13+ interpreter, or `None` to skip (a panic under
/// `TYC_REQUIRE_PYTHON=1`, as in the rest of the suite).
fn python() -> Option<String> {
    for candidate in ["python3.13", "python3"] {
        if let Ok(out) = Command::new(candidate)
            .args(["-c", "import sys; print(sys.version_info >= (3, 13))"])
            .output()
        {
            if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "True" {
                return Some(candidate.to_owned());
            }
        }
    }
    assert!(
        std::env::var_os("TYC_REQUIRE_PYTHON").is_none(),
        "TYC_REQUIRE_PYTHON is set but no Python 3.13+ interpreter was found"
    );
    None
}

fn scaffold(dir: &Path, src: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("typhon.toml"),
        "[project]\nname = \"w7\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
    )
    .unwrap();
    std::fs::write(dir.join("src").join("main.ty"), src).unwrap();
}

/// Run `src` on the VM and, when Python is present, compiled on CPython;
/// both must print exactly `expected`.
fn assert_runs_as(src: &str, expected: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    scaffold(dir, src);

    let vm = tyc()
        .current_dir(dir)
        .args(["run", "--no-fallback", "src/main.ty"])
        .output()
        .unwrap();
    assert!(
        vm.status.success(),
        "tyc run failed:\n{}",
        String::from_utf8_lossy(&vm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&vm.stdout), expected, "VM output");

    assert_cpython_runs_in(dir, expected);
}

/// `tyc check` passes on `src`, and compiled on CPython it prints exactly
/// `expected`. For lowerings whose VM side depends on a VM change outside
/// this workstream (named at each call site).
fn assert_checks_and_cpython_runs_as(src: &str, expected: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    scaffold(dir, src);
    let check = tyc()
        .current_dir(dir)
        .args(["check", "src"])
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "tyc check failed:\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_cpython_runs_in(dir, expected);
}

fn assert_cpython_runs_in(dir: &Path, expected: &str) {
    let Some(py) = python() else { return };
    let build = tyc()
        .current_dir(dir)
        .args(["build", "--no-sync"])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "tyc build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(py)
        .current_dir(dir)
        .arg("build/main.py")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "python failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        expected,
        "CPython output"
    );
}

/// W7-04: an inline `?` must not move its operand ahead of anything the
/// statement evaluates before it — the review's four repros (`start=self.pos`
/// captured before `self.word()?` advances it; `first()` before `second()?`,
/// and still run when `second()` fails; list elements in order; a subscript
/// assignment's right-hand side before its target index).
#[test]
fn inline_question_keeps_python_evaluation_order() {
    assert_runs_as(
        include_str!("fourth_wave/eval_order_basic.ty"),
        include_str!("fourth_wave/eval_order_basic.expected"),
    );
}

/// W7-04: the same guarantee across statement shapes — a multi-line call, a
/// left f-string operand, nested calls with two `?`, a binary operator, a
/// computed method receiver, keyword arguments, an `if` / `for` header, and
/// an augmented subscript assignment.
#[test]
fn inline_question_keeps_python_evaluation_order_across_shapes() {
    assert_runs_as(
        include_str!("fourth_wave/eval_order_shapes.ty"),
        include_str!("fourth_wave/eval_order_shapes.expected"),
    );
}

/// W7-05: `gather` used as an ordinary name — a class attribute, a module
/// binding, and a parameter on a continuation line — is not a `gather:`
/// block. (It was lowered to `async with asyncio.TaskGroup()` in a class
/// body, a bogus `unknown_name`, and a `tyc::parse` error respectively.)
#[test]
fn gather_is_an_ordinary_name_outside_async_bodies() {
    assert_runs_as(
        include_str!("fourth_wave/gather_identifier.ty"),
        include_str!("fourth_wave/gather_identifier.expected"),
    );
}

/// W7-06: an `impl` method whose default or decorator names something bound
/// after the class is defined where the `impl` block is, so the module
/// imports instead of raising `NameError` (both surfaces), including a
/// decorator between a sealed-union alias and its `impl` block.
#[test]
fn impl_methods_see_names_bound_before_their_block() {
    assert_runs_as(
        include_str!("fourth_wave/impl_site.ty"),
        include_str!("fourth_wave/impl_site.expected"),
    );
}

/// W7-07: a field whose default is a module-level list / dict / set given by
/// name imports (it raised `ValueError: mutable default` on both surfaces)
/// and every instance gets its own shallow copy.
#[test]
fn named_mutable_default_gives_each_instance_a_copy() {
    assert_runs_as(
        include_str!("fourth_wave/named_mutable_default.ty"),
        include_str!("fourth_wave/named_mutable_default.expected"),
    );
}

/// W7-08: a CRLF file's multi-line string literals hold `\n`, as CPython
/// reads them (both surfaces printed `'a\r\nb'`). The source is built here
/// rather than checked in, so no checkout line-ending setting can alter it.
#[test]
fn crlf_source_strings_hold_lf_on_both_surfaces() {
    let lf = "let s: str = \"\"\"a\nb\"\"\"\nprint(repr(s))\nlet t: str = f\"\"\"x\n{1 + 1}\ny\"\"\"\nprint(repr(t))\nlet u: bytes = b\"\"\"p\nq\"\"\"\nprint(repr(u))\n";
    assert_runs_as(
        &lf.replace('\n', "\r\n"),
        "'a\\nb'\n'x\\n2\\ny'\nb'p\\nq'\n",
    );
}

/// W7-10: a `let` in the `else` block of a with-chain with two or more
/// bindings no longer trips `tyc::no_block_shadow`; the block runs once on
/// the first failing binding (three-binding, mixed error types, and an
/// `else` that falls through to the code after the chain).
#[test]
fn with_chain_else_block_may_declare_names() {
    assert_runs_as(
        include_str!("fourth_wave/with_chain_else_let.ty"),
        include_str!("fourth_wave/with_chain_else_let.expected"),
    );
}

/// W7-11: a metaclass (`class Registry(type)`, and a subclass of one) is not
/// given `@dataclass`, which replaced `type.__init__` and raised `TypeError`
/// when the first class used it. CPython only: `tyc run` ignores
/// `metaclass=` (reported to the VM workstream).
#[test]
fn metaclass_is_not_a_dataclass() {
    assert_checks_and_cpython_runs_as(
        "class Registry(type):\n    pass\n\nclass Strict(Registry):\n    pass\n\n\
         class Widget(metaclass=Registry):\n    size: int = 3\n\n\
         class Gadget(metaclass=Strict):\n    pass\n\n\
         print(type(Widget).__name__, type(Gadget).__name__)\n\
         print(isinstance(Widget, Registry), isinstance(Gadget, Registry))\n\
         print(Widget().size)\n",
        "Registry Strict\nTrue True\n3\n",
    );
}

/// W7-11: a `plain class` subclass no longer re-declares the attributes it
/// inherits, so a later `Cfg.debug = True` reaches it (a subclass that
/// declares its own `debug` keeps it). CPython only: `tyc run` snapshots a
/// base's class attributes when a subclass is created (reported to the VM
/// workstream).
#[test]
fn plain_subclass_inherits_rather_than_copies_attributes() {
    assert_checks_and_cpython_runs_as(
        "plain class Cfg:\n    debug: bool = False\n    name: str = \"cfg\"\n\n\
         plain class Sub(Cfg):\n    debug: bool = False\n\n\
         plain class Other(Cfg):\n    pass\n\n\
         Cfg.debug = True\n\
         print(Cfg.debug, Sub.debug, Other.debug, Sub.name)\n",
        "True False True cfg\n",
    );
}

/// W7-09: `|>` in a comprehension (element and condition), a dict
/// comprehension (key and value), tuple elements, a call argument with a
/// keyword argument, an f-string field, a generator argument, `if` and
/// `for` headers — each a `tyc::parse` error before — plus the unchanged
/// lowest precedence (`1 < 2 |> str()` is `str(1 < 2)`).
#[test]
fn pipe_works_in_every_expression_position() {
    assert_runs_as(
        include_str!("fourth_wave/pipe_positions.ty"),
        include_str!("fourth_wave/pipe_positions.expected"),
    );
}

/// W7-12: a `?` in replacement fields on a continuation line of a
/// triple-quoted f-string, a declaration-only interface method with a
/// trailing comment, and `go` / `comptime` as variables at the start of
/// continuation lines — each a `tyc::parse` (or `go` / `comptime`) error
/// before.
#[test]
fn low_severity_preprocessor_shapes_run_on_both_surfaces() {
    assert_runs_as(
        include_str!("fourth_wave/low_severity.ty"),
        include_str!("fourth_wave/low_severity.expected"),
    );
}

/// W7-12: more than 200 nested brackets is CPython's compile-time
/// `SyntaxError: too many nested parentheses`; `tyc check` and `tyc run`
/// accepted it and `tyc build` emitted a `.py` that did not compile.
#[test]
fn bracket_nesting_past_cpython_limit_is_rejected_at_check_time() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let d = 201;
    scaffold(
        dir,
        &format!(
            "let x: list[object] = {}{}\nprint(len(x))\n",
            "[".repeat(d),
            "]".repeat(d)
        ),
    );
    for args in [
        &["check", "src"][..],
        &["run", "--no-fallback", "src/main.ty"][..],
    ] {
        let out = tyc().current_dir(dir).args(args).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} accepted it");
        assert!(
            stderr.contains("too many nested parentheses"),
            "{args:?}:\n{stderr}"
        );
    }
}
