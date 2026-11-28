//! Regression tests for the robustness / security review of the VM:
//! allocation failure as `MemoryError`, deep iterator chains, `tempfile`,
//! `re`, and the security-relevant corners of the filesystem shims.
//!
//! Where CPython's behaviour is deterministic the probe is compared with
//! the hosting `python3.13` (see `parity_tests`). Where CPython would
//! itself run out of memory or stack, the probe asserts on the VM alone.
//! No probe allocates more than a few megabytes: every "too large" size is
//! one no allocator can satisfy (beyond the address space), so the outcome
//! does not depend on how much memory the host has. Probes leave the
//! process-wide working directory and umask alone: the tests of this crate
//! share one process.

use crate::parity_tests::assert_matches_cpython;
use crate::run_source;

/// Run `src` on a worker with `stack` bytes of stack; the program asserts
/// on what it observes, so success is exit code 0.
fn runs_clean(src: &str, stack: usize) {
    let code = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(stack)
            .spawn_scoped(scope, || run_source(src, None, &[]))
            .expect("failed to spawn the VM test worker thread")
            .join()
            .expect("the VM worker panicked")
    });
    assert_eq!(code.expect("the VM reported an error"), 0);
}

const BIG_STACK: usize = 256 * 1024 * 1024;

// ── 1. Allocation failure ────────────────────────────────────────────────

#[test]
fn unallocatable_results_raise_memory_error_like_cpython() {
    // Each of these aborted the process ("memory allocation of N bytes
    // failed", SIGABRT); CPython raises a catchable exception.
    assert_matches_cpython(
        "unallocatable_results_raise_memory_error_like_cpython",
        r#"import random
trap("shl", lambda: (1 << 2**62).bit_length())
trap("shl neg", lambda: ((-1) << 2**62).bit_length())
trap("shl zero", lambda: (0 << 2**62, 0 << 2**70))
trap("shl count", lambda: (1 << 2**70).bit_length())
trap("shl small", lambda: (1 << 100, -3 << 70, 5 << 0))
trap("getrandbits", lambda: random.getrandbits(2**62).bit_length())
trap("getrandbits63", lambda: random.getrandbits(2**63))
trap("getrandbits-1", lambda: random.getrandbits(-1))
trap("randbytes", lambda: len(random.randbytes(2**59)))
trap("list(range)", lambda: len(list(range(2**61))))
trap("tuple(range)", lambda: len(tuple(range(2**61))))
trap("sorted(range)", lambda: len(sorted(range(2**61))))
trap("[*range]", lambda: len([*range(2**61)]))
trap("(*range,)", lambda: len((*range(2**61),)))
trap("bytes(range)", lambda: len(bytes(range(2**61))))
trap("extend(range)", lambda: [].extend(range(2**61)))
trap("pow 1", lambda: 1 ** 10**30)
trap("pow -1", lambda: ((-1) ** (10**30 + 1), (-1) ** 10**30))
trap("pow 0", lambda: (0 ** 10**30, 0 ** 0, 5 ** 0))
trap("pow small", lambda: (2 ** 100, (-3) ** 5, 10 ** 20, 2 ** -1))
trap("mul", lambda: (10**30) * (10**30) == 10**60)
try:
    x = 1 << 2**62
    show("no error")
except MemoryError:
    show("caught")
show(list(range(5)), tuple(range(3)), sorted(range(3, 0, -1)), bytes(range(3)))
show(",".join(["a", "b"]), "a" + "b", b"a" + b"b", [1] + [2], (1,) + (2,))
show(b",".join([b"a", b"b"]), b"-".join((b"x", b"y")), "aXa".replace("X", "YY"), b"aXa".replace(b"X", b"YY"))
show(bin(5), hex(-255), oct(8), bin(1 << 70))
xs = [1, 2]
xs.extend(xs)
show(xs, {i for i in range(4)}, {i: i for i in range(3)}, [i for i in range(3)])
"#,
    );
}

