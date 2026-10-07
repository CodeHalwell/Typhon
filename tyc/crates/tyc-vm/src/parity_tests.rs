//! VM ↔ CPython parity probes from the 2026-10-03 reviews (W5).
//!
//! Every probe here is compared against the hosting `python3.13` at test
//! time — never against a pinned literal — so a green run means the VM and
//! CPython agree on this host. The probes are plain Python plus the few
//! Typhon spellings [`to_python`] rewrites (`plain class`, `mut`), so the
//! same text runs on both surfaces; a `plain class` keeps CPython's bare
//! class semantics where a bare Typhon `class` would become a dataclass.
//!
//! Probes record what they observe with `show(...)` / `trap(...)` (see
//! [`PRELUDE`]); the harness compares the recorded transcripts.

use crate::{run_file, run_source_reporting, VmError};
use std::path::Path;

/// Same worker-stack size the CLI uses (see `tests::TEST_WORKER_STACK_SIZE`).
const STACK: usize = 256 * 1024 * 1024;

fn on_worker<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .stack_size(STACK)
            .spawn_scoped(scope, f)
            .expect("failed to spawn the VM test worker thread");
        match handle.join() {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

/// Recording helpers every probe can use. `show` appends one line of
/// `str()`-joined parts; `trap` runs a thunk and records either its result's
/// `repr` or the exception's type and message.
pub(crate) const PRELUDE: &str = r#"out: list[str] = []
def show(*parts: object) -> None:
    out.append(" ".join([str(p) for p in parts]))
def trap(label: str, thunk: object) -> None:
    try:
        out.append(label + " = " + repr(thunk()))
    except BaseException as e:
        out.append(label + " ! " + type(e).__name__ + ": " + str(e))
"#;

/// The probe as plain Python: the only Typhon spellings probes use.
fn to_python(probe: &str) -> String {
    probe.replace("plain class ", "class ").replace("mut ", "")
}

/// The CPython a probe is compared against: python3.13, or python3.15 for
/// the 3.15 builtins (`frozendict`, `sentinel`). Each has its own
/// "required" switch, as in the CLI tests.
#[derive(Clone, Copy)]
enum Oracle {
    Py313,
    Py315,
}

impl Oracle {
    fn exe(self) -> &'static str {
        match self {
            Oracle::Py313 => "python3.13",
            Oracle::Py315 => "python3.15",
        }
    }

    fn required_var(self) -> &'static str {
        match self {
            Oracle::Py313 => "TYC_REQUIRE_PYTHON",
            Oracle::Py315 => "TYC_REQUIRE_PYTHON315",
        }
    }
}

fn python_missing(test: &str) {
    python_missing_for(test, Oracle::Py313);
}

fn python_missing_for(test: &str, oracle: Oracle) {
    let (exe, var) = (oracle.exe(), oracle.required_var());
    if std::env::var_os(var).is_some() {
        panic!("{exe} is required as the oracle ({var} is set)");
    }
    eprintln!("skipping {test}: no {exe} on PATH");
}

/// Run `py` under python3.13 in `dir`; `(stdout, stderr, exit code)`.
fn run_python(dir: &Path, py: &str) -> Option<(String, String, i32)> {
    run_python_as(Oracle::Py313, dir, py)
}

fn run_python_as(oracle: Oracle, dir: &Path, py: &str) -> Option<(String, String, i32)> {
    let script = dir.join("probe.py");
    std::fs::write(&script, py).ok()?;
    let out = std::process::Command::new(oracle.exe())
        .arg("-X")
        .arg("no_debug_ranges")
        .arg(&script)
        .current_dir(dir)
        // The VM's `str` hash is CPython's under this seed (see `pyhash`),
        // so string-set order is comparable too.
        .env("PYTHONHASHSEED", "0")
        .output()
        .ok()?;
    Some((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    ))
}

/// Run `probe` (prefixed with [`PRELUDE`]) on the VM and on CPython and
/// assert the two `out` transcripts are identical.
pub(crate) fn assert_matches_cpython(test: &str, probe: &str) {
    assert_matches_cpython_with(test, probe, "");
}

/// [`assert_matches_cpython`] with `py_header` run first on the CPython side
/// only — the generated runtime a compiled program would import.
fn assert_matches_cpython_with(test: &str, probe: &str, py_header: &str) {
    assert_matches_oracle(test, probe, py_header, Oracle::Py313);
}

/// [`assert_matches_cpython`] against python3.15, for the builtins only it
/// has (`frozendict`, `sentinel`; the VM models them on every target).
fn assert_matches_python315(test: &str, probe: &str) {
    assert_matches_oracle(test, probe, "", Oracle::Py315);
}

fn assert_matches_oracle(test: &str, probe: &str, py_header: &str, oracle: Oracle) {
    let exe = oracle.exe();
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("transcript.txt");
    let tail = format!(
        "\nwith open({:?}, \"w\", encoding=\"utf-8\") as _out:\n    _out.write(\"\\n\".join(out))\n",
        dump.display().to_string()
    );
    let body = format!("{PRELUDE}{probe}{tail}");
    let python = format!("{py_header}{}", to_python(&body));
    let Some((_, py_err, code)) = run_python_as(oracle, dir.path(), &python) else {
        return python_missing_for(test, oracle);
    };
    assert_eq!(code, 0, "{test}: the probe fails under {exe}:\n{py_err}");
    let expected = std::fs::read_to_string(&dump).unwrap();
    std::fs::remove_file(&dump).unwrap();
    let vm = on_worker(|| crate::run_source(&body, None, &[]));
    assert_eq!(vm.unwrap(), 0, "{test}: the probe fails under the VM");
    let got = std::fs::read_to_string(&dump).unwrap();
    if got != expected {
        let diff: Vec<String> = got
            .lines()
            .zip(expected.lines())
            .filter(|(g, e)| g != e)
            .take(12)
            .map(|(g, e)| format!("  vm:  {g}\n  cpy: {e}"))
            .collect();
        panic!(
            "{test}: VM transcript differs from {exe} ({} vs {} lines)\n{}",
            got.lines().count(),
            expected.lines().count(),
            diff.join("\n")
        );
    }
}

/// Run a program that ends in an uncaught exception on both surfaces and
/// return `(vm traceback, cpython stderr)`, both naming the same file.
fn tracebacks(probe: &str) -> Option<(String, String)> {
    let dir = tempfile::tempdir().unwrap();
    let (_, py_err, _) = run_python(dir.path(), &to_python(probe))?;
    let origin = dir.path().join("probe.py");
    let mut vm_err = String::new();
    let code = on_worker(|| {
        let mut sink = |s: &str| vm_err.push_str(s);
        run_source_reporting(
            probe,
            Some(&origin),
            &[],
            crate::VmOptions::default(),
            &mut sink,
        )
    });
    assert_eq!(code.unwrap(), 1);
    Some((vm_err, py_err))
}

fn run_project(files: &[(&str, &str)], entry: &str) -> Result<i32, VmError> {
    let dir = tempfile::tempdir().unwrap();
    for (rel, text) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }
    on_worker(|| run_file(&dir.path().join(entry), &[]))
}

// ── W5-19: tracebacks ─────────────────────────────────────────────────────

#[test]
fn w5_19_traceback_lines_and_text_follow_the_user_source() {
    // Each `?` used to add four lines to every later frame, and the frame
    // text was the lowered Python (`__typhon_checked_cast__(…)`).
    let src = r#"def parse(s: str) -> Result[int, str]:
    return Ok(int(s))
def use(s: str) -> Result[int, str]:
    let v: int = parse(s)?
    let w: object = v as! str
    return Ok(v)
def main() -> None:
    use("3")
main()
"#;
    let mut err = String::new();
    let code = on_worker(|| {
        let mut sink = |s: &str| err.push_str(s);
        run_source_reporting(
            src,
            Some(Path::new("tb.ty")),
            &[],
            crate::VmOptions::default(),
            &mut sink,
        )
    });
    assert_eq!(code.unwrap(), 1);
    assert_eq!(
        err,
        "Traceback (most recent call last):\n  File \"tb.ty\", line 9, in <module>\n    main()\n  File \"tb.ty\", line 8, in main\n    use(\"3\")\n  File \"tb.ty\", line 5, in use\n    let w: object = v as! str\nTypeError: as! cast failed: value of type int does not match <class 'str'>\n"
    );
}

#[test]
fn w5_19_chained_tracebacks_match_cpython() {
    let probes = [
        // `raise … from e`: the cause's own traceback, then the separator.
        r#"def lookup(d: dict[str, int], k: str) -> int:
    return d[k]
def wrap() -> int:
    try:
        return lookup({}, "missing")
    except KeyError as e:
        raise ValueError("bad key") from e
wrap()
"#,
        // Implicit context: raised while handling another exception.
        r#"def lookup(d: dict[str, int], k: str) -> int:
    return d[k]
def ctx() -> int:
    try:
        return lookup({}, "x")
    except KeyError:
        raise RuntimeError("during")
ctx()
"#,
        // `finally` raising while an exception propagates.
        r#"def f() -> None:
    try:
        raise KeyError("first")
    finally:
        raise ValueError("second")
f()
"#,
        // A user-raised KeyError prints its argument's repr.
        "raise KeyError(\"missing\")\n",
        // `from None` suppresses the context.
        r#"try:
    {}["k"]
except KeyError:
    raise ValueError("clean") from None
"#,
        // Deep recursion collapses into "[Previous line repeated N more times]".
        r#"def rec(n: int) -> int:
    return rec(n + 1)
rec(0)
"#,
    ];
    for probe in probes {
        let Some((vm, py)) = tracebacks(probe) else {
            return python_missing("w5_19_chained_tracebacks_match_cpython");
        };
        assert_eq!(vm, py, "traceback differs for probe:\n{probe}");
    }
}

