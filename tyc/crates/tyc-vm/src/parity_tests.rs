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

// ── Flag / IntFlag composites ─────────────────────────────────────────────

#[test]
fn flag_and_int_flag_composites_match_cpython() {
    assert_matches_cpython(
        "flag_and_int_flag_composites_match_cpython",
        r#"from enum import Flag, IntFlag, auto
class P(IntFlag):
    R = 4
    W = 2
    X = 1
    RW = 6
class C(Flag):
    A = auto()
    B = auto()
    AB = 3
    D = auto()
show(repr(P.R | P.W), P.R | P.W, repr(P(7)), repr(P(6)), P(6) is P.RW, repr(P(15)), repr(P(8)), P(8).name)
show(list(P), list(P(7)), len(P(7)), len(P), repr(P.R | 1), repr(1 | P.R), repr(P.R & 2), P.R + 0, repr(~P.R), repr(P(-1)))
show(repr(C(7)), list(C), len(C), list(C(7)), repr(~C.A), repr(C.A ^ C.AB), C(3) is C.AB, (C.A | C.D) is (C.D | C.A))
show(bool(C(0)), repr(C(0)), str(C(0)), str(P(0)), repr(~C(0)), C(0) in C.A, C.AB in C(7), list(C(0)), C["AB"])
show(P(7) == 7, hash(P.R) == hash(4), {P.R: 1}[4], f"{C.A}", format(P.R))
show(len(P(8)), len(P(9)), len(P.RW), len(C.AB), len(C(0)))
class Q(IntFlag):
    A = 1
    B = 2
show(repr(Q(-8)), repr(Q(-1)), repr(Q(-3)), repr(Q.A | -8), repr(Q.A ^ -1), repr(-8 | Q.A), repr(~Q(8)), repr(C(-1)), repr(C(-8)))
trap("strict negative", lambda: C(-17))
class N(Flag):
    A = 3
    B = 4
class Other(IntFlag):
    Y = 2
show(list(N.A), list(N(7)), list(N), len(N.A), repr(Other.Y & Q.A), type(Q.A | Other.Y).__name__, int(Q.A ^ Other.Y))
trap("foreign in", lambda: Q.A in Other.Y)
trap("int in", lambda: 1 in Other.Y)
class PA(IntFlag):
    A = 1
    AB = 3
    C = 4
class FA(Flag):
    A = 1
    AB = 3
    C = 4
show(PA(15).name, repr(PA(15)), PA(7).name, PA(11).name, repr(PA(8)), FA(7).name, FA(5).name)
show(repr(Q.B | True), repr(Q.B & True), repr(Q.B ^ True), repr(True | Q.B))
trap("strict", lambda: C(8))
class Neg(Flag):
    A = 1
    B = -3
show(len(Neg.B), Neg.B.value)
trap("negative iter", lambda: list(Neg.B))
class NegAuto(Flag):
    A = -1
    B = auto()
class NegAutoI(IntFlag):
    A = -3
    B = auto()
class MixAuto(Flag):
    A = -1
    B = 4
    C = auto()
show(NegAuto.B.value, NegAutoI.B.value, MixAuto.C.value)
class BoolAuto(Flag):
    A = True
    B = auto()
show(BoolAuto.B.value)
class NegMember(Flag):
    A = 1
    B = -3
class NegMemberI(IntFlag):
    A = 1
    B = -3
show(NegMember(-3) is NegMember.B, NegMemberI(-3) is NegMemberI.B)
class Big(IntFlag):
    LOW = 1
    BIG = 1 << 70
    NEXT = auto()
class BigF(Flag):
    A = 1
    B = 1 << 64
show(repr(Big.LOW | Big.BIG), Big.NEXT.value, list(Big.LOW | Big.BIG | Big.NEXT), len(Big.BIG | Big.LOW))
show(repr(~Big.LOW), repr(Big(1 << 80)), repr(Big.BIG | (1 << 90)), Big.BIG in (Big.BIG | Big.LOW), bool(Big.BIG & Big.LOW))
show(repr(BigF(BigF.A.value | BigF.B.value)), repr(~BigF.A), list(BigF), repr(BigF.A ^ BigF.B))
trap("big strict", lambda: BigF(1 << 65))
show(list(Neg), list(Neg.A))
"#,
    );
}

// ── `del` targets and scopes ──────────────────────────────────────────────

