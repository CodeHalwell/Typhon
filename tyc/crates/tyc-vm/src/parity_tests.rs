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

fn python_missing(test: &str) {
    if std::env::var_os("TYC_REQUIRE_PYTHON").is_some() {
        panic!("python3.13 is required as the oracle (TYC_REQUIRE_PYTHON is set)");
    }
    eprintln!("skipping {test}: no python3.13 on PATH");
}

/// Run `py` under python3.13 in `dir`; `(stdout, stderr, exit code)`.
fn run_python(dir: &Path, py: &str) -> Option<(String, String, i32)> {
    let script = dir.join("probe.py");
    std::fs::write(&script, py).ok()?;
    let out = std::process::Command::new("python3.13")
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
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("transcript.txt");
    let tail = format!(
        "\nwith open({:?}, \"w\", encoding=\"utf-8\") as _out:\n    _out.write(\"\\n\".join(out))\n",
        dump.display().to_string()
    );
    let body = format!("{PRELUDE}{probe}{tail}");
    let python = format!("{py_header}{}", to_python(&body));
    let Some((_, py_err, code)) = run_python(dir.path(), &python) else {
        return python_missing(test);
    };
    assert_eq!(
        code, 0,
        "{test}: the probe fails under python3.13:\n{py_err}"
    );
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
            "{test}: VM transcript differs from python3.13 ({} vs {} lines)\n{}",
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
        run_source_reporting(probe, Some(&origin), &[], &mut sink)
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
        run_source_reporting(src, Some(Path::new("tb.ty")), &[], &mut sink)
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