#[test]
fn tables_and_powers_too_large_to_hold_raise_memory_error() {
    // CPython fills memory (or computes for hours) before it fails on
    // these, so there is no oracle to run; the VM answers at once.
    runs_clean(
        r#"import itertools
def is_memory_error(thunk: object) -> bool:
    try:
        thunk()
    except MemoryError:
        return True
    return False
assert is_memory_error(lambda: set(range(2**61)))
assert is_memory_error(lambda: frozenset(range(2**61)))
assert is_memory_error(lambda: dict.fromkeys(range(2**61)))
assert is_memory_error(lambda: 2 ** 10**12)
assert is_memory_error(lambda: (-7) ** 2**40)
assert is_memory_error(lambda: list(reversed(range(2**61))))
assert len(set(range(1000))) == 1000
assert len(dict.fromkeys(range(1000))) == 1000
big: set[int] = set()
for i in range(200000):
    big.add(i)
assert len(big) == 200000
d: dict[int, int] = {}
for i in range(200000):
    d[i] = i
assert len(d) == 200000 and d[199999] == 199999
"#,
        BIG_STACK,
    );
}

#[test]
fn size_checks_reject_what_cannot_be_allocated() {
    use crate::limits;
    let is_memory_error = |r: Result<(), crate::error::Unwind>| matches!(r, Err(crate::error::Unwind::Exception(e)) if e.kind == "MemoryError");
    // An int of 2^70 bits, a replace whose result overflows the index
    // range, a split into more pieces than memory holds.
    assert!(is_memory_error(limits::ensure_int_bits(1 << 70, false)));
    assert!(is_memory_error(limits::ensure_int_bits(u128::MAX, true)));
    assert!(limits::ensure_int_bits(4096, true).is_ok());
    assert!(is_memory_error(limits::ensure_replace_result(
        1 << 40,
        1,
        1 << 30,
        1 << 40
    )));
    assert!(limits::ensure_replace_result(10, 1, 2, 10).is_ok());
    assert!(limits::ensure_replace_result(100, 4, 1, 25).is_ok());
    assert!(is_memory_error(limits::ensure_text_result(
        1 << 30,
        "split",
        || usize::MAX / 2
    )));
    assert!(limits::ensure_text_result(10, "split", || unreachable!("small text")).is_ok());
    assert!(limits::ensure_text_result(1 << 30, "startswith", || 0).is_ok());
    assert_eq!(limits::range_len(0, 10, 3), 4);
    assert_eq!(limits::range_len(10, 0, -3), 4);
    assert_eq!(limits::range_len(5, 5, 1), 0);
    assert_eq!(limits::range_len(0, 1 << 61, 1), 1 << 61);
    assert_eq!(limits::range_len(i64::MIN, i64::MAX, 1), u64::MAX as u128);
    let mut v: Vec<u8> = Vec::new();
    for i in 0..100u8 {
        assert!(limits::try_push(&mut v, i).is_ok());
    }
    assert_eq!(v.len(), 100);
    assert!(limits::try_reserve_items(&mut v, usize::MAX).is_err());
    assert_eq!(
        limits::try_join_str(", ", &[std::rc::Rc::new("a".to_owned())])
            .ok()
            .as_deref(),
        Some("a")
    );
}

#[test]
fn set_and_dict_report_growth_they_cannot_make() {
    let mut s = crate::pyset::PySet::new();
    for i in 0..10_000i64 {
        assert!(s.try_reserve_one());
        s.insert(crate::value::HashKey::Int(crate::value::VmInt::from(i)));
    }
    assert_eq!(s.len(), 10_000);
    let mut d = crate::pydict::PyDict::new();
    for i in 0..10_000i64 {
        assert!(d.try_reserve_one());
        d.insert(
            crate::value::HashKey::Int(crate::value::VmInt::from(i)),
            crate::value::Value::None,
        );
    }
    assert_eq!(d.len(), 10_000);
}

// ── 2. Deep iterator chains ──────────────────────────────────────────────