#[test]
fn w5_19_recursion_limit_counts_the_module_frame() {
    // CPython counts `<module>` against the limit: at the default 1000, the
    // 999th nested call is the last that fits.
    assert_matches_cpython(
        "w5_19_recursion_limit_counts_the_module_frame",
        r#"def depth(n: int) -> int:
    try:
        return depth(n + 1)
    except RecursionError:
        return n
show(depth(0))
def main() -> None:
    show(depth(0))
main()
"#,
    );
}

// ── W5-20: one canonical front end ────────────────────────────────────────

#[test]
fn w5_20_imported_modules_get_the_canonical_sugar_chain() {
    // A sibling module using with-chains, inline `?` in an `elif`, typed
    // unpacking and `as!` — every sugar pass the entry module gets — and a
    // traceback raised inside it reports the sibling's own `.ty` line.
    let util = r#"def half(n: int) -> Result[int, str]:
    if n % 2 == 0:
        return Ok(n // 2)
    return Err("odd")

def classify(n: int) -> Result[str, str]:
    if n < 0:
        return Ok("negative")
    elif half(n)? > 10:
        return Ok("big")
    return Ok("small")

def pair() -> tuple[int, str]:
    return (1, "a")

def unpack() -> str:
    let (a, b): tuple[int, str] = pair()
    let s: object = b
    return str(a) + (s as! str)
"#;
    let main = r#"from util import classify, unpack
assert classify(40).value == "big"
assert classify(4).value == "small"
assert classify(5).error == "odd"
assert classify(-1).value == "negative"
assert unpack() == "1a"
"#;
    assert_eq!(
        run_project(&[("util.ty", util), ("main.ty", main)], "main.ty").unwrap(),
        0
    );
}

#[test]
fn w5_20_shims_and_programs_share_the_front_end() {
    // The shim compiler goes through the same entry point; a shim-backed
    // module still imports and runs.
    assert_matches_cpython(
        "w5_20_shims_and_programs_share_the_front_end",
        r#"import collections, itertools, functools
show(collections.Counter("abca").most_common(1))
show(list(itertools.accumulate([1, 2, 3])))
show(functools.reduce(lambda a, b: a * b, [1, 2, 3, 4]))
"#,
    );
}

// ── W5-07: C3 method resolution and class namespaces ──────────────────────

#[test]
fn w5_07_c3_mro_and_cooperative_super() {
    assert_matches_cpython(
        "w5_07_c3_mro_and_cooperative_super",
        r#"plain class A:
    def who(self) -> str:
        return "A"
plain class B(A):
    def who(self) -> str:
        return "B>" + super().who()
plain class C(A):
    def who(self) -> str:
        return "C>" + super().who()
plain class D(B, C):
    def who(self) -> str:
        return "D>" + super().who()
show(D().who())
show([k.__name__ for k in D.__mro__])
show(super(B, D()).who())
plain class R:
    def __init__(self) -> None:
        show("R init")
plain class L(R):
    def __init__(self) -> None:
        show("L init")
        super().__init__()
plain class M(R):
    def __init__(self) -> None:
        show("M init")
        super().__init__()
plain class N(L, M):
    def __init__(self) -> None:
        show("N init")
        super().__init__()
N()
plain class X1:
    pass
plain class X2(X1):
    pass
def bad() -> None:
    plain class X3(X1, X2):
        pass
trap("inconsistent mro", bad)
plain class H:
    def f(self) -> str:
        return "method"
plain class J(H):
    f = "attr"
show(J().f, J.f)
"#,
    );
}

#[test]
fn w5_07_class_attributes_resolve_through_the_mro_at_read_time() {
    assert_matches_cpython(
        "w5_07_class_attributes_resolve_through_the_mro_at_read_time",
        r#"plain class Base:
    pass
plain class Sub(Base):
    pass
Base.tag = "late"
show(Sub.tag, Sub().tag)
def late_method(self: object) -> str:
    return "late method"
Base.lm = late_method
show(Sub().lm())
plain class DA:
    x = 1
plain class DB(DA):
    x = 2
show(DB.x, DB().x)
del DB.x
show(DB.x, DB().x)
DA.x = 10
show(DB.x)
trap("del inherited", lambda: delattr(DB, "x"))
plain class IA:
    items: list[int] = []
a1 = IA()
a2 = IA()
a1.items.append(1)
show(a2.items, IA.items)
a1.items = [9]
show(a1.items, a2.items)
plain class SA:
    label = "sa"
plain class SB(SA):
    label = "sb"
    def get(self) -> str:
        return super().label
show(SB().get())
plain class FC:
    def m(self) -> str:
        return "old"
def helper(self: object, n: int) -> int:
    return n * 2
FC.double = helper
FC.m = lambda self: "new"
show(FC().double(4), FC().m())
"#,
    );
}

#[test]
fn w5_07_functions_stored_as_class_attributes() {
    // classmethod / staticmethod / property called as functions, super()
    // through classmethods and properties, `__init_subclass__`.
    assert_matches_cpython(
        "w5_07_functions_stored_as_class_attributes",
        r#"def _make(cls: type) -> object:
    return cls()
def _get(self: object) -> int:
    return 42
def _set(self: object, v: int) -> None:
    show("set", v)
def _stat(n: int) -> int:
    return n + 1
plain class FA:
    make = classmethod(_make)
    val = property(_get, _set)
    st = staticmethod(_stat)
show(type(FA.make()).__name__, FA().val, FA.st(1), FA().st(2))
f = FA()
f.val = 7
trap("no deleter", lambda: delattr(f, "val"))
plain class FB(FA):
    pass
show(type(FB.make()).__name__, FB().val, type(FA.val).__name__)
plain class CA:
    @classmethod
    def create(cls) -> str:
        return "CA:" + cls.__name__
plain class CB(CA):
    @classmethod
    def create(cls) -> str:
        return "CB>" + super().create()
show(CB.create(), CB().create())
plain class PA:
    @property
    def name(self) -> str:
        return "pa"
plain class PB(PA):
    @property
    def name(self) -> str:
        return "pb+" + super().name
show(PB().name)
plain class Del:
    def __init__(self) -> None:
        self._v = 1
    @property
    def v(self) -> int:
        return self._v
    @v.deleter
    def v(self) -> None:
        show("deleting")
        del self._v
d = Del()
del d.v
show(hasattr(d, "_v"))
plain class Registry:
    subs: list[str] = []
    def __init_subclass__(cls, tag: str = "", **kw: object) -> None:
        super().__init_subclass__(**kw)
        Registry.subs.append(cls.__name__ + ":" + tag)
plain class R1(Registry, tag="one"):
    pass
plain class R2(R1):
    pass
show(Registry.subs)
"#,
    );
}

// ── W5-08: set iteration order ────────────────────────────────────────────

#[test]
fn w5_08_set_order_matches_cpython() {
    assert_matches_cpython(
        "w5_08_set_order_matches_cpython",
        r#"s = {-1, 0, 1}
show(s, list(s))
show({5, 3, 1, 100, 33, 2})
x = 5
show({x, 3, 1, 100, 33, 2})
show(set([5, 3, 1, 100, 33, 2, 64, 17, 9, 1000, -7]))
show(set(range(20, 0, -3)))
a = set(range(10))
b = {3, 4, 50, 60, 70, 8}
show(a | b, a & b, a - b, a ^ b, b - a, b ^ a, b & a)
show(a.union([100, 7, 99]), a.intersection([9, 1, 77]), a.difference(range(5)), a.symmetric_difference([1, 2, 300]))
c: set[int] = set()
for i in [10, 20, 30, 40, 50, 60, 70, 80, 90]:
    c.add(i)
show(c)
c.discard(30)
c.add(31)
c.add(8)
show(c)
show(c.pop(), c.pop(), c)
d = {"b": 1, "a": 2, 77: 3}
show(set(d), {"pear", "apple", "fig"}, set("hello"))
show({i * 7 for i in range(15)})
f = frozenset([3, 1, 2, 900])
show(f, {f: 1}, frozenset(f) is f, f | {5}, type(f | {5}).__name__)
show({1.5, 2.5, 0.5, 1e10}, {(1, 2), (3, 4), (0, 0)}, {True, 2, None})
e = {1, 2, 3}
e.update([4, 5], {6, 7})
show(e)
e.intersection_update({1, 2, 3, 4, 99})
show(e)
e.difference_update([1])
show(e)
e.symmetric_difference_update({2, 10})
show(e)
trap("pop empty", lambda: set().pop())
big = set(range(0, 100000, 7))
show(len(big), list(big)[:5])
for k in range(0, 100000, 14):
    big.discard(k)
show(len(big), list(big)[:5])
"#,
    );
}

// ── W5-10: value-mixin enums ──────────────────────────────────────────────

#[test]
fn w5_10_mixin_enums_behave_as_their_value() {
    assert_matches_cpython(
        "w5_10_mixin_enums_behave_as_their_value",
        r#"import json
from enum import Enum, IntEnum, StrEnum, IntFlag, auto
class Mode(str, Enum):
    FAST = "fast"
    SLOW = "slow"
    QUICK = "fast"
show(Mode.FAST == "fast", {"fast": 1}.get(Mode.FAST), Mode.FAST in ["fast"], "fast" == Mode.FAST)
show(Mode("fast"), Mode.FAST.value, Mode.FAST.upper(), isinstance(Mode.FAST, str), hash(Mode.FAST) == hash("fast"))
show(repr(Mode.FAST), str(Mode.FAST), f"{Mode.FAST}", Mode.FAST + "!", len(Mode.FAST))
show(list(Mode), len(Mode), Mode.QUICK is Mode.FAST, json.dumps({"m": Mode.SLOW}), "-".join([Mode.FAST, Mode.SLOW]))
class L(int, Enum):
    ONE = 1
    TWO = 2
show(L.ONE == 1, L.ONE + 1, isinstance(L.ONE, int), L.ONE < L.TWO, {1: "a"}[L.ONE], repr(L.ONE), str(L.ONE), L(2))
class IE(IntEnum):
    X = 1
    Y = 2
show(isinstance(IE.X, int), IE.X == 1, IE.X + IE.Y, str(IE.X), repr(IE.X), f"{IE.Y}", sorted([IE.Y, IE.X]), [1, 2][IE.X])
show(sorted({IE.Y: 1, IE.X: 2}.items()), json.dumps({"l": IE.X}), json.dumps(IE.Y), [IE.X] == [1], (IE.X, 0) < (IE.Y, 0))
class SE(StrEnum):
    ALPHA = auto()
    BETA = "bee"
show(SE.ALPHA, SE.ALPHA == "alpha", repr(SE.BETA), isinstance(SE.ALPHA, str), list(SE), SE("bee"))
class Perm(IntFlag):
    R = 4
    W = 2
show(Perm.R | Perm.W, Perm.R == 4, isinstance(Perm.R, int))
class Color(Enum):
    RED = 1
    CRIMSON = 1
show(Color.RED == 1, isinstance(Color.RED, int), list(Color), Color.CRIMSON is Color.RED)
"#,
    );
}

// ── W5-11: changing a dict or set while iterating it ──────────────────────

#[test]
fn w5_11_mutation_during_iteration_raises() {
    assert_matches_cpython(
        "w5_11_mutation_during_iteration_raises",
        r#"def grow() -> object:
    d = {1: 1, 2: 2}
    for k in d:
        d[k + 10] = 0
    return d
trap("grow", grow)
def shrink() -> object:
    d = {1: 1, 2: 2, 3: 3}
    try:
        for k in d:
            del d[k]
    except RuntimeError as e:
        show("caught", e)
    return d
trap("shrink", shrink)
def same_size() -> object:
    d = {"a": 1, "b": 2, "c": 3}
    out = []
    for k in d:
        out.append(k)
        if k == "a":
            del d["b"]
            d["d"] = 4
    return out
trap("same_size", same_size)
def setgrow() -> object:
    s = {1, 2, 3}
    for x in s:
        s.add(x + 100)
    return s
trap("setgrow", setgrow)
def setshrink() -> object:
    s = {1, 2, 3}
    for x in s:
        s.discard(x)
    return s
trap("setshrink", setshrink)
def update_ok() -> object:
    d = {1: 1, 2: 2}
    for k in d:
        d[k] = 5
    return d
trap("update_ok", update_ok)
def viewmut() -> None:
    d = {1: 1}
    for x in d.values():
        d[x + 5] = 1
trap("viewmut", viewmut)
d = {"a": 1}
k = d.keys()
v = d.values()
it = d.items()
d["b"] = 2
show(k, "b" in k, v, it, len(k), ("b", 2) in it, ("b", 3) in it)
show(d.keys() & {"a", "z"}, d.items() - {("a", 1)})
l = [1, 2, 3]; r = reversed(l); l.pop(); show(list(r))
l = [1, 2, 3]; r = reversed(l); l[0] = 9; show(list(r), type(r).__name__)
show(list(reversed(range(0, 10, 3))), list(reversed(range(10, 0, -3))), list(reversed("abc")))
dd = {"a": 1, "b": 2}
show(list(reversed(dd)), list(reversed(dd.items())), type(reversed(dd)).__name__)
def drev() -> None:
    d = {"a": 1, "b": 2}
    for k in reversed(d):
        d["z"] = 0
trap("drev", drev)
trap("set", lambda: reversed({1, 2}))
"#,
    );
}

// ── W5-12: catchable exceptions where the VM used to abort or panic ───────

#[test]
fn w5_12_oversized_results_raise_instead_of_aborting() {
    // Each of these aborted `tyc run` (exit 134: "memory allocation of …
    // bytes failed"), panicked ("capacity overflow", "Formatting argument
    // out of range", exit 101) or overflowed the native stack.
    assert_matches_cpython(
        "w5_12_oversized_results_raise_instead_of_aborting",
        r#"import json
import os
def size(thunk: object) -> object:
    return len(thunk())
trap("str*", lambda: size(lambda: "a" * 2**62))
trap("str2*", lambda: size(lambda: "aa" * 2**62))
trap("bytes*", lambda: size(lambda: b"a" * 2**62))
trap("zfill", lambda: size(lambda: "x".zfill(2**62)))
trap("ljust", lambda: size(lambda: "x".ljust(2**62)))
trap("rjust", lambda: size(lambda: "x".rjust(2**62, "*")))
trap("center", lambda: size(lambda: "x".center(2**62)))
trap("ljust63", lambda: size(lambda: "x".ljust(2**63)))
trap("bytes()", lambda: size(lambda: bytes(2**62)))
trap("bytes63", lambda: size(lambda: bytes(2**63)))
trap("bytes-1", lambda: size(lambda: bytes(-1)))
trap("to_bytes", lambda: size(lambda: (5).to_bytes(2**62, "big")))
trap("to_bytes-1", lambda: size(lambda: (5).to_bytes(-1, "big")))
trap("expandtabs", lambda: size(lambda: "a\tb".expandtabs(2**62)))
trap("fwidth", lambda: size(lambda: f"{1:{2**62}}"))
trap("fmtwidth", lambda: size(lambda: "{:>{w}}".format(1, w=2**62)))
trap("urandom", lambda: size(lambda: os.urandom(2**62)))
trap("prec", lambda: format(1.5, ".70000f")[:8])
trap("prec-g", lambda: f"{2.5:.70000}"[:8])
trap("prec-e", lambda: format(1.5, ".70000e")[-6:])
trap("prec-%", lambda: ("%.70000f" % 1.5)[:8])
trap("prec-pct", lambda: format(0.5, ".70000%")[-3:])
trap("prec-big", lambda: format(1.5, ".2147483648f"))
trap("prec-%big", lambda: "%.2147483648f" % 1.5)
trap("digits", lambda: format(1, "99999999999999999999d"))
trap("round", lambda: round(1.25, 70000))
trap("round-huge", lambda: round(1.25, 2**100))
trap("round-neg", lambda: round(1.25e300, -400))
cyc: list[object] = []
cyc.append(cyc)
trap("json-cycle", lambda: json.dumps(cyc))
d: dict[str, object] = {}
d["x"] = [d]
trap("json-dcycle", lambda: json.dumps(d))
shared = [1]
trap("json-shared", lambda: json.dumps([shared, shared, (shared,)]))
def nest(n: int) -> list[object]:
    x: list[object] = []
    for _ in range(n):
        x = [x]
    return x
trap("json-9997", lambda: len(json.dumps(nest(9997))))
trap("json-9998", lambda: len(json.dumps(nest(9998))))
trap("repr-150", lambda: len(repr(nest(150))))
trap("repr-9998", lambda: len(repr(nest(9998))))
trap("eq-9999", lambda: nest(9999) == nest(9999))
trap("lt-9999", lambda: nest(9999) < nest(9999))
t = (cyc,)
cyc.append(t)
dd: dict[str, object] = {}
dd["self"] = dd
dd["l"] = [dd, cyc]
show(cyc, dd, f"{dd}")
"#,
    );
}

#[test]
fn w5_12_native_stack_exhaustion_is_a_recursion_error() {
    // A raised recursion limit no longer lets a deep recursion run the host
    // stack into its guard page: the VM raises `RecursionError` near the
    // end of whatever stack it runs on (here a deliberately small one).
    let src = r#"import sys
sys.setrecursionlimit(10**6)
def f(n: int) -> int:
    if n == 0:
        return 0
    return 1 + f(n - 1)
try:
    f(10**6)
    print("finished")
except RecursionError as e:
    print("RecursionError", e)
print(f(50))
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep.ty");
    std::fs::write(&path, src).unwrap();
    let code = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(scope, || run_file(&path, &[]))
            .unwrap()
            .join()
            .unwrap()
    });
    assert_eq!(code.unwrap(), 0);
}

