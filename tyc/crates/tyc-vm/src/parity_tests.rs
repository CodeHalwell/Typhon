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