#[test]
fn deep_iterator_chains_raise_recursion_error_and_release_cleanly() {
    // A chain of adapters built in a loop is as deep as the loop ran:
    // stepping it recursed once per link and overflowed the native stack
    // (SIGABRT), and so did releasing it. The worker's stack is small on
    // purpose, so both would overflow here without the guards.
    runs_clean(
        r#"import itertools
def ident(x: int) -> int:
    return x
def keep(x: int) -> bool:
    return True
def outcome(g: object) -> str:
    try:
        next(g)
        return "value"
    except RecursionError:
        return "RecursionError"
N = 150000
for wrap in [
    lambda g: (x for x in g),
    lambda g: map(ident, g),
    lambda g: filter(keep, g),
    lambda g: zip(g),
    lambda g: enumerate(g),
    lambda g: itertools.chain(g),
]:
    g = iter(range(3))
    for _ in range(N):
        g = wrap(g)
    assert outcome(g) == "RecursionError"
    try:
        list(g)
    except RecursionError:
        pass
    g = None
g = iter(range(3))
for _ in range(N):
    g = (x for x in g)
g = None
# A short chain still works.
g = iter(range(3))
for _ in range(50):
    g = map(ident, (x for x in filter(keep, g)))
assert list(g) == [0, 1, 2]
print("ok")
"#,
        16 * 1024 * 1024,
    );
}

// ── 3. tempfile ──────────────────────────────────────────────────────────

#[test]
fn tempfile_names_modes_and_cleanup_match_cpython() {
    assert_matches_cpython(
        "tempfile_names_modes_and_cleanup_match_cpython",
        r#"import os
import random
import shutil
import tempfile
def mode_of(p: str) -> str:
    return oct(os.stat(p).st_mode & 0o777)
# The program's own seed must not decide temporary names.
random.seed(1234)
a = tempfile.mktemp()
random.seed(1234)
b = tempfile.mktemp()
name = os.path.basename(a)
show("names", a != b, name.startswith("tmp"), len(name), all(c in "abcdefghijklmnopqrstuvwxyz0123456789_" for c in name[3:]))
random.seed(1234)
show("random itself", random.random())
names = {os.path.basename(tempfile.mktemp()) for _ in range(200)}
show("distinct", len(names))
d = tempfile.mkdtemp()
show("mkdtemp", os.path.isabs(d), mode_of(d))
fd, p = tempfile.mkstemp(dir=d, suffix=".txt", prefix="pre")
show("mkstemp", os.path.isabs(p), mode_of(p), os.path.basename(p).startswith("pre"), p.endswith(".txt"), os.path.getsize(p))
with os.fdopen(fd, "w") as f:
    f.write("hello")
show(open(p).read(), mode_of(p))
with tempfile.NamedTemporaryFile(dir=d) as f:
    show("ntf", mode_of(f.name), f.mode, os.path.exists(f.name), type(f).__name__)
    f.write(b"abc")
    f.flush()
    show(open(f.name, "rb").read(), mode_of(f.name))
    f.seek(0)
    show(f.read())
    keep = f.name
show("deleted", not os.path.exists(keep))
with tempfile.NamedTemporaryFile("w+", dir=d, delete=False, suffix=".cfg") as f:
    f.write("text\n")
    keep = f.name
    show(f.mode)
show("kept", os.path.exists(keep), mode_of(keep), open(keep).read())
with tempfile.NamedTemporaryFile("r+", dir=d) as f:
    show(f.mode, repr(f.read()))
    f.write("x")
    f.seek(0)
    show(f.read())
with tempfile.NamedTemporaryFile(dir=d, delete_on_close=False) as f:
    f.write(b"zz")
    f.close()
    show("after close", os.path.exists(f.name), open(f.name, "rb").read())
    keep = f.name
show("after with", os.path.exists(keep))
with tempfile.TemporaryFile(dir=d) as f:
    f.write(b"tf")
    f.seek(0)
    show(f.read(), f.mode)
with tempfile.TemporaryDirectory(dir=d) as td:
    show("td", mode_of(td), os.path.isdir(td))
    open(os.path.join(td, "x"), "w").close()
show("td gone", not os.path.exists(td))
s = tempfile.SpooledTemporaryFile(max_size=10)
s.write(b"12345")
show(s._rolled, s.name, s.mode)
s.write(b"6789012")
show(s._rolled, s.mode, s.tell())
s.seek(0)
show(s.read())
s.close()
show(s.closed)
with tempfile.SpooledTemporaryFile(max_size=100, mode="w+") as s:
    s.write("hi\nthere\n")
    s.seek(0)
    # The *name* CPython reports for the default encoding follows the
    # locale ("utf-8" under C.UTF-8, "UTF-8" on the CI runners): compare
    # the codec, not its spelling.
    show(s.readline(), s._rolled, s.mode, s.encoding.lower())
    show(list(s))
with tempfile.SpooledTemporaryFile(max_size=1, mode="w+", encoding="utf-8") as s:
    # Rolling over after multi-byte text must leave the cursor at the end.
    s.write("\u00e9\u00e9")
    s.write("X")
    s.seek(0)
    show(s._rolled, s.read())
shutil.rmtree(d)
show("cleaned", not os.path.exists(d))
"#,
    );
}