// ── W5-16: `as!` follows the checked-boundary-cast target table ───────────

/// A generated `typhon_runtime` module the compiled program imports — the
/// oracle a probe's CPython side runs — read from the template in
/// `tyc build` (`const NAME: &str = "\` … `";`), with `alias` appended.
fn runtime_py(name: &str, alias: &str) -> String {
    const BUILD_RS: &str = include_str!("../../tyc/src/commands/build.rs");
    let marker = format!("const {name}: &str = \"\\\n");
    let start = BUILD_RS
        .find(&marker)
        .expect("runtime template in build.rs");
    let body = &BUILD_RS[start + marker.len()..];
    let end = body.find("\n\";").expect("end of the runtime template");
    // Undo the Rust string escapes the template uses.
    let mut out = String::new();
    let mut chars = body[..end].chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out.push('\n');
    out.push_str(alias);
    out.push('\n');
    out
}

fn cast_runtime_py() -> String {
    runtime_py(
        "TYPHON_RUNTIME_CAST_PY",
        "__typhon_checked_cast__ = checked_cast",
    )
}

#[test]
fn w5_16_checked_casts_follow_the_target_table() {
    // Newtypes and `Literal` were accepted unchecked, `type` aliases
    // (generic or not) were rejected or accepted wholesale, `Sequence[int]`
    // / `Mapping[...]` skipped their elements, and parameterised user
    // classes / `Callable` were accepted where the runtime refuses them.
    assert_matches_cpython_with(
        "w5_16_checked_casts_follow_the_target_table",
        r#"from typing import Any, Literal, Sequence, Mapping, MutableMapping, Optional, Union, Callable, Iterator, Collection, AbstractSet, NewType, Protocol
from enum import Enum
UserId = NewType("UserId", int)
Admin = NewType("Admin", UserId)
Ids = NewType("Ids", list[int])
type Pair[T] = tuple[T, T]
type IntList = list[int]
type Color = Literal["red", "green"]
type Json = int | str | list[Json] | dict[str, Json]
type Wrap[T] = list[T]
type PairList[T] = list[Pair[T]]
plain class Named(Protocol):
    name: str
plain class Person:
    def __init__(self, name: str) -> None:
        self.name = name
    def __repr__(self) -> str:
        return "Person(" + self.name + ")"
plain class Box[T]:
    def __init__(self, item: T) -> None:
        self.item = item
plain class Shade(Enum):
    DARK = 1
def cast(label: str, value: object, thunk: Any) -> None:
    try:
        r = thunk(value)
        out.append(label + " = " + repr(r) + (" (same)" if r is value else ""))
    except BaseException as e:
        out.append(label + " ! " + type(e).__name__ + ": " + str(e))
cast("uid", 5, lambda v: __typhon_checked_cast__(v, UserId))
cast("uid bad", "x", lambda v: __typhon_checked_cast__(v, UserId))
cast("admin bad", 3.5, lambda v: __typhon_checked_cast__(v, Admin))
cast("ids", [1, 2], lambda v: __typhon_checked_cast__(v, Ids))
cast("ids bad", [1, "x"], lambda v: __typhon_checked_cast__(v, Ids))
cast("pair", (1, 2), lambda v: __typhon_checked_cast__(v, Pair[int]))
cast("pair bad", (1, "a"), lambda v: __typhon_checked_cast__(v, Pair[int]))
cast("pair arity", (1, 1), lambda v: __typhon_checked_cast__(v, Pair[int, str]))
cast("pair unbound", (1, 1), lambda v: __typhon_checked_cast__(v, Pair))
cast("intlist", [1, 2], lambda v: __typhon_checked_cast__(v, IntList))
cast("intlist bad", [1, "a"], lambda v: __typhon_checked_cast__(v, IntList))
cast("color", "red", lambda v: __typhon_checked_cast__(v, Color))
cast("color bad", "blue", lambda v: __typhon_checked_cast__(v, Color))
cast("lit bool", True, lambda v: __typhon_checked_cast__(v, Literal[1, 2]))
cast("lit enum", Shade.DARK, lambda v: __typhon_checked_cast__(v, Literal[Shade.DARK]))
cast("lit enum bad", 1, lambda v: __typhon_checked_cast__(v, Literal[Shade.DARK]))
cast("json", {"a": [1, "b", {"c": 2}]}, lambda v: __typhon_checked_cast__(v, Json))
cast("json bad", {"a": [1.5]}, lambda v: __typhon_checked_cast__(v, Json))
cyc: list[Any] = []
cyc.append(cyc)
cast("json cyc", cyc, lambda v: __typhon_checked_cast__(v, Json))
cast("wrap nested", [[1, 2]], lambda v: __typhon_checked_cast__(v, Wrap[Pair[int]]))
cast("pairlist bad", [(1, "a")], lambda v: __typhon_checked_cast__(v, PairList[int]))
cast("named", Person("a"), lambda v: __typhon_checked_cast__(v, Named))
cast("named bad", 3, lambda v: __typhon_checked_cast__(v, Named))
cast("named list", [Person("a"), 3], lambda v: __typhon_checked_cast__(v, list[Named]))
cast("seq bad", (1, "a"), lambda v: __typhon_checked_cast__(v, Sequence[int]))
cast("seq bare bad", {1}, lambda v: __typhon_checked_cast__(v, Sequence))
cast("map bad", {"a": "b"}, lambda v: __typhon_checked_cast__(v, Mapping[str, int]))
cast("mutmap", {"a": 1}, lambda v: __typhon_checked_cast__(v, MutableMapping[str, int]))
cast("coll", {1: 2}, lambda v: __typhon_checked_cast__(v, Collection[int]))
cast("abset keys", {"a": 1}.keys(), lambda v: list(__typhon_checked_cast__(v, AbstractSet[str])))
cast("frozenset as set", frozenset({1}), lambda v: __typhon_checked_cast__(v, set[int]))
cast("opt bad", "a", lambda v: __typhon_checked_cast__(v, Optional[int]))
cast("union3 bad", 1.5, lambda v: __typhon_checked_cast__(v, Union[int, str, None]))
cast("pipe bad", 1.5, lambda v: __typhon_checked_cast__(v, int | None))
cast("bool float", True, lambda v: __typhon_checked_cast__(v, float))
cast("class val", int, lambda v: __typhon_checked_cast__(v, int))
cast("tuple empty", (), lambda v: __typhon_checked_cast__(v, tuple[()]))
cast("box", Box(1), lambda v: __typhon_checked_cast__(v, Box[int]))
cast("list box", [Box(1)], lambda v: __typhon_checked_cast__(v, list[Box[int]]))
cast("callable", len, lambda v: __typhon_checked_cast__(v, Callable[[int], str]))
cast("iterator", iter([1]), lambda v: __typhon_checked_cast__(v, Iterator[int]))
cast("type", int, lambda v: __typhon_checked_cast__(v, type[int]))
cast("none bad", 0, lambda v: __typhon_checked_cast__(v, None))
show(UserId, UserId(3), UserId.__supertype__ is int)
"#,
        &cast_runtime_py(),
    );
}