#[test]
fn del_targets_and_scopes_match_cpython() {
    assert_matches_cpython(
        "del_targets_and_scopes_match_cpython",
        r#"plain class Bag:
    def __delitem__(self, k: object) -> None:
        show("delitem", k)
mut bag = Bag()
del bag[1:2]
del bag[::2]
del bag[3]
mut ba = bytearray(b"abcd")
del ba[1:3]
show(ba)
plain class Key:
    def __index__(self) -> int:
        return 1
    def __hash__(self) -> int:
        return 99
mut xs = [1, 2, 3]
del xs[Key()]
show(xs)
mut dk = {1: "one"}
show("absent", Key() in dk)
dk[Key()] = "key"
show(len(dk), dk[1])
plain class Holder:
    x = 1
mut proxy = Holder.__dict__
def drop() -> None:
    del proxy["x"]
trap("proxy del", drop)
show(hasattr(Holder, "x"))
g = 1
def drop_global() -> None:
    global g
    del g
def drop_twice() -> None:
    c = 3
    del c
    del c
def outer() -> object:
    n = 1
    def inner() -> None:
        nonlocal n
        del n
    inner()
    return "n" in dir()
mut xs = [1, 2, 3, 4]
mut d = {"k": 1, "j": 2}
a, b = 1, 2
del (a, b), [xs[0], d["k"]]
show("a" in dir(), "b" in dir(), xs, d)
drop_global()
show("g" in dir())
trap("again", drop_global)
trap("twice", drop_twice)
trap("nonlocal", outer)
def class_del() -> None:
    plain class K:
        del missing
trap("class body", class_del)
plain class Kept:
    a = 1
    b = 2
    c = 3
    del a
    del (b, c)
show(hasattr(Kept, "a"), hasattr(Kept, "b"), hasattr(Kept, "c"))
mut gx = 1
mut gy = 1
plain class Decl:
    global gx, gy
    del gx
    gy = 5
trap("gx gone", lambda: gx)
show(gy, hasattr(Decl, "gy"))
plain class DeclDef:
    global gx
    def gx() -> int:
        return 3
show(gx(), hasattr(DeclDef, "gx"))
plain class Gone:
    def f(self) -> int:
        return 1
    del f
    @property
    def p(self) -> int:
        return 1
    del p
    @classmethod
    def k(cls) -> int:
        return 1
    del k
show(hasattr(Gone, "f"), hasattr(Gone, "p"), hasattr(Gone, "k"), hasattr(Gone(), "p"))
mut shadow = 99
def del_then_read() -> None:
    mut shadow = 1
    del shadow
    show(shadow)
trap("deleted local", del_then_read)
def del_then_rebind() -> None:
    mut shadow = 1
    del shadow
    shadow = 5
    show("rebound", shadow)
del_then_rebind()
def nested() -> object:
    x = 1
    def mid() -> object:
        x = 2
        def inner() -> None:
            nonlocal x
            del x
            del x
        trap("second del", inner)
    mid()
    return x
trap("outer kept", nested)
"#,
    );
}

// ── bytes.translate / bytes.maketrans ─────────────────────────────────────

#[test]
fn bytes_translate_matches_cpython() {
    assert_matches_cpython(
        "bytes_translate_matches_cpython",
        r#"t = bytes.maketrans(b"ab", b"xy")
show(len(t), t[97], b"aabbc".translate(t), b"abc".translate(None, b"b"), b"abcab".translate(t, b"c"))
show(b"abc".translate(None), b"abc".translate(t, delete=b"a"))
trap("short", lambda: b"x".translate(b"short"))
trap("uneven", lambda: bytes.maketrans(b"ab", b"x"))
trap("extra", lambda: b"x".translate(None, b"", b"extra"))
trap("int table", lambda: bytes.maketrans(97, 98))
show(bytes.maketrans(bytearray(b"a"), bytearray(b"b"))[97], b"ab".translate(None, bytearray(b"b")))
trap("int delete", lambda: b"x".translate(None, 120))
trap("int translate", lambda: b"x".translate(5))
show(b"a".maketrans(b"a", b"b")[97], "a".maketrans("a", "b"), {}.fromkeys("ab", 0), hasattr(b"", "maketrans"))
"#,
    );
}

// ── `async for` over a hand-written async iterator ────────────────────────