#[cfg(unix)]
#[test]
fn a_private_file_is_written_through_its_identity_not_its_name() {
    use crate::builtins::{create_private, private_file_id, private_write};
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tmpabc");
    let path_str = path.to_str().unwrap().to_owned();
    let victim = dir.path().join("victim");
    std::fs::write(&victim, b"precious").unwrap();

    drop(create_private(&path_str, 0o600).unwrap());
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    // Exclusive: the name is taken now, by a file or by a symlink.
    assert_eq!(
        create_private(&path_str, 0o600).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    let dangling = dir.path().join("dangling");
    std::os::unix::fs::symlink(dir.path().join("nowhere"), &dangling).unwrap();
    assert_eq!(
        create_private(dangling.to_str().unwrap(), 0o600)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert!(!dir.path().join("nowhere").exists());

    let id = private_file_id(&path_str).expect("the file is recorded");
    private_write(&path_str, b"one", false, id).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"one");
    private_write(&path_str, b"+two", true, id).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"one+two");

    // Someone swaps a symlink to another file in under the name: the
    // write must not follow it.
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&victim, &path).unwrap();
    private_write(&path_str, b"overwritten", false, id).unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");

    // …or a different regular file (a hard link to the victim).
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&victim, &path).unwrap();
    private_write(&path_str, b"overwritten", false, id).unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");

    // The name is simply gone: the file comes back owner-only.
    std::fs::remove_file(&path).unwrap();
    private_write(&path_str, b"again", false, id).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"again");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

// ── 4. re ────────────────────────────────────────────────────────────────