// ── W5-17: `freeze let` values ────────────────────────────────────────────

#[test]
fn w5_17_frozen_values_match_the_runtime() {
    // A frozen dataclass instance passes through `freeze let` unchanged
    // (its fields are not rebuilt), and a frozen dict is a `mappingproxy`
    // — which `type()` names and `isinstance(_, dict)` rejects.
    assert_matches_cpython_with(
        "w5_17_frozen_values_match_the_runtime",
        r#"from dataclasses import dataclass
@dataclass(frozen=True)
plain class B:
    values: list[int]
b = B([1])
f = __typhon_freeze__(b)
f.values.append(2)
show(f, f is b, b.values)
d = __typhon_freeze__({"a": [1, 2], "b": {"c": 3}})
show(type(d).__name__, type(d["a"]).__name__, type(d["b"]).__name__, d)
show(isinstance(d, dict), str(d), repr(d), len(d), d["a"])
s = __typhon_freeze__({1, 2})
show(type(s).__name__, s, isinstance(s, set), isinstance(s, frozenset))
l = __typhon_freeze__([1, [2, 3], {"k": {4}}])
show(type(l).__name__, l, type(l[2]).__name__)
trap("setitem", lambda: d.__setitem__("x", 1))
trap("dict(d)", lambda: dict(d))
trap("copy type", lambda: type(d.copy()).__name__)
trap("union", lambda: d | {"z": 1})
trap("eq", lambda: d == {"a": (1, 2), "b": {"c": 3}})
"#,
        &runtime_py(
            "TYPHON_RUNTIME_FREEZE_PY",
            "__typhon_freeze__ = deep_freeze",
        ),
    );
}

// ── W5-18: remaining silent divergences ───────────────────────────────────

#[test]
fn w5_18_dict_fromkeys_and_class_level_calls() {
    // `dict.fromkeys` was dispatched as an unbound method on its first
    // argument (`list has no method 'fromkeys'`); corpus/valid/textwrap.ty
    // builds a translation table with it in a class body.
    assert_matches_cpython(
        "w5_18_dict_fromkeys_and_class_level_calls",
        r#"plain class Wrapper:
    whitespace = "\t\n\x0b\x0c\r "
    trans = dict.fromkeys(map(ord, whitespace), ord(" "))
show(Wrapper.trans, "a\tb".translate(Wrapper.trans))
show(dict.fromkeys(["a", "b"], 0), dict.fromkeys("ab"), {}.fromkeys([1, 1, 2], []))
d = dict.fromkeys(range(3))
show(d, type(d).__name__, str.maketrans("ab", "xy"), "abc".translate(str.maketrans("ab", "xy")))
trap("no args", lambda: dict.fromkeys())
"#,
    );
}