#[test]
fn async_for_steps_a_user_async_iterator_lazily() {
    assert_matches_cpython(
        "async_for_steps_a_user_async_iterator_lazily",
        r#"import asyncio
plain class Ticker:
    def __init__(self, n: int) -> None:
        self.i = 0
        self.n = n
    def __aiter__(self) -> "Ticker":
        return self
    async def __anext__(self) -> int:
        if self.i >= self.n:
            raise StopAsyncIteration
        self.i += 1
        show("step", self.i)
        return self.i
plain class Forever:
    def __aiter__(self) -> "Forever":
        return self
    async def __anext__(self) -> int:
        return 1
async def main() -> None:
    async for v in Ticker(3):
        show("body", v)
    total = 0
    async for w in Forever():
        total += w
        if total >= 5:
            break
    show("total", total)
asyncio.run(main())
def sync_for() -> None:
    for x in Ticker(1):
        show(x)
trap("sync for", sync_for)
trap("sync list", lambda: list(Ticker(1)))
plain class ListAiter:
    def __aiter__(self) -> object:
        return [1, 2]
plain class Both:
    def __init__(self) -> None:
        self.done = False
    def __iter__(self) -> object:
        return iter(["sync"])
    def __aiter__(self) -> "Both":
        return self
    async def __anext__(self) -> str:
        if self.done:
            raise StopAsyncIteration
        self.done = True
        return "async"
async def odd() -> None:
    async for b in Both():
        show("both", b)
    async for x in ListAiter():
        show("list", x)
trap("aiter list", lambda: asyncio.run(odd()))
async def over(it: object) -> None:
    async for x in it:
        show(x)
for it in ([1], (1,), "a", b"a", {1: 2}, {1}, range(1), frozenset({1})):
    trap("sync iterable", lambda: asyncio.run(over(it)))
plain class SyncOnly:
    def __iter__(self) -> object:
        return iter([1])
trap("sync instance", lambda: asyncio.run(over(SyncOnly())))
trap("sync iterator", lambda: asyncio.run(over(iter([1]))))
def sync_gen() -> object:
    yield 1
trap("sync generator", lambda: asyncio.run(over(sync_gen())))
async def agen() -> object:
    yield 2
asyncio.run(over(agen()))
plain class CoroAiter:
    async def __aiter__(self) -> "CoroAiter":
        return self
    async def __anext__(self) -> int:
        raise StopAsyncIteration
trap("coroutine aiter", lambda: asyncio.run(over(CoroAiter())))
plain class GenAiter:
    async def __aiter__(self) -> object:
        yield 7
asyncio.run(over(GenAiter()))
plain class SyncAnext:
    def __aiter__(self) -> "SyncAnext":
        return self
    def __anext__(self) -> int:
        return 1
trap("sync anext", lambda: asyncio.run(over(SyncAnext())))
plain class Token:
    pass
plain class TokenAnext:
    def __aiter__(self) -> "TokenAnext":
        return self
    def __anext__(self) -> Token:
        return Token()
trap("instance anext", lambda: asyncio.run(over(TokenAnext())))
plain class TypeAnext:
    def __aiter__(self) -> "TypeAnext":
        return self
    def __anext__(self) -> object:
        return int
trap("type anext", lambda: asyncio.run(over(TypeAnext())))
plain class SleepAnext:
    def __init__(self) -> None:
        self.n = 0
    def __aiter__(self) -> "SleepAnext":
        return self
    def __anext__(self) -> object:
        self.n += 1
        if self.n > 2:
            raise StopAsyncIteration
        return asyncio.sleep(0, self.n)
asyncio.run(over(SleepAnext()))
d = {1: 2}
for view in (d.keys(), d.values(), d.items()):
    trap("dict view", lambda: asyncio.run(over(view)))
plain class AgenAnext:
    def __aiter__(self) -> "AgenAnext":
        return self
    async def __anext__(self) -> object:
        yield 1
trap("agen anext", lambda: asyncio.run(over(AgenAnext())))
async def source() -> object:
    yield 1
    yield 2
async def genexp() -> None:
    mut values = (x async for x in source())
    show(type(values).__name__)
    async for x in values:
        show("gx", x)
    mut plain = (x for x in [1])
    trap("sync genexp", lambda: asyncio.run(over(plain)))
asyncio.run(genexp())
async def fetch(x: int) -> int:
    return x * 10
async def await_genexp() -> None:
    mut g = (await fetch(x) for x in [1, 2])
    show(type(g).__name__)
    async for v in g:
        show("v", v)
    mut h = (x for x in [1] if await fetch(x))
    show(type(h).__name__)
asyncio.run(await_genexp())
async def coro_list() -> list[int]:
    return [1, 2]
trap("coroutine iterable", lambda: asyncio.run(over(coro_list())))
async def lambda_default_genexp() -> None:
    mut g = ((lambda x=await fetch(3): x)() for _ in [1])
    show(type(g).__name__)
    async for v in g:
        show("lam", v)
asyncio.run(lambda_default_genexp())
plain class GenexpAiter:
    def __aiter__(self) -> object:
        return (x async for x in source())
async def genexp_aiter() -> None:
    async for x in GenexpAiter():
        show("ai", x)
    mut nested = ((await fetch(z) for z in [1]) for _ in [1])
    show(type(nested).__name__)
asyncio.run(genexp_aiter())
plain class AwaitableIter:
    def __init__(self) -> None:
        self.n = 0
    def __aiter__(self) -> "AwaitableIter":
        return self
    def __await__(self) -> object:
        show("await called")
        return iter([])
    async def __anext__(self) -> int:
        self.n += 1
        if self.n > 2:
            raise StopAsyncIteration
        return self.n
async def both_loop() -> None:
    async for x in AwaitableIter():
        show("b", x)
asyncio.run(both_loop())
async def await_agen() -> None:
    await source()
trap("await agen", lambda: asyncio.run(await_agen()))
plain class StopWith:
    def __iter__(self) -> "StopWith":
        return self
    def __next__(self) -> int:
        raise StopIteration(42)
plain class Aw:
    def __await__(self) -> StopWith:
        return StopWith()
plain class AwAnext:
    def __init__(self) -> None:
        self.n = 0
    def __aiter__(self) -> "AwAnext":
        return self
    def __anext__(self) -> Aw:
        self.n += 1
        if self.n > 1:
            raise StopAsyncIteration
        return Aw()
async def await_result() -> None:
    async for x in AwAnext():
        show("aw item", x)
    show("aw await", await Aw())
asyncio.run(await_result())
async def comp() -> None:
    show("comp", [x async for x in Both()])
asyncio.run(comp())
async def comp_list() -> None:
    xs = [1]
    show([x async for x in xs])
trap("sync comp", lambda: asyncio.run(comp_list()))
class Done(StopAsyncIteration):
    pass
class DoneWith(StopIteration):
    pass
plain class StopSub:
    def __iter__(self) -> "StopSub":
        return self
    def __next__(self) -> int:
        raise DoneWith(7)
plain class AwSub:
    def __await__(self) -> StopSub:
        return StopSub()
plain class AwWrapped:
    def __await__(self) -> object:
        return iter(StopWith())
plain class SubEnd:
    def __aiter__(self) -> "SubEnd":
        return self
    async def __anext__(self) -> int:
        raise Done()
async def sub_end() -> None:
    async for x in SubEnd():
        show("never", x)
    show("sub end", await AwSub(), await AwWrapped())
asyncio.run(sub_end())
from enum import Enum
class Colour(Enum):
    RED = 1
async def over_class(c: object) -> None:
    async for x in c:
        show(x)
trap("enum class", lambda: asyncio.run(over_class(Colour)))
trap("plain class", lambda: asyncio.run(over_class(SubEnd)))
from typing import Iterator
def sync_gen() -> Iterator[int]:
    yield 1
plain class SyncGenAiter:
    def __aiter__(self) -> object:
        return sync_gen()
trap("sync gen aiter", lambda: asyncio.run(over_class(SyncGenAiter())))
"#,
    );
}

