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
    probe
        .replace("plain class ", "class ")
        .replace("mut ", "")
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
    let dir = tempfile::tempdir().unwrap();
    let dump = dir.path().join("transcript.txt");
    let tail = format!(
        "\nwith open({:?}, \"w\", encoding=\"utf-8\") as _out:\n    _out.write(\"\\n\".join(out))\n",
        dump.display().to_string()
    );
    let body = format!("{PRELUDE}{probe}{tail}");
    let Some((_, py_err, code)) = run_python(dir.path(), &to_python(&body)) else {
        return python_missing(test);
    };
    assert_eq!(code, 0, "{test}: the probe fails under python3.13:\n{py_err}");
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