#[test]
fn re_dollar_and_fullmatch_follow_cpython() {
    assert_matches_cpython(
        "re_dollar_and_fullmatch_follow_cpython",
        r#"import re
def m(x: object) -> object:
    if x is None:
        return None
    return (x.span(), x.group(0), x.groups())
cases = [
    ("a$", "a\n"), ("a$", "a"), ("a$", "a\nb"), ("a$", "a\nb\n"), ("b$", "a\nb\n"),
    (r"^\d+$", "123\n"), (".*$", "abc\n"), (r"a\Z", "a\n"), (r"a\Z", "a"), (r"a\n\Z", "x\na\n"),
    (r"a\$", "a$\n"), ("a[$]", "a$"), ("x*|xy", "xy"), ("a|ab", "ab"), ("a|ab", "abc"),
    ("(a|ab)(c|bcd)", "abcd"), ("(a|ab)(c|bcd)$", "abcd\n"), (r"\w+$", "foo bar\n"),
    (r"\s*$", "x  \n"), (r"x\s*$", "x  \n"), ("$", "a\n"), ("^$", "\n"), ("^$", ""),
    (r"(?P<k>\w+)=(?P<v>\w+)$", "a=b\n"), (r"[]a]+", "]a"),
    (r"end$", "the end\nnot the end\n"), (r"end$", "the end\nnot the end"),
    (r"foo(?#comment)bar", "foobar"), (r"a|b$", "b\n"), (r"(?:a|b)+$", "abab\n\n"),
]
for p, s in cases:
    for flags in (0, re.MULTILINE, re.DOTALL):
        c = re.compile(p, flags)
        show(p, repr(s), int(flags), m(c.search(s)), m(c.match(s)), m(c.fullmatch(s)), c.findall(s))
    show(p, repr(s), m(re.search(p, s)), m(re.match(p, s)), m(re.fullmatch(p, s)))
show(re.sub("x*", "-", "abxd"), re.split("x*", "abxd"), re.findall("x*", "abxd"))
show([x.span() for x in re.finditer("x*", "abxd")])
show(repr(re.sub("$", "!", "a\n")), repr(re.sub("$", "!", "a\nb\n")), repr(re.sub("(?m)$", "!", "a\nb\n")))
show(repr(re.sub(r"\s+$", "", "line  \n")))
show(m(re.compile("b|bc").fullmatch("abc", 1)), m(re.compile("b|bc").fullmatch("abcd", 1, 3)))
show(m(re.compile("a$").search("xa\nyy", 0, 3)))
show(m(re.compile("\n    \\d+   # digits\n    $     # end\n", re.VERBOSE).search("12\n")))
show(m(re.compile(r"\d+ # trailing comment", re.X).fullmatch("12")))
show(m(re.fullmatch(r"(?i)ab|abc", "ABC")), m(re.compile("a.c$", re.S).search("a\nc\n")))
show(re.findall(r"^\w+$", "one\ntwo\n"), re.findall(r"^\w+$", "one\ntwo\n", re.M))
"#,
    );
}

// ── 5. Audit ─────────────────────────────────────────────────────────────

#[test]
fn eval_sees_only_the_scope_it_is_given() {
    // `eval(text, globals)` ignored `globals`: the text could read every
    // module global and builtin however the caller restricted it.
    assert_matches_cpython(
        "eval_sees_only_the_scope_it_is_given",
        r#"secret = "module-secret"
trap("default", lambda: eval("secret"))
trap("empty globals", lambda: eval("secret", {}))
trap("given globals", lambda: eval("a + 1", {"a": 41}))
trap("locals", lambda: eval("a + b", {"a": 1}, {"b": 2}))
trap("locals shadow", lambda: eval("a", {"a": 1}, {"a": 2}))
trap("builtins kept", lambda: eval("len('abc')", {}))
trap("no builtins", lambda: eval("len('abc')", {"__builtins__": {}}))
trap("chosen builtins", lambda: eval("size('abc')", {"__builtins__": {"size": len}}))
trap("chosen builtins only", lambda: eval("secret", {"__builtins__": {"size": len}}))
trap("None globals", lambda: eval("secret", None))
trap("bytes", lambda: eval(b"1 + 1"))
trap("lambda", lambda: eval("(lambda q: q * k)(4)", {"k": 3}))
trap("comprehension", lambda: eval("[x * k for x in range(3)]", {"k": 2}))
trap("expr only", lambda: type(eval("1 + 2")).__name__)
for bad in ["x = 1", "import os", "1; 2"]:
    try:
        eval(bad)
        show("ran", bad)
    except SyntaxError:
        show("SyntaxError", bad)
"#,
    );
}