// ── `__mro__` / `__bases__` of builtin types and exceptions ──────────────

#[test]
fn builtin_mro_and_bases_match_cpython() {
    assert_matches_cpython(
        "builtin_mro_and_bases_match_cpython",
        r#"names = lambda cs: [c.__name__ for c in cs]
class AppError(ValueError):
    pass
class Computed(type(ValueError())):
    pass
show(names(Computed.__mro__), issubclass(Computed, Exception))
def raise_computed() -> None:
    try:
        raise Computed("boom")
    except ValueError as e:
        show("caught", type(e).__name__, e)
raise_computed()
Alias = ValueError
ValueError = 3
class Aliased(Alias):
    pass
show(names(Aliased.__mro__), names(Aliased.__bases__), issubclass(Aliased, Exception))
import asyncio
show(names(asyncio.CancelledError.__mro__), names(asyncio.CancelledError.__bases__))
ValueError = Alias
class Deeper(AppError):
    pass
plain class Base:
    pass
plain class Child(Base):
    pass
plain class Mixin:
    pass
plain class Both(Child, Mixin):
    pass
plain class PlainErr(ValueError):
    pass
plain class Sub(PlainErr):
    pass
show(names(ValueError.__mro__), names(KeyError.__mro__), names(FileNotFoundError.__mro__), names(KeyboardInterrupt.__mro__))
show(names(AppError.__mro__), names(Deeper.__mro__), names(AppError.__bases__), names(Deeper.__bases__))
show(names(type(ValueError("x")).__mro__), names(type(KeyError("k")).__bases__), names(Exception.__bases__))
show(names(bool.__mro__), names(int.__mro__), names(bool.__bases__), names(str.__bases__))
show(names(Child.__mro__), names(Both.__mro__), names(Both.__bases__), names(Base.__bases__))
show(names(type(True).__mro__), names(ExceptionGroup.__mro__), names(UnicodeDecodeError.__mro__))
show(names(Sub.__bases__), names(Sub.__mro__), names(PlainErr.__bases__), names(ExceptionGroup.__bases__))
plain class ErrFirst(ValueError, Mixin):
    pass
plain class MixFirst(Mixin, KeyError):
    pass
plain class Join(ErrFirst, KeyError):
    pass
show(names(ErrFirst.__mro__), names(ErrFirst.__bases__), names(MixFirst.__mro__), names(Join.__mro__), names(Join.__bases__))
plain class ML(list):
    pass
plain class MixList(Mixin, dict):
    pass
show(names(ML.__bases__), names(ML.__mro__), names(MixList.__mro__), names(MixList.__bases__))
plain class Meta(type):
    pass
show(names(Meta.__bases__), names(Meta.__mro__))
plain class WithObject(Mixin, object):
    pass
plain class OnlyObject(object):
    pass
show(names(WithObject.__bases__), names(WithObject.__mro__), names(OnlyObject.__bases__), names(OnlyObject.__mro__))
list = 7
show(ML.__bases__[0] == list, names(ML.__mro__))
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

// ── A `yield` in any expression position makes a generator ───────────────

#[test]
fn yield_in_a_dict_display_makes_a_generator() {
    assert_matches_cpython(
        "yield_in_a_dict_display_makes_a_generator",
        r#"from typing import Generator
def g() -> Generator[int, object, None]:
    show({1: (yield 1)})
    show({"a": (yield 2)})
it = g()
show(next(it))
show(it.send(5))
trap("end", lambda: it.send(6))
"#,
    );
}

// ── Exception str/repr over user args; ExceptionGroup split/subgroup/derive ──

#[test]
fn exception_str_and_repr_follow_their_args() {
    assert_matches_cpython(
        "exception_str_and_repr_follow_their_args",
        r#"class E(Exception):
    pass
show(str(E([1, 2])), repr(E([1, 2])))
show(str(KeyError("k")), repr(KeyError("k")))
show(str(ValueError(1, 2)), repr(ValueError(1, 2)), repr(ValueError()))
show(str(E()), str(E(3)), repr([E("x"), TypeError(None)]))
class X:
    def __str__(self) -> str:
        return "sx"
    def __repr__(self) -> str:
        return "rx"
class K(KeyError):
    pass
show(str(E(X())), repr(E(X())), str(E(X(), 1)), repr([E(X())]), str(K(X())))
"#,
    );
}

#[test]
fn exception_group_split_subgroup_and_derive() {
    assert_matches_cpython(
        "exception_group_split_subgroup_and_derive",
        r#"def pred(e: BaseException) -> bool:
    return isinstance(e, TypeError)
eg = ExceptionGroup("g", [ValueError("a"), TypeError("b"), ExceptionGroup("inner", [ValueError("c"), KeyError("k")])])
show(repr(eg.split(ValueError)))
show(repr(eg.subgroup(TypeError)), eg.subgroup(OSError))
show(str(eg.derive([KeyError("z")])), repr(eg.derive([KeyError("z")])))
show(repr(eg.split(pred)))
show(repr(eg.split((TypeError, KeyError))))
show(eg.subgroup(lambda e: True) is eg)
trap("bad", lambda: eg.split(3))
class C:
    pass
trap("plain class", lambda: eg.split(C))
trap("empty derive", lambda: eg.derive([]))
show(repr(eg.split(())), eg.subgroup(()))
class X:
    def __repr__(self) -> str:
        return "XX"
class P:
    def __call__(self, e: BaseException) -> bool:
        return isinstance(e, ValueError)
show(repr(ExceptionGroup("h", [ValueError(X())])))
show(repr(eg.split(P())))
show(repr(eg.derive([KeyboardInterrupt()])))
trap("non-exception derive", lambda: eg.derive([1]))
trap("builtin type", lambda: eg.split(bool))
try:
    try:
        raise KeyError(1)
    except KeyError as k:
        raise ExceptionGroup("c", [ValueError("a"), TypeError("b")]) from k
except ExceptionGroup as caught:
    m, r = caught.split(ValueError)
    show(repr(m.__cause__), repr(r.__context__), m.__suppress_context__)
"#,
    );
}

// ── str case predicates: titlecase letters, uncased letters, final sigma ────

#[test]
fn str_case_follows_cased_titlecase_and_final_sigma() {
    assert_matches_cpython(
        "str_case_follows_cased_titlecase_and_final_sigma",
        r#"for t in ["ǅungla", "中A", "中a", "Aǅ", "ᾈ", "ǈa Bc", "A中b", "ºA", "ⓐⓑ", "Ⓐⓑ", "ǆemal", "ß x", "they're bill's"]:
    show(t, t.istitle(), t.title(), t.islower(), t.isupper(), t.capitalize(), t.swapcase())
for t in ["ΑΣ ΣΑΣ", "ΑΣ", "ΑΣ1", "όΣ", "ΣΑΣ. ΟΔΟΣ"]:
    show(t, t.title(), t.capitalize(), t.swapcase(), t.lower())
"#,
    );
}

#[test]
fn qualname_follows_lexical_nesting() {
    assert_matches_cpython(
        "qualname_follows_lexical_nesting",
        r#"from dataclasses import dataclass
def outer():
    def inc(x):
        return x + 1
    @dataclass
    class P:
        x: int
    class L:
        def m(self):
            def h():
                pass
            return h
    show(inc.__qualname__, inc.__name__, (lambda: 0).__qualname__)
    show(P.__qualname__, P.__name__, repr(P(1)), repr(P))
    show(L.m.__qualname__, L().m.__qualname__, L().m().__qualname__)
    return inc
class Top:
    def meth(self):
        def h():
            pass
        return h
    @staticmethod
    def s():
        return 0
def gen():
    def g():
        pass
    yield g.__qualname__
    def g2():
        pass
    yield g2.__qualname__
show(outer().__qualname__, Top.meth.__qualname__, Top().meth().__qualname__, Top.s.__qualname__)
show(Top.__qualname__, list(gen()), (lambda: 0).__qualname__)
@dataclass
class R:
    x: int
R.__qualname__ = "Alias"
def f():
    pass
f.__qualname__ = "g.h"
show(R.__qualname__, repr(R(1)), repr(R), f.__qualname__)
def lams():
    a = next(lambda: 0 for _ in [1]).__qualname__
    b = [lambda: 0 for _ in [1]][0].__qualname__
    return a, b
show(lams(), next(lambda: 0 for _ in [1]).__qualname__)
"#,
    );
}

#[test]
fn re_match_reports_char_offsets_and_groups() {
    assert_matches_cpython(
        "re_match_reports_char_offsets_and_groups",
        r#"import re
from enum import IntEnum
class G(IntEnum):
    ONE = 1
m = re.search(r"(b)(x)?", "ééab")
show(m.start(), m.end(), m.span(), m.start(1), m.span(2), m.end(2), m[0], m[1], m[2])
show([q.span() for q in re.finditer(r"\w", "éa")], re.sub("(é)", lambda q: str(q.span()), "aéb"))
m = re.match(r"(?P<k>a)(b)", "ab")
show(m.start("k"), m.end(2), m["k"], m.string, m.pos, m.endpos, m.expand(r"\2\g<k>"), repr(m))
p = re.compile(r"(\w)(\d)?")
m = p.search("éé x1 y", 2)
show(repr(m), m.pos, m.endpos, m.groups("-"))
show([(repr(q), q.span(2)) for q in p.finditer("ab1 é2", 1, 5)])
m = re.fullmatch(r"(?P<a>x)(?P<b>y)?", "x")
show(m.groupdict("z"), m.span("b"), m.groupdict(default="d"), m.groups(default="g"))
m = re.match(r"(a)(b)", "ab")
show(m.expand(r"\0|\012|\101|\1\2|\08|\1x"), m.expand(template=r"\2"), re.sub("(a)", r"\101\0", "xa"))
show(repr(re.match("a*", "a" * 100)))
m = re.match("(a)(?P<n>b)?", "a")
for t in [r"\9", r"\q", r"\g<x>", r"\g<9>", "\\a\\v", r"\-", r"\g<1", "x\\", r"\g<n>|\2", r"\g<-1>", r"\g<>", r"\gx", r"\400", r"x\777"]:
    try:
        show(t, m.expand(t))
    except Exception as e:
        show(t, type(e).__name__, str(e))
for f in [lambda: m.start(0, 1), lambda: m.groups(1, 2), lambda: m.expand("a", "b")]:
    try:
        show(f())
    except TypeError as e:
        show("TypeError", str(e))
for f in [lambda: re.sub("x", r"\q", "a"), lambda: re.subn("x", r"\9", "a")]:
    try:
        show(f())
    except re.error as e:
        show("error", str(e))
show(re.sub("(x)", r"\1", "a"))
m = re.match(r"(a)(b)", "ab")
show(m.group(G.ONE), m.start(G.ONE), m.span(G.ONE), m[G.ONE], m.group(G.ONE, 2))
s = "".join(["he", "llo"])
c = re.compile("l")
show(re.search("l", s).string is s, c.match(s, 2).string is s, re.sub("e", lambda q: str(q.string is s), s))
"#,
    );
}

#[test]
fn str_identity_follows_cpython_objects() {
    assert_matches_cpython(
        "str_identity_follows_cpython_objects",
        r#"import sys
from enum import Enum
class Color(Enum):
    RED = 1
class Box:
    pass
def ident(x):
    return x
def kw(**k):
    return list(k)
c = "".join(["hello", " world"])
w = "".join(["he", "llo"])
lit = "hello world"
name = "hello"
show(lit is "hello world", c is lit, w is name, ident("hello world") is lit, c is c)
show(str(c) is c, c[:] is c, c[0:] is c, c[::1] is c, c[::-1] is c, c[1:] is c)
show((c + "") is c, ("" + c) is c, (c * 1) is c, f"{c}" is c, f"{c!s}" is c, f"{c!r}" is c, f"<{c}>" is c)
show("".join([c]) is c, ", ".join([c]) is c, format(c) is c, format(c, "") is c, ("%s" % c) is c, "{}".format(c) is c)
show(c.strip() is c, c.rstrip("z") is c, c.replace("zz", "y") is c, c.ljust(3) is c, c.zfill(1) is c, c.removeprefix("zz") is c)
show(c.split("zz")[0] is c, w.split()[0] is w, c.partition("zz")[0] is c, c.rpartition("zz")[2] is c, c.splitlines()[0] is c)
show(c.lower() is c, c.title() is c, c.strip("h") is c, c.replace("l", "L") is c)
show(sys.intern(w) is name, sys.intern(c) is c, max([c]) is c, next(iter({c: 1})) is c, next(iter({c})) is c)
show(Box.__name__ is "Box", Box.__name__ is Box.__name__, ident.__name__ is "ident", Color.RED.name is "RED", kw(alpha=1)[0] is "alpha")
show(c[0] is "h", "".join([]) is "", c[0:0] is "", getattr(Box, "__name__") is "Box", repr(c) is repr(c))
c2 = "".join(["hello", " world"])
show("{1}".format(c, c2) is c2, "{0}".format(c, c2) is c, "{k}".format(k=c2) is c2, "{!s}".format(c) is c, "hello world".format(c) is c, "{}{}".format(c, "") is c)
show(("%s%s" % (c, "")) is c, ("%(k)s" % {"k": c}) is c, ("%r" % c) is c, ("%.20s" % c) is c)
show(format(c, "1") is c, format(c, "20") is c, format(c, ".3") is c, "{:1}".format(c) is c, "{0!s:>5}".format(c) is c, "{:20}".format(c) is c, "{:{}}".format(c, 3) is c)
def _named() -> int:
    return 0
_named.__qualname__ = c
show(_named.__qualname__ is c, c.__str__() is c, c.__format__("") is c, c.__format__("5") is c, c.__format__("20") is c)
show("{k}".format_map({"k": c}) is c, "{k:3}".format_map({"k": c}) is c, "{k:30}".format_map({"k": c}) is c)
_he = "he"
show(("ab" * (1 + 1)) is "abab", ("ab" * (2 * 2)) is "abababab", ("ab" * (3 - 1)) is "abab", ("ab" * -1) is "")
def _documented() -> None:
    """the quick brown fox"""
show(_documented.__doc__ is _documented.__doc__, _documented.__doc__ is "the quick brown fox")
show(("he" + "llo") is "hello", ("a b" + " c") is "a b c", ("ab" * 3) is "ababab", (2 * "xy" + "!") is "xyxy!", ("x" * 4096) is ("x" * 4096), ("x" * 4097) is ("x" * 4097), (_he + "llo") is "hello")
show((5).__format__("03"), (2.5).__format__(".1f"), [1].__format__(""))
for _bad in (lambda: c.__format__(1), lambda: (5).__format__(), lambda: c.__format__("a", "b")):
    try:
        _bad()
    except TypeError as e:
        show(str(e))
show(("%(k)1s" % {"k": c}) is c, ("%(k)#s" % {"k": c}) is c, ("%(k).20s" % {"k": c}) is c, ("%(k).3s" % {"k": c}) is c)
show(("%#s" % c) is c, ("%-#5s" % c) is c, ("%+s" % c) is c, ("% s" % c) is c, f"{c:3}" is c, f"{c:20}" is c, f"{c!r}" is c)
show(c.partition(c2)[1] is c2, c.partition(c2)[1] is c, c.rpartition("zz")[0] is c, c.partition("zz")[2] is c)
show(eval("'hello world'") is eval("'hello world'"), eval("'hello world'") is lit, eval("'abc'") is "abc")
try:
    sys.intern(1)
except TypeError as e:
    show(str(e))
"#,
    );
}

#[test]
fn literal_and_qualname_objects_belong_to_their_module() {
    // A non-name literal and a nested `__qualname__` are constants of the
    // module that defines them: shared within it, distinct across modules.
    let util = r#"def make() -> type:
    class C:
        pass
    return C
def inner():
    def f() -> int:
        return 1
    return f
VALUE = "hello world"
NAME = "hello_world"
"#;
    let main = r#"import util
def make() -> type:
    class C:
        pass
    return C
def inner():
    def f() -> int:
        return 1
    return f
VALUE = "hello world"
NAME = "hello_world"
assert util.VALUE is not VALUE
assert util.NAME is NAME
assert util.make().__qualname__ is not make().__qualname__
assert make().__qualname__ is make().__qualname__
assert util.inner().__qualname__ is not inner().__qualname__
assert inner().__qualname__ is inner().__qualname__
assert util.make().__name__ is make().__name__
"#;
    assert_eq!(
        run_project(&[("util.ty", util), ("main.ty", main)], "main.ty").unwrap(),
        0
    );
}

#[test]
fn copy_module_matches_cpython() {
    assert_matches_cpython(
        "copy_module_matches_cpython",
        r#"import copy
from dataclasses import dataclass, field
from enum import Enum
from collections import namedtuple
class Color(Enum):
    RED = 1
@dataclass
class P:
    x: int
    ys: list
@dataclass(frozen=True)
class F:
    a: int
    b: list = field(default_factory=list)
class Plain:
    n: int
    items: list
    def __init__(self, n):
        self.n = n
        self.items = [n]
class Custom:
    log: list
    def __init__(self, log):
        self.log = log
    def __copy__(self):
        return Custom(["copied"])
    def __deepcopy__(self, memo):
        return Custom(["deep", isinstance(memo, dict)])
class Stateful:
    a: int
    def __init__(self, a):
        self.a = a
    def __getstate__(self):
        return {"a": self.a * 10}
    def __setstate__(self, st):
        self.a = st["a"] + 1
Pt = namedtuple("Pt", "x y")
a = [[1, 2], {"k": [3]}, (4, [5]), {6}, frozenset({7}), "s", 8, None]
s = copy.copy(a)
d = copy.deepcopy(a)
show(s == a, s is a, s[0] is a[0], d == a, d[0] is a[0], d[1]["k"] is a[1]["k"], d[2] is a[2], d[2][1] is a[2][1])
t = (1, "x", (2, 3))
show(copy.copy(t) is t, copy.deepcopy(t) is t, copy.deepcopy((1, [2]))[1] is (1, [2])[1])
r = []
r.append(r)
rc = copy.deepcopy(r)
show(rc[0] is rc, rc is not r)
shared = [1]
pair = [shared, shared]
pc = copy.deepcopy(pair)
show(pc[0] is pc[1], pc[0] is shared)
p = P(1, [2])
ps, pd = copy.copy(p), copy.deepcopy(p)
show(ps, ps is p, ps.ys is p.ys, pd.ys is p.ys, pd == p)
f = F(1, [2])
fs, fd = copy.copy(f), copy.deepcopy(f)
show(fs, fs.b is f.b, fd.b is f.b, fd == f)
pl = Plain(5)
pls, pld = copy.copy(pl), copy.deepcopy(pl)
show(type(pls).__name__, pls.n, pls.items is pl.items, pld.items is pl.items, pld.items, vars(pld))
show(copy.copy(Custom([])).log, copy.deepcopy(Custom([])).log)
st = Stateful(1)
show(copy.copy(st).a, copy.deepcopy(st).a)
show(copy.copy(Color.RED) is Color.RED, copy.deepcopy(Color.RED) is Color.RED, copy.deepcopy(len) is len, copy.copy(P) is P)
show(copy.replace(p, x=9), copy.replace(f, a=7), copy.replace(Pt(1, 2), y=5))
try:
    copy.replace([1], x=1)
except TypeError as e:
    show("TypeError", str(e))
show(copy.Error is copy.error, issubclass(copy.Error, Exception))
m = {}
show(copy.deepcopy([1, [2]], m) == [1, [2]], len(m) > 0)
def _fn() -> int:
    return 1
class _Box:
    v: int
    def __init__(self) -> None:
        self.v = 1
    def meth(self) -> int:
        return self.v
_b = _Box()
show(copy.copy(_fn) is _fn, copy.deepcopy(_fn) is _fn, copy.copy(len) is len, copy.deepcopy([_fn])[0] is _fn)
show(copy.copy(_b.meth)() == 1)
calls = []
class _NoState:
    v: int
    def __init__(self) -> None:
        self.v = 1
    def __getstate__(self):
        return None
    def __setstate__(self, st) -> None:
        calls.append(st)
copy.copy(_NoState())
copy.deepcopy(_NoState())
show(calls)
class _EmptyState:
    v: int
    def __init__(self) -> None:
        self.v = 1
    def __getstate__(self):
        return {}
    def __setstate__(self, st) -> None:
        calls.append(st)
copy.copy(_EmptyState())
copy.deepcopy(_EmptyState())
show(calls)
"#,
    );
}

#[test]
fn builtin_type_objects_are_the_builtins() {
    assert_matches_cpython(
        "builtin_type_objects_are_the_builtins",
        r#"class P:
    x: int
o0 = object()
xs = [1]
show(type(xs) is list, type({}) is dict, type({1}) is set, type((1,)) is tuple, type("s") is str, type(1) is int, type(True) is bool, type(1.5) is float)
show(type(xs) is not list, type(xs) is tuple, type(xs) in (list, dict))
show(type(xs)([1, 2]), type({1})([3, 3]), type("")(5), type(0)("7"), type(())(xs), type({})(a=1))
show(isinstance(P, type), isinstance(int, type), isinstance(ValueError, type), isinstance(type(xs), type), isinstance(len, type), isinstance(xs, type), isinstance(o0, type))
for _t in (list, ValueError, KeyError):
    try:
        object.__new__(_t)
    except TypeError as e:
        show(str(e))
o = object.__new__(P)
show(type(o) is P, isinstance(o, P))
"#,
    );
}

#[test]
fn a_user_class_named_type_is_not_the_metaclass() {
    assert_matches_cpython(
        "a_user_class_named_type_is_not_the_metaclass",
        r#"class P:
    x: int
class type:
    v: int
    def __init__(self) -> None:
        self.v = 1
show(isinstance(P, type), isinstance(int, type), isinstance(type(), type))
"#,
    );
}