#[test]
fn w5_18_values_and_errors_match_cpython() {
    assert_matches_cpython(
        "w5_18_values_and_errors_match_cpython",
        r#"import sys
trap("float tie", lambda: (-4819706.21) ** 2)
trap("float tie2", lambda: 1e15 + 0.3)
trap("fstr conv", lambda: f"{True!s:<8}|{'a'!r:>5}|")
trap("type3", lambda: type("X", (), {"a": 1}).a)
trap("type3 bad", lambda: type("Q", [], {}))
trap("split empty", lambda: "abc".split(""))
trap("float big", lambda: float(10**400))
trap("pow big", lambda: 2.0 ** 10000)
trap("floordiv big", lambda: 10**400 // 3.0)
trap("truediv", lambda: (10**400 / 10**399, -(10**400) / 3 < 0, 7 / -2, (2**80 + 1) / 2**27))
trap("truediv big", lambda: 10**400 / 3)
trap("neg pow big", lambda: (10**400) ** -1)
trap("%c", lambda: "%c" % 0x110000)
trap("%c str", lambda: "%c" % "ab")
trap("fmt ,_", lambda: format(1, ",_"))
trap("int str limit", lambda: len(str(10**5000)))
trap("str int limit", lambda: int("1" * 5000))
trap("hex big", lambda: format(10**5000, "x")[:4])
sys.set_int_max_str_digits(0)
trap("unlimited", lambda: len(str(10**5000)))
sys.set_int_max_str_digits(4300)
trap("slice huge", lambda: "abc"[2**64:] + "abc"[: -(2**70)])
trap("index huge", lambda: [1][-(2**70)])
def raise_from() -> object:
    try:
        e = ValueError("x")
        raise e from e
    except ValueError as err:
        return (err.__cause__ is err, err is e)
trap("raise from self", raise_from)
trap("None fmt", lambda: f"{None:>6}")
trap("list fmt", lambda: format([1], "x"))
trap("int in str", lambda: 1 in "abc")
trap("bit_length", lambda: (True.bit_length(), (5).bit_length()))
trap("keys views", lambda: ({1: 2}.keys() <= {1: 2, 3: 4}.keys(), {1: 2}.keys() == {1}, {1: 2}.items() == {(1, 2)}, {1: 2}.keys() == [1]))
trap("unions", lambda: (repr(int | str), repr(int | None), isinstance(1, int | str), isinstance(None, int | None), issubclass(bool, int | str)))
def g() -> object:
    yield 1
gen = g()
trap("iter gen", lambda: iter(gen) is gen)
"#,
    );
}

#[test]
fn w5_18_object_model_matches_cpython() {
    assert_matches_cpython(
        "w5_18_object_model_matches_cpython",
        r#"import asyncio
from abc import ABC, abstractmethod
from dataclasses import dataclass
from functools import total_ordering
plain class Abs(ABC):
    @abstractmethod
    def f(self) -> int: ...
    @abstractmethod
    def b(self) -> int: ...
plain class Half(Abs):
    def f(self) -> int:
        return 1
plain class Full(Half):
    def b(self) -> int:
        return 2
trap("abstract", lambda: Abs())
trap("half", lambda: Half())
show(Full().f() + Full().b())
@dataclass(repr=False)
plain class NR:
    a: int
show(repr(NR(1))[:12])
plain class Doc:
    """
    Indented doc.
      More.
    """
def fdoc() -> None:
    """First line.

        Body.
    """
show(repr(Doc.__doc__), repr(fdoc.__doc__))
plain class Seq:
    def __init__(self, n: int) -> None:
        self.n = n
    def __getitem__(self, i: int) -> int:
        if i >= self.n:
            raise IndexError
        return i * 10
show(20 in Seq(5), 25 in Seq(5))
@total_ordering
plain class TO:
    def __init__(self, v: int) -> None:
        self.v = v
    def __eq__(self, o: object) -> bool:
        return self.v == o.v
    def __lt__(self, o: "TO") -> bool:
        return self.v < o.v
show(TO(1) <= TO(2), TO(3) >= TO(2), TO(1) > TO(2))
@dataclass(order=True)
plain class OD:
    a: int
    b: str = "x"
show(OD(1) < OD(2), [o.a for o in sorted([OD(3), OD(1), OD(2)])], OD(1, "a") < OD(1, "b"))
registry: list[str] = []
def register(cls: type) -> type:
    registry.append(cls.__name__)
    return cls
@register
plain class Reg:
    pass
show(registry, "__lt__" in TO.__dict__, "__le__" in TO.__dict__)
plain class V:
    def __init__(self, x: int) -> None:
        self.x = x
    def __eq__(self, o: object) -> bool:
        if not isinstance(o, V):
            return NotImplemented
        return self.x == o.x
    def __add__(self, o: object) -> "V":
        if isinstance(o, V):
            return V(self.x + o.x)
        return NotImplemented
    def __radd__(self, o: object) -> "V":
        if isinstance(o, int):
            return V(self.x + o)
        return NotImplemented
show(V(1) == V(1), V(1) == 1, (3 + V(2)).x, repr(NotImplemented))
trap("add bad", lambda: V(1) + "s")
plain class Box[T]:
    def __init__(self, v: T) -> None:
        self.v = v
plain class Named(Box[int]):
    pass
show(Named(3).v, Box[int], Box[int](5).v, list[int], tuple[int, ...], Box[int] | None)
plain class Plain:
    pass
trap("plain subscript", lambda: Plain[int])
X = type("X", (ValueError,), {"code": 7})
try:
    raise X("boom")
except ValueError as e:
    show("caught", type(e).__name__, e, e.code)
plain class Fut:
    def __init__(self, v: int) -> None:
        self.v = v
    def __await__(self) -> object:
        if False:
            yield
        return self.v * 2
async def main() -> None:
    show("awaited", await Fut(21))
asyncio.run(main())
"#,
    );
}

// ── W5-22: performance cliffs (the fast paths must keep CPython's answers) ──

#[test]
fn w5_22_fast_paths_keep_cpython_semantics() {
    assert_matches_cpython(
        "w5_22_fast_paths_keep_cpython_semantics",
        r#"from collections import OrderedDict, deque
d = {k: k * k for k in range(40)}
for k in range(0, 40, 3):
    del d[k]
show(len(d), list(d)[:5], list(reversed(d))[:5], next(iter(d)), d.popitem())
for k in range(1, 30):
    d.pop(k, None)
d[3] = "back"
d[100] = "new"
show(d, list(d.items())[-2:], list(reversed(d.values())))
it = iter(d)
show(next(it), next(it))
d[3] = "again"
show(list(it))
del d[37]
trap("after delete", lambda: next(it))
e = dict.fromkeys(range(10))
for k in list(e):
    if k % 2:
        del e[k]
e.setdefault(1, "one")
show(e, {**e}, e == {0: None, 2: None, 4: None, 6: None, 8: None, 1: "one"})
o = OrderedDict((k, str(k)) for k in range(6))
show(o.popitem(last=False), o.popitem(), o)
o.move_to_end(2, last=False)
o.move_to_end(1)
show(o, list(reversed(o)), len(o), bool(o))
q = deque(range(5))
q.appendleft(-1)
q.extendleft([-3, -2])
show(q, q.popleft(), q.pop(), q[0], q[-1], len(q), 3 in q)
for _ in range(40):
    q.append(q.popleft())
show(q, q.index(0), list(q)[2:4])
q.rotate(-2)
show(q, deque(maxlen=2) == deque(), deque([1, 2, 3], maxlen=2))
r = deque()
for i in range(30):
    r.appendleft(i)
    if i % 4 == 0:
        r.pop()
show(r, len(r), r[3], r[-3])
s = "aé€😀" * 40
show(len(s), s[3], s[-1], s[5:9], s[150:], s[2:157:37], s.index("😀", 50))
a = "x" * 100 + "y"
show(len(a), a[100], a[95:], a[-1])
"#,
    );
}

// ── W5-21: keyword arguments bind to CPython's signatures ─────────────────

#[test]
fn w5_21_keyword_arguments_bind_like_cpython() {
    assert_matches_cpython(
        "w5_21_keyword_arguments_bind_like_cpython",
        r#"import functools
import heapq
import json
import math
trap("round", lambda: (round(3.14159, ndigits=2), round(number=2.5)))
trap("int base", lambda: (int("ff", base=16), functools.partial(int, base=2)("101")))
trap("prod start", lambda: math.prod([2, 3], start=5))
trap("nlargest", lambda: (heapq.nlargest(2, [1, 5, 3], key=lambda x: -x), heapq.nsmallest(2, [1, 5, 3], key=lambda x: -x)))
trap("loads hooks", lambda: (json.loads('{"a": {"b": 1}}', object_hook=lambda d: sorted(d)), json.loads('{"a": 1, "b": 2}', object_pairs_hook=lambda p: p)))
trap("replace", lambda: "aaa".replace("a", "b", count=2))
trap("split", lambda: ("a b c".split(maxsplit=1), b"a b".split(sep=b" ", maxsplit=1), "a\tb".expandtabs(tabsize=2)))
trap("encode", lambda: ("é".encode(errors="replace", encoding="ascii"), b"a".decode(encoding="ascii")))
trap("to_bytes", lambda: ((5).to_bytes(2, byteorder="little"), (-5).to_bytes(2, "big", signed=True)))
trap("hex sep", lambda: (b"\x01\x02".hex(sep=":"), b"\x01\x02\x03".hex(sep="-", bytes_per_sep=-2)))
trap("splitlines", lambda: "a\nb".splitlines(keepends=True))
trap("sorted None", lambda: sorted([3, 1], key=None, reverse=True))
trap("dict.pop kw", lambda: {}.pop("k", default=1))
trap("list.index kw", lambda: [1, 2].index(2, start=0))
trap("dict.get kw", lambda: {}.get("a", default=2))
trap("unknown kw", lambda: "a".split(foo=1))
trap("twice", lambda: "a".split("a", sep="b"))
trap("count kw", lambda: "a".count(sub="a"))
"#,
    );
}

// ── PR #493 review: VM ↔ CPython parity ───────────────────────────────────

#[test]
fn pr493_a_class_named_object_is_not_the_builtin_root() {
    // The builtin `object` placeholder was recognised by its name, so a user
    // class called `object` was moved to the end of `C(object, B)`'s MRO
    // (`C().who()` found `B.who`), dropped from `__mro__`, and treated as the
    // root by `isinstance` / `issubclass`.
    assert_matches_cpython(
        "pr493_a_class_named_object_is_not_the_builtin_root",
        r#"root = object
plain class object:
    def who(self) -> str:
        return "user object"
plain class B:
    def who(self) -> str:
        return "B"
plain class C(object, B):
    pass
show(C().who(), [k.__name__ for k in C.__mro__], len(C.__mro__))
show(isinstance(5, object), isinstance(C(), object), isinstance(B(), object))
show(issubclass(int, object), issubclass(C, object), issubclass(B, object))
plain class D(B, root):
    pass
show(isinstance(5, root), isinstance(C(), root), issubclass(C, root), issubclass(object, root))
show([k.__name__ for k in D.__mro__], D().who())
"#,
    );
}

#[test]
fn pr493_checked_casts_resolve_a_rebound_target_name() {
    // The cast picked the container kind (and the `float` / `Any` /
    // `object` shortcuts) from the target's spelling, so after
    // `list = tuple` the VM checked `x as! list[int]` against `list` where
    // the compiled program checks `tuple[int]`.
    assert_matches_cpython_with(
        "pr493_checked_casts_resolve_a_rebound_target_name",
        r#"from typing import Any, List, Optional, Sequence
plain class Obj:
    pass
def cast(label: str, value: object, thunk: Any) -> None:
    try:
        r = thunk(value)
        out.append(label + " = " + repr(r))
    except BaseException as e:
        out.append(label + " ! " + type(e).__name__ + ": " + str(e))
def rebound() -> None:
    list = tuple
    float = int
    L = List
    Opt = Optional
    Seq = Sequence
    object = Obj
    Any = int
    cast("list[int] one", (1,), lambda v: __typhon_checked_cast__(v, list[int]))
    cast("list[int] two", (1, 2), lambda v: __typhon_checked_cast__(v, list[int]))
    cast("list[int] list", [1], lambda v: __typhon_checked_cast__(v, list[int]))
    cast("list bare", (1, 2), lambda v: __typhon_checked_cast__(v, list))
    cast("list bare list", [1], lambda v: __typhon_checked_cast__(v, list))
    cast("float", 1.5, lambda v: __typhon_checked_cast__(v, float))
    cast("float int", 2, lambda v: __typhon_checked_cast__(v, float))
    cast("L[int]", [1], lambda v: __typhon_checked_cast__(v, L[int]))
    cast("L[int] bad", [1, "a"], lambda v: __typhon_checked_cast__(v, L[int]))
    cast("Opt[int]", None, lambda v: __typhon_checked_cast__(v, Opt[int]))
    cast("Opt[int] bad", "a", lambda v: __typhon_checked_cast__(v, Opt[int]))
    cast("Seq[int] bad", (1, "a"), lambda v: __typhon_checked_cast__(v, Seq[int]))
    cast("object", 5, lambda v: __typhon_checked_cast__(v, object))
    cast("object obj", Obj(), lambda v: type(__typhon_checked_cast__(v, object)).__name__)
    cast("Any", "a", lambda v: __typhon_checked_cast__(v, Any))
rebound()
cast("list[int] builtin", (1,), lambda v: __typhon_checked_cast__(v, list[int]))
cast("object builtin", 5, lambda v: __typhon_checked_cast__(v, object))
cast("Any builtin", "a", lambda v: __typhon_checked_cast__(v, Any))
"#,
        &cast_runtime_py(),
    );
}

#[test]
fn pr493_datetime_and_path_instances_refuse_attribute_writes() {
    // `freeze let` passes a date / time / timedelta / timezone / path
    // through as already immutable, but the VM's shims stored attributes
    // like any plain instance: `d.year = 1`, `d.foo = 1` and `p.foo = 1`
    // succeeded where CPython's C types (and `PurePath`'s slots) refuse
    // them — frozen or not.
    assert_matches_cpython_with(
        "pr493_datetime_and_path_instances_refuse_attribute_writes",
        r#"import datetime
import pathlib
def attempt(label: str, obj: object, name: str) -> None:
    trap(label + " set " + name, lambda: setattr(obj, name, 1))
    trap(label + " del " + name, lambda: delattr(obj, name))
D = __typhon_freeze__(datetime.date(2020, 1, 2))
P = __typhon_freeze__(pathlib.PurePosixPath("a/b"))
objs = [
    ("D", D),
    ("datetime", datetime.datetime(2020, 1, 2, 3, 4)),
    ("time", datetime.time(1, 2)),
    ("timedelta", datetime.timedelta(1)),
    ("utc", datetime.timezone.utc),
    ("tz", datetime.timezone(datetime.timedelta(hours=1))),
    ("iso", datetime.date(2020, 1, 2).isocalendar()),
    ("P", P),
    ("path", pathlib.Path("a/b")),
]
names = ["year", "month", "day", "hour", "minute", "second", "microsecond", "tzinfo", "fold",
         "days", "seconds", "microseconds", "week", "weekday", "foo", "name", "parts",
         "min", "max", "resolution", "utc", "isoformat", "joinpath", "_drv"]
for label, obj in objs:
    for name in names:
        attempt(label, obj, name)
show(D, D.year, P, P.name, datetime.timedelta(1).days)
plain class MyDate(datetime.date):
    pass
m = MyDate(2020, 1, 2)
trap("sub foo", lambda: setattr(m, "foo", 1))
trap("sub year", lambda: setattr(m, "year", 1))
trap("sub min", lambda: setattr(m, "min", 1))
show(m.foo, m.year, m.min)
plain class Zone(datetime.tzinfo):
    pass
z = Zone()
z.label = "x"
show(z.label)
"#,
        &runtime_py(
            "TYPHON_RUNTIME_FREEZE_PY",
            "__typhon_freeze__ = deep_freeze",
        ),
    );
}

#[test]
fn pr493_set_operators_keep_cpython_result_types_and_identity() {
    // `frozenset | d.keys()` came back a frozenset (the view's reflected
    // operator builds a `set`), and `s |= t` / `&=` / `-=` / `^=` rebound
    // the name to a new set, so an alias (or the object behind a field)
    // never saw the change.
    assert_matches_cpython(
        "pr493_set_operators_keep_cpython_result_types_and_identity",
        r#"f = frozenset([1, 2])
d = {"a": 1}
show(repr(f | d.keys()), repr(d.keys() | f), repr(f - d.keys()), repr(f & d.keys()), repr(f ^ d.keys()))
show(repr(f | {3}), repr({3} | f), repr(f.union([4])), repr(f.intersection({1})), repr(f.difference([1])), repr(f.symmetric_difference({9})))
s = {1}
alias = s
s |= {2}
s &= {1, 2, 3}
s -= {9}
s ^= {5}
show(repr(s), repr(alias), s is alias)
t = {1}
talias = t
t |= frozenset([2])
show(repr(t), t is talias)
u = {1}
ualias = u
u |= d.keys()
show(repr(u), repr(ualias), u is ualias)
v = {1, 2}
v ^= v
w = {1, 2}
w -= w
x = {1, 2}
x &= x
x |= x
show(repr(v), repr(w), repr(x))
g = frozenset([1])
galias = g
g |= {2}
show(repr(g), repr(galias), g is galias)
plain class Holder:
    def __init__(self) -> None:
        self.items = {1}
h = Holder()
keep = h.items
h.items |= {7}
show(repr(keep), keep is h.items)
lst = [{1}]
first = lst[0]
lst[0] -= {1}
show(repr(lst), first is lst[0])
"#,
    );
}

// ── 2026-10-07 root-cause investigation: VM ↔ CPython parity ──────────────

#[test]
fn rc_type_objects_are_the_builtin_names() {
    // `type(d) is dict` and `d.__class__ is dict` were `False`: `type()`
    // returned a stand-in class while `dict` is the constructor native.
    assert_matches_cpython(
        "rc_type_objects_are_the_builtin_names",
        r#"plain class P:
    pass
def local_dict() -> object:
    plain class dict:
        pass
    return dict()
d = {"a": 1}
show(type(d) is dict, d.__class__ is dict, dict is type(d), type(d) is not dict, type(d) is list)
show(type([1]) is list, type({1}) is set, type(frozenset()) is frozenset, type((1,)) is tuple)
show(type("a") is str, type(1) is int, type(1.5) is float, type(True) is bool, type(True) is int)
show(type(b"") is bytes, type(range(1)) is range, (1).__class__ is int, type(d) is type({}))
p = P()
show(type(p) is P, p.__class__ is P, type(P) is type, type(type(1)) is type)
try:
    raise ValueError("x")
except ValueError as e:
    show(type(e) is ValueError, e.__class__ is ValueError, type(e) is Exception, e.__class__.__name__)
o = local_dict()
show(type(o) is dict, type(o).__name__)
"#,
    );
}

#[test]
fn rc_decorated_methods_bind_self() {
    // A method wrapped by `@contextmanager` / `@cache` is a function on
    // CPython and binds `self`; the VM's native wrapper did not.
    assert_matches_cpython(
        "rc_decorated_methods_bind_self",
        r#"from contextlib import contextmanager
from functools import cache, lru_cache
plain class Mutex:
    def __init__(self) -> None:
        self.held = False
    @contextmanager
    def lock(self, tag="t"):
        self.held = True
        try:
            yield tag + "!"
        finally:
            self.held = False
    @staticmethod
    @contextmanager
    def quiet(x):
        yield x * 2
    @classmethod
    @contextmanager
    def make(cls, x):
        yield cls.__name__ + str(x)
    @cache
    def twice(self, x):
        return x * 2
    @staticmethod
    @cache
    def square(x):
        return x * x
    @classmethod
    @lru_cache(maxsize=None)
    def tag(cls, x):
        return cls.__name__ + str(x)
plain class Sub(Mutex):
    @contextmanager
    def lock(self, tag="s"):
        with super().lock("sub-" + tag) as inner:
            yield inner
m = Mutex()
with m.lock() as k:
    show(k, m.held)
show(m.held)
with m.lock(tag="kw") as k:
    show(k)
bound = m.lock
with bound("b") as k:
    show(k)
with Mutex.lock(m, "u") as k:
    show(k)
with Mutex.quiet(4) as q:
    show(q)
with m.quiet(5) as q:
    show(q)
with Mutex.make(7) as q:
    show(q)
with Sub().make(1) as q:
    show(q)
with Sub().lock() as q:
    show(q)
show(m.twice(4), Mutex.square(3), m.square(4), Mutex.tag(1), Sub().tag(2))
# Read off the class, a plain method stays unbound (the VM's wording for a
# missing argument differs, so only the type is compared).
try:
    Mutex.twice(4)
except TypeError as e:
    show("unbound", type(e).__name__)
"#,
    );
}

#[test]
fn rc_builtin_method_arity_raises() {
    // `xs.append(1, 2)` appended `1` and `d.get(k, a, b)` returned `a`;
    // CPython checks the positional count before the body runs.
    assert_matches_cpython(
        "rc_builtin_method_arity_raises",
        r#"xs = [3, 1, 2]
s = {1}
d = {"a": 1}
trap("append2", lambda: xs.append(1, 2))
trap("append0", lambda: xs.append())
trap("extend2", lambda: xs.extend([1], [2]))
trap("insert1", lambda: xs.insert(0))
trap("insert3", lambda: xs.insert(0, 1, 2))
trap("pop2", lambda: xs.pop(0, 1))
trap("remove0", lambda: xs.remove())
trap("clear1", lambda: xs.clear(1))
trap("index4", lambda: xs.index(1, 0, 3, 4))
trap("count2", lambda: xs.count(1, 2))
trap("sort1", lambda: xs.sort(None))
trap("list.append", lambda: list.append(xs, 1, 2))
trap("add2", lambda: s.add(1, 2))
trap("discard0", lambda: s.discard())
trap("issubset2", lambda: s.issubset({1}, {2}))
trap("fs issubset0", lambda: frozenset([1]).issubset())
trap("fs add", lambda: frozenset([1]).add(1))
trap("get3", lambda: d.get("a", 1, 2))
trap("get0", lambda: d.get())
trap("get kw", lambda: d.get("a", default=0))
trap("pop kw", lambda: d.pop("a", default=0))
trap("pop0", lambda: d.pop())
trap("setdefault3", lambda: d.setdefault("a", 1, 2))
trap("keys1", lambda: d.keys(1))
trap("update2", lambda: d.update({}, {}))
trap("upper1", lambda: "a".upper(1))
trap("split3", lambda: "a,b".split(",", 1, 2))
trap("replace1", lambda: "a".replace("a"))
trap("replace4", lambda: "a".replace("a", "b", 1, 2))
trap("join0", lambda: "a".join())
trap("strip2", lambda: "a".strip("a", "b"))
trap("startswith0", lambda: "a".startswith())
trap("center0", lambda: "a".center())
trap("encode3", lambda: "a".encode("utf-8", "strict", 1))
trap("tuple count0", lambda: (1, 2).count())
trap("bit_length1", lambda: (5).bit_length(1))
trap("bool bit_length1", lambda: True.bit_length(1))
trap("to_bytes3", lambda: (5).to_bytes(1, "big", 3))
trap("float hex1", lambda: (1.5).hex(1))
trap("bytes split3", lambda: b"a b".split(b" ", 1, 2))
trap("bytes replace1", lambda: b"a".replace(b"a"))
show(xs, s, d)
trap("ok append", lambda: (xs.append(9), xs)[1])
trap("ok get", lambda: d.get("zz", 5))
trap("ok split", lambda: "a b c".split(maxsplit=1))
trap("ok to_bytes", lambda: (5).to_bytes(2, "little", signed=True))
trap("ok sort", lambda: (xs.sort(key=lambda v: -v), xs)[1])
trap("ok update", lambda: (d.update({"b": 2}, c=3), d.update(e=5), d.update(), d)[3])
trap("ok self update", lambda: (d.update(d, a=0), d)[1])
trap("ok set update", lambda: (s.update({2}, [3]), s)[1])
"#,
    );
}

#[test]
fn rc_int_is_integer() {
    assert_matches_cpython(
        "rc_int_is_integer",
        r#"trap("int", lambda: (3).is_integer())
trap("neg", lambda: (-7).is_integer())
trap("big", lambda: (2 ** 100).is_integer())
trap("bool", lambda: True.is_integer())
trap("unbound", lambda: int.is_integer(5))
trap("float", lambda: (2.5).is_integer())
trap("arg", lambda: (3).is_integer(1))
trap("hasattr", lambda: hasattr(3, "is_integer"))
"#,
    );
}

// ── 2026-10-07 review round 2: VM ↔ CPython parity ────────────────────────

#[test]
fn rc2_type_objects_relate_like_cpython() {
    // `issubclass(e.__class__, Exception)` went False once `__class__` gave
    // the concrete kind's stand-in (it has no bases); `type(Color) is type`
    // went True for an enum / ABC class; and `type(x) is T` stayed False for
    // the builtins the VM models as shim classes, and for `type(int)`.
    assert_matches_cpython(
        "rc2_type_objects_relate_like_cpython",
        r#"from collections import defaultdict, Counter, OrderedDict
from enum import Enum, IntEnum
from abc import ABC, ABCMeta, abstractmethod
from typing import Protocol
def is_error(e):
    return issubclass(e.__class__, Exception)
try:
    d = {}
    d["missing"]
except KeyError as err:
    show(is_error(err), issubclass(err.__class__, LookupError), issubclass(type(err), BaseException))
    show(issubclass(type(err), (ValueError, Exception)), issubclass(type(err), KeyError), issubclass(type(err), ValueError))
    show([c.__name__ for c in type(err).__mro__], type(err).__mro__[1] is LookupError)
show(is_error(ValueError("x")), is_error(3), issubclass(type(True), int), issubclass(type(1), object))
show(issubclass(type(ZeroDivisionError()), ArithmeticError), issubclass(type(FileNotFoundError()), OSError))
show(issubclass(type(KeyboardInterrupt()), Exception), issubclass(type(KeyboardInterrupt()), BaseException))
show(issubclass(int, Exception), issubclass(type(1), BaseException), issubclass(str, ValueError), issubclass(bool, int))
show([c.__name__ for c in type(True).__mro__], [c.__name__ for c in type(StopIteration()).__mro__])
class Color(Enum):
    RED = 1
class N(IntEnum):
    A = 1
plain class Shape(ABC):
    @abstractmethod
    def area(self): ...
plain class M(metaclass=ABCMeta):
    pass
plain class Sub(Shape):
    def area(self):
        return 1.0
plain class Proto(Protocol):
    def run(self) -> None: ...
plain class Plain:
    @property
    def p(self):
        return 1
show(type(Color) is type, type(N) is type, type(Shape) is type, type(M) is type, type(Sub) is type, type(Proto) is type, type(Plain) is type)
show(type(Color) == type, type(Shape) == type, type(Plain) == type, type(Color) is not type)
show(type(Color).__name__, type(N).__name__, type(Shape).__name__, type(M).__name__, type(Proto).__name__, type(Plain).__name__)
show(repr(type(Color)), repr(type(Shape)), type(type(Color)) is type)
show(isinstance(Color, type), isinstance(Plain, type), isinstance(int, type), isinstance(len, type), isinstance(Color, type(Color)))
ba = bytearray(b"x")
show(type(1) is int, type({}) is dict, type(ba) is bytearray, type(ba) == bytearray, ba.__class__ is bytearray)
show(type(Plain.p) is property, type(Plain.p) == property, type(property(lambda s: 1)) is property)
dd = defaultdict(int)
show(type(dd) is defaultdict, type(dd) == defaultdict, type(dd).__name__, dd.default_factory is int)
show(type(int) is type, type(str) is type, type(int) == type, int.__class__ is type, type(int).__name__, type(ValueError) is type, type(len) is type)
show(type(IOError("x")) is OSError, IOError is OSError, EnvironmentError is OSError, type(IOError("x")).__name__)
try:
    raise IOError("disk")
except OSError as e:
    show("caught", type(e).__name__, e)
show(id(type(1)) == id(int), id(type(ba)) == id(bytearray), hash(type(1)) == hash(int))
reg = {int: "i", bytearray: "b", str: "s"}
show(reg.get(type(1)), reg.get(type(ba)), list(reg)[0] is int, repr(reg))
show(type(Counter()) is Counter, type(OrderedDict()) is OrderedDict)
def local_bytearray():
    plain class bytearray:
        pass
    return bytearray()
o = local_bytearray()
show(type(o) is bytearray, type(o).__name__)
"#,
    );
}

#[test]
fn rc2_builtin_method_optional_args_are_read() {
    // The arity table admitted `start` / `stop` / `maxsplit` and tuple
    // prefixes, but the bytes and tuple handlers read only the first
    // argument: `b.split(b" ", 1)` split everywhere, `t.index(x, 2)` started
    // from 0.
    assert_matches_cpython(
        "rc2_builtin_method_optional_args_are_read",
        r#"b = b"a b c a"
tp = (1, 2, 3, 2)
xs = [1, 2, 3, 2]
trap("t.index2", lambda: tp.index(2, 2))
trap("t.index3", lambda: tp.index(2, 2, 4))
trap("t.index3miss", lambda: (5, 6, 5, 6).index(5, 1, 2))
trap("t.indexneg", lambda: tp.index(2, -1))
trap("t.indexneg2", lambda: tp.index(2, -3, -1))
trap("t.indexbig", lambda: tp.index(2, 0, 10 ** 30))
trap("t.indexhuge", lambda: tp.index(2, 10 ** 30))
trap("t.indexbool", lambda: tp.index(2, True))
trap("t.indexfloat", lambda: tp.index(2, 1.5))
trap("t.indexnone", lambda: tp.index(2, None))
trap("l.index3", lambda: xs.index(2, 2, 4))
trap("l.index3miss", lambda: [5, 6, 5, 6].index(5, 1, 2))
trap("l.indexnone", lambda: xs.index(2, None))
trap("b.split1", lambda: b.split(b" ", 1))
trap("b.splitkw", lambda: b.split(maxsplit=1))
trap("b.splitkw2", lambda: b.split(b" ", maxsplit=2))
trap("b.splitnone1", lambda: b.split(None, 1))
trap("b.split0", lambda: b.split(b" ", 0))
trap("b.splitneg", lambda: b.split(b" ", -1))
trap("b.rsplit1", lambda: b.rsplit(b" ", 1))
trap("b.rsplitkw", lambda: b.rsplit(maxsplit=1))
trap("b.rsplitnone2", lambda: b"  a  b c  ".rsplit(None, 2))
trap("b.splitws1", lambda: b"  a  b c  ".split(None, 1))
trap("b.splitws0", lambda: b"  a  b c  ".split(None, 0))
trap("b.rsplitws0", lambda: b"  a  b c  ".rsplit(None, 0))
trap("b.splitwsall", lambda: b"   ".split(None, 1))
trap("b.splitempty", lambda: b"".split())
trap("b.rsplitmulti", lambda: b"a::b::c".rsplit(b"::", 1))
trap("b.splitmulti", lambda: b"a::b::c".split(b"::", 1))
trap("b.splitfloat", lambda: b.split(b" ", 1.0))
trap("b.splitbool", lambda: b.split(b" ", True))
trap("b.splitbytearray", lambda: b.split(bytearray(b" "), 1))
trap("ba.split", lambda: bytearray(b"a b c").split(None, 1))
trap("b.rfind3", lambda: b.rfind(b"a", 0, 3))
trap("b.rfind2", lambda: b.rfind(b"a", 1))
trap("b.rfindneg", lambda: b.rfind(b"a", -3, -1))
trap("b.rindex3", lambda: b"xa ya za".rindex(b"a", 0, 4))
trap("b.rindexmiss", lambda: b"xa ya za".rindex(b"a", 2, 4))
trap("b.rfindint", lambda: b.rfind(97, 0, 3))
trap("b.rfindempty", lambda: b.rfind(b"", 2, 4))
trap("b.rfindempty2", lambda: b.rfind(b"", 5, 2))
trap("b.rfindnone", lambda: b.rfind(b"a", None, 3))
trap("b.find3", lambda: b.find(b"a", 1, 7))
trap("b.findempty", lambda: b.find(b"", 9))
trap("b.countempty", lambda: b.count(b"", 9))
trap("b.count3", lambda: b.count(b"a", 1))
trap("b.sw2", lambda: b.startswith(b"b", 2))
trap("b.sw3", lambda: b.startswith(b"b", 2, 2))
trap("b.sw3b", lambda: b.startswith(b"b", 2, 3))
trap("b.swtuple", lambda: b"xa ya".startswith((b"q", b"xa")))
trap("b.swtuplemiss", lambda: b"xa ya".startswith((b"q", b"z")))
trap("b.swtuple2", lambda: b"xa ya".startswith((b"q", b"ya"), 3))
trap("b.swlazy", lambda: b"xa".startswith((b"x", "s")))
trap("b.swlazybad", lambda: b"xa".startswith(("s", b"x")))
trap("b.swbadrange", lambda: b"xa".startswith((b"q", "s"), 9))
trap("b.swneg", lambda: b.startswith(b"a", -1))
trap("b.swempty", lambda: b.startswith(b"", 7))
trap("b.swempty2", lambda: b.startswith(b"", 8))
trap("b.swbad", lambda: b.startswith("a"))
trap("b.swint", lambda: b.startswith(97))
trap("b.swbytearray", lambda: b.startswith(bytearray(b"a ")))
trap("b.ew3", lambda: b"xa ya za".endswith(b"xa", 0, 2))
trap("b.ew2", lambda: b"xa ya za".endswith(b"za", 3))
trap("b.ewtuple", lambda: b"xa ya za".endswith((b"q", b"za")))
trap("b.ewneg", lambda: b"xa ya za".endswith(b"ya", -5, -3))
trap("b.ewempty", lambda: b.endswith(b"", 3, 2))
trap("b.ewnone", lambda: b.endswith(b"a", None, None))
trap("b.join_tuple", lambda: b",".join((b"a", b"b")))
trap("b.join_bytearray", lambda: b",".join([b"a", bytearray(b"b")]))
"#,
    );
}

#[test]
fn rc2_mapping_get_keywords_match_cpython() {
    // `os.environ` is a `Mapping` (`os._Environ`), so `get` / `pop` take
    // `default=`; the VM's plain dict refused it. The dict subclasses
    // (`Counter`, `OrderedDict`, `defaultdict`) inherit the C `dict.get`,
    // which refuses it — the VM's Python shims accepted it.
    assert_matches_cpython(
        "rc2_mapping_get_keywords_match_cpython",
        r#"import os
from collections import Counter, OrderedDict, defaultdict, ChainMap, UserDict
env = os.environ
env["TYC_PROBE_A"] = "1"
trap("get kw", lambda: env.get("TYC_PROBE_NOPE", default="d"))
trap("get kw hit", lambda: env.get("TYC_PROBE_A", default="d"))
trap("get pos", lambda: env.get("TYC_PROBE_NOPE", "p"))
trap("get1", lambda: env.get("TYC_PROBE_NOPE"))
trap("get key=", lambda: env.get(key="TYC_PROBE_A"))
trap("get int", lambda: env.get(1))
trap("pop kw", lambda: env.pop("TYC_PROBE_NOPE", default="d"))
trap("pop miss", lambda: env.pop("TYC_PROBE_NOPE"))
trap("setdefault", lambda: env.setdefault("TYC_PROBE_B", "2"))
trap("in", lambda: "TYC_PROBE_A" in env)
trap("in int", lambda: 1 in env)
trap("getitem miss", lambda: env["TYC_PROBE_NOPE"])
trap("setitem int", lambda: env.__setitem__("TYC_PROBE_C", 3))
trap("type", lambda: type(env).__name__)
trap("isinstance dict", lambda: isinstance(env, dict))
trap("copy type", lambda: type(env.copy()).__name__)
trap("copy eq", lambda: (env.copy() == env, env == env.copy(), env != {}))
trap("dict()", lambda: dict(env)["TYC_PROBE_A"])
trap("star", lambda: {**env}["TYC_PROBE_A"])
trap("or", lambda: (env | {"X_Y_Z": "1"})["X_Y_Z"])
trap("or types", lambda: (type(env | {}).__name__, type({} | env).__name__))
trap("len", lambda: len(env) == len(dict(env)))
trap("iter", lambda: "TYC_PROBE_A" in list(env))
trap("items", lambda: ("TYC_PROBE_A", "1") in env.items())
trap("format_map", lambda: "{TYC_PROBE_A}".format_map(env))
def kwf(**kw):
    return kw["TYC_PROBE_A"]
trap("call star", lambda: kwf(**env))
trap("dict update", lambda: (lambda d: (d.update(env), d["TYC_PROBE_A"])[1])({}))
del env["TYC_PROBE_A"]
trap("del", lambda: env.get("TYC_PROBE_A"))
trap("del miss", lambda: env.__delitem__("TYC_PROBE_A"))
env.update({"TYC_PROBE_D": "4"}, TYC_PROBE_E="5")
trap("update", lambda: (env["TYC_PROBE_D"], env["TYC_PROBE_E"]))
trap("getenv kw", lambda: os.getenv("TYC_PROBE_NOPE", default="g"))
trap("hash", lambda: hash(env))
trap("repr", lambda: repr(env).startswith("environ({"))
for name, m in [("Counter", Counter("ab")), ("OrderedDict", OrderedDict(a=1)), ("defaultdict", defaultdict(int, {"a": 1}))]:
    trap(name + " get kw", lambda: m.get("z", default=0))
    trap(name + " get key=", lambda: m.get(key="a"))
    trap(name + " get3", lambda: m.get("z", 0, 1))
    trap(name + " get0", lambda: m.get())
    trap(name + " get", lambda: (m.get("a"), m.get("z", 7), m.get("z")))
trap("star counter", lambda: {**Counter("aab")})
trap("update counter", lambda: (lambda d: (d.update(Counter("ab")), d)[1])({}))
for name, m in [("ChainMap", ChainMap({"a": 1})), ("UserDict", UserDict(a=1))]:
    trap(name + " get kw", lambda: m.get("z", default=0))
    trap(name + " get key=", lambda: m.get(key="a"))
"#,
    );
}

#[test]
fn rc2_python315_builtins_match_cpython() {
    // `frozendict` was missing from the arity table, so `fd.get(k, a, b)`
    // and `fd.keys(1)` dropped the surplus argument; `type(s) is sentinel`
    // was False because `sentinel` is a shim class in the VM.
    assert_matches_python315(
        "rc2_python315_builtins_match_cpython",
        r#"fd = frozendict({"a": 1})
show(type(fd) is frozendict, type(fd) == frozendict, fd.__class__ is frozendict, type(fd).__name__)
trap("fd.get3", lambda: fd.get("z", 0, 1))
trap("fd.get0", lambda: fd.get())
trap("fd.getkw", lambda: fd.get("z", default=0))
trap("fd.keys1", lambda: fd.keys(1))
trap("fd.items1", lambda: fd.items(1))
trap("fd.values1", lambda: fd.values(1))
trap("fd.copy1", lambda: fd.copy(1))
trap("fd.ok", lambda: (fd.get("a"), fd.get("z", 5), list(fd.keys()), fd.copy()))
trap("fd.fromkeys0", lambda: fd.fromkeys())
M = sentinel("M")
show(type(M) is sentinel, type(M) == sentinel, isinstance(M, sentinel), M.__class__ is sentinel, type(M).__name__)
show(repr(sentinel), repr(frozendict), type(sentinel) is type, type(frozendict) is type)
show(id(type(fd)) == id(frozendict), {frozendict: 1}.get(type(fd)), {sentinel: 2}.get(type(M)))
"#,
    );
}

#[test]
fn rc2_freeze_let_frozendict_checks_arity() {
    // Under `[emit] freeze-dict = "frozendict"` a `freeze let` dict is a
    // `frozendict`, whose read-only methods were not arity-checked (the
    // default `mappingproxy` lowering was).
    let src = r#"from collections.abc import Callable
freeze let CFG: dict[str, int] = {"a": 1}
def err(thunk: Callable[[], object]) -> str:
    try:
        thunk()
    except TypeError as e:
        return str(e)
    return "no error"
assert err(lambda: CFG.get("a", 1, 2)) == "get expected at most 2 arguments, got 3"
assert err(lambda: CFG.get("a", default=0)) == TYPE + ".get() takes no keyword arguments"
assert err(lambda: CFG.keys(1)) == TYPE + ".keys() takes no arguments (1 given)"
assert err(lambda: CFG.copy(1)) == TYPE + ".copy() takes no arguments (1 given)"
assert CFG.get("a") == 1 and CFG.get("z", 2) == 2 and type(CFG).__name__ == TYPE
"#;
    for (frozendict, ty) in [(true, "frozendict"), (false, "mappingproxy")] {
        let program = format!("TYPE = {ty:?}\n{src}");
        let code = on_worker(|| {
            run_source_reporting(
                &program,
                None,
                &[],
                crate::VmOptions {
                    freeze_to_frozendict: frozendict,
                },
                &mut |tb| panic!("{ty}: {tb}"),
            )
        });
        assert_eq!(code.unwrap(), 0, "{ty}");
    }
}