#[test]
fn json_nesting_is_bounded_like_cpython() {
    // The decoder recursed natively with no bound: `[[[[…` from an
    // untrusted source overflowed the stack (SIGABRT).
    assert_matches_cpython(
        "json_nesting_is_bounded_like_cpython",
        r#"import json
def depth_ok(text: str) -> str:
    try:
        json.loads(text)
        return "ok"
    except RecursionError as e:
        return "RecursionError: " + str(e)
show(depth_ok("[" * 9998 + "]" * 9998))
show(depth_ok("[" * 9999 + "]" * 9999))
show(depth_ok('{"a":' * 9998 + "1" + "}" * 9998))
show(depth_ok('{"a":' * 9999 + "1" + "}" * 9999))
show(depth_ok('[{"a":' * 4999 + "1" + "}]" * 4999))
show(depth_ok('[{"a":' * 5000 + "1" + "}]" * 5000))
show(depth_ok("[" * 200000))
show(json.loads("[[1, [2, {\"k\": [3]}]]]"))
"#,
    );
}

#[test]
fn filesystem_shims_do_not_follow_symlinks_where_cpython_does_not() {
    assert_matches_cpython(
        "filesystem_shims_do_not_follow_symlinks_where_cpython_does_not",
        r#"import hashlib
import os
import pathlib
import shutil
import tempfile
def mode_of(p: str) -> str:
    return oct(os.lstat(p).st_mode & 0o7777)
base = tempfile.mkdtemp()
def at(name: str) -> str:
    return os.path.join(base, name)
os.mkdir(at("victim"))
with open(at("victim/keep.txt"), "w") as f:
    f.write("precious")
os.mkdir(at("tree"))
with open(at("tree/x"), "w") as f:
    f.write("x")
os.symlink(at("victim"), at("tree/link_to_victim"))
os.symlink(at("victim"), at("toplink"))
# `rmtree` of a symlink refuses; of a tree, removes the link, not its target.
try:
    shutil.rmtree(at("toplink"))
    show("rmtree followed a symlink")
except OSError as e:
    show("rmtree symlink", type(e).__name__)
seen: list[object] = []
shutil.rmtree(at("toplink"), onexc=lambda fn, path, exc: seen.append((fn.__name__, os.path.basename(path), type(exc).__name__)))
show(seen, os.path.islink(at("toplink")), sorted(os.listdir(at("victim"))))
shutil.rmtree(at("toplink"), ignore_errors=True)
shutil.rmtree(at("tree"))
show(os.path.exists(at("tree")), sorted(os.listdir(at("victim"))))
# `chmod(..., follow_symlinks=False)` must not change the link's target.
with open(at("target"), "w") as f:
    pass
os.chmod(at("target"), 0o644)
os.symlink(at("target"), at("tlink"))
for call in [
    lambda: os.chmod(at("tlink"), 0o600, follow_symlinks=False),
    lambda: pathlib.Path(at("tlink")).chmod(0o600, follow_symlinks=False),
    lambda: pathlib.Path(at("tlink")).lchmod(0o600),
]:
    try:
        call()
        show("changed")
    except NotImplementedError as e:
        show("NotImplementedError", e)
show(mode_of(at("target")))
os.chmod(at("target"), 0o600, follow_symlinks=False)
show(mode_of(at("target")))
os.chmod(at("tlink"), 0o640)
show(mode_of(at("target")))
# Creation modes honour the (ambient) umask.
with open(at("plain"), "w") as f:
    pass
os.makedirs(at("m1/m2"), mode=0o700)
pathlib.Path(at("pl")).mkdir(mode=0o750)
pathlib.Path(at("pl/a.txt")).write_text("hi")
pathlib.Path(at("pl/t")).touch(mode=0o600)
show(mode_of(at("plain")), mode_of(at("m1")), mode_of(at("m1/m2")), mode_of(at("pl")), mode_of(at("pl/a.txt")), mode_of(at("pl/t")))
# The raw wait status: exit code in the high byte, a signal in the low.
show(os.system("exit 3"), os.system("true"), os.system("kill -9 $$"))
show(hashlib.new("SHA256", b"abc").hexdigest() == hashlib.sha256(b"abc").hexdigest())
show(len(os.urandom(32)), os.urandom(16) != os.urandom(16), len(set(os.urandom(4096))) > 200)
shutil.rmtree(base)
"#,
    );
}
