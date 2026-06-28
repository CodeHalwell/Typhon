# Typhon Adversarial Pre-Release Review — 2026-06-28

Reviewer: automated adversarial sweep against `tyc 1.0.0-alpha` (release build).
Scope: type-checker soundness & false positives, VM↔CPython parity, parser/checker
robustness, formatter round-tripping, and docs/onboarding accuracy.

## Verdict

The toolchain is in genuinely good shape for an **alpha**: `cargo test` (full
workspace), `cargo clippy -D warnings`, `cargo fmt --check` all pass; the entire
`examples/` (32) + `examples/apps/` (15) corpus type-checks clean and emits
syntactically-valid Python; the compiler does not panic on the vast majority of
malformed input. The findings below are the sharp edges an adversarial user will
hit. Two of them (the narrowing-invalidation soundness cluster, and the `and`/`or`
narrowing false positive) are worth fixing **before** a public launch because they
sit on idiomatic, everyday code.

Severity legend: **CRITICAL** = silent wrong/unsafe in idiomatic code ·
**HIGH** = wrong/blocking in plausible code · **MEDIUM** = edge case · **LOW** = cosmetic.

---

## 1. Type-checker soundness (false negatives)

### CRITICAL — Flow narrowing of non-local places is not invalidated across calls/aliases
The checker correctly invalidates a narrowed **local** on reassignment, but a
narrowed **global / instance field / nested attribute** keeps its narrowed type
across (a) a call that reassigns it and (b) a write through an alias. This breaks
both nullability **and** the headline sealed-union exhaustiveness guarantee.

`scratch_probe_sound/p15_field_narrow.ty` — checks clean, crashes at runtime:
```python
class Box:
    val: str?
impl Box:
    def clear(self) -> None:
        self.val = None
    def use(self) -> int:
        if self.val is not None:
            self.clear()              # nulls the field
            return len(self.val)      # checker still thinks str → TypeError at runtime
        return 0
```
`scratch_probe_sound/p34_match_global.ty` — match-narrowing of a global survives a
variant-changing call → `AttributeError: 'B' object has no attribute 'x'` at runtime,
clean at check time. Both independently reproduced on VM and compiled CPython.
Also: `p13` (global across a clearing call), `p16` (field via free-function alias),
`p24` (object field via `list` alias), `p37` (nested `o.inner.val`).

Fix direction: invalidate narrowing of non-local places at every call site and at any
write that could alias the same object (`tyc-types` flow analysis).

---

## 2. Type-checker false positives (valid code rejected)

### HIGH — `x is not None and x.method()` rejected with `tyc::nullable_use`
The canonical Python null-check idiom is rejected when the narrowed variable is the
**receiver of a bare attribute/method access used directly as the boolean operand**.
```python
def a(x: str?) -> bool:
    return x is not None and x.startswith("a")   # ❌ tyc::nullable_use (runs fine: True/False)
def c(s: str?) -> None:
    if s is None or s.startswith("#"):           # ❌ same, a documented narrowing form
        return
```
`x is not None and len(x) > 0` and `... == True` narrow correctly, confirming it is
specifically the bare-access-as-operand case. mypy/pyright both accept all of these.
Reproducer: `scratch_probe_fp/REPRO_and_narrow.ty`. Users will hit this constantly.

---

## 3. VM ↔ CPython parity (`tyc run` drop-in promise)

### CRITICAL — `as!` checked cast is a no-op in the VM
`__typhon_checked_cast__` is hardcoded identity in the VM (`tyc-vm/src/builtins.rs:1315`).
A wrong-shaped value passes silently under `tyc run`, while `tyc build && python` correctly
raises `TypeError`. Since `tyc run` is the **default** mode and `as!` is marketed as the
*sound* boundary cast, a developer validating a boundary with `tyc run` sees it "pass" and
ships code that throws in production.
```python
let raw: object = ["a", "b", "c"]
let d: dict[str, int] = raw as! dict[str, int]   # VM: prints the list; CPython: TypeError
```
This also contradicts the v0.15.0 changelog ("The VM intercepts `__typhon_checked_cast__`
… runs under `tyc run`") and the §5.10 "identity passthrough" note — the docs disagree
with each other. The VM unit test `checked_cast_union_and_parametric_targets_run` only
asserts the program *runs*, not that the cast *validates*.

### CRITICAL — int/float/bool keys not hash-equivalent
`scratch_probe_vm/t39.ty`: `{1: ..., 1.0: ..., True: ...}` keeps 3 keys (CPython: 1);
`len({1, 1.0, True, 2})` → 3 (CPython: 2); `1 in {1.0}` → False (CPython: True). Silent
data-loss in any dict/set mixing numeric types.

### CRITICAL — non-frozen dataclass instances wrongly hashable in the VM
`scratch_probe_vm/t08.ty`: a plain `class` (→ `@dataclass(slots=True)`) is unhashable in
CPython, but the VM allows it as a dict/set key — `dict[P, str]` runs in the VM and raises
`TypeError: unhashable type: 'P'` under `tyc build && python`. (`tyc check` also accepts the
unhashable-key dict — a related checker gap.)

### CRITICAL (documented limitation) — generators are eagerly materialized
`scratch_probe_vm/t23.ty`: side effects fire at creation, in the wrong order; infinite
generator + `islice`/`takewhile` hits the 1M cap and dies (`t22.ty`). Already documented
as a known VM limitation, but it is a real drop-in divergence — keep it loud in release notes.

### HIGH — VM stdlib/operator gaps (wrong or crashing where CPython works)
- `bin()/hex()/oct()` of negatives → 64-bit two's-complement instead of `-0b101` (`t33b.ty`).
- `Counter` modeled as bare dict: `c1 + c2`, `c1 - c2`, `&`, `|` all `TypeError`; repr is a
  plain dict not `Counter({...})` count-sorted (`t15.ty`, `t15b.ty`).
- `dict | dict` / `dict |= dict` merge → `TypeError` (`t40.ty`).
- `"%(name)s" % {...}` mapping format → `ValueError` (`t19.ty`).
- `date.weekday()`, `datetime.strftime()`, `math.isclose`, `Path.parts` missing (`t20*.ty`, `t35.ty`).

### MEDIUM — exception `str`/`repr` text mismatches
`repr(KeyError('z'))` → `KeyError("'z'")` (CPython `KeyError('z')`); `int("abc")` message
omits "with base 10"; float `ZeroDivisionError` says "division by zero" not "float division
by zero" (`t04.ty`).

---

## 4. Robustness — stack overflow (SIGABRT, no diagnostic)

### HIGH — flat binary-operator chain overflows the stack
`let x: int = 1+1+…` (~5000 terms) → `fatal runtime error: stack overflow, aborting`
(exit 134) from `tyc check`/`run`. A flat `a+b+c…` chain is plausible generated/serialized
code. The recursion is in `tyc-types`/`tyc-vm` AST walks (`tyc fmt` survives it).

### MEDIUM — deeply nested expr/type/pattern forms overflow
`((((…))))`, `----…1`, `list[list[…]]`, `int|int|…`, nested f-strings, nested sequence
patterns — all SIGABRT past a depth threshold. parens/f-string variants overflow in the
parser (`tyc fmt` too); the rest in analysis/VM. A public compiler should emit a
`nesting_too_deep` diagnostic (or use `stacker`) rather than abort.

Reproducers: `scratch_probe_fuzz/repro/`. Everything else (~90 malformed inputs:
half-written `as!`/`?`/`gather:`/`enum`/`comptime`, BOM/CRLF/null bytes, unicode idents,
recursive type aliases, quotes-in-comments) produced clean `tyc::` diagnostics.

---

## 5. Docs & onboarding

### HIGH — `sealed union NAME:` block form is in the cheatsheet but does not parse
`docs/cheatsheet.md:140` (and `tyc cheatsheet`, `REFERENCE.md`, and `SKILL.md` which
explicitly claims it "is also accepted") show:
```
sealed union Shape:
    Circle(radius: float)
    Square(side: float)
```
`tyc check` → `tyc::parse`. The only working form is `type Shape = Circle | Rectangle`.
This is the first thing a new user copies. Either implement the keyword form or remove it
everywhere.

### MEDIUM — 5 diagnostic codes are unknown to `tyc explain`
`field_default_ordering`, `use_of_uninitialised`, `pub_name_collision`, `missing_field_init`
have committed `docs/diagnostics/*.md` pages and are emitted by the compiler, but
`tyc explain <code>` returns "unknown diagnostic code" and they're absent from
`tyc explain --list`. `kind_mismatch` is advertised in `README.md:129` but has no doc page
and is also unknown to `explain`. CLAUDE.md mandates every diagnostic be explainable.

### MEDIUM — cheatsheet `comptime let` example omits the required annotation
`docs/cheatsheet.md:176` shows `comptime let API_URL = env(...)`, which fails with the
**misleading** `tyc::comptime — comptime binding 'API_URL' has no initialiser` (it has an
initialiser; it's missing `: str`). Fix the example and the message.

### LOW
- `docs/zero-to-hero/lesson-*.md` fence `.ty` snippets as ` ```python `.
- "emitted `build/main.py` is byte-identical to the input bar formatting" overstates it
  (`Result`/`?`/`freeze let`/`guard` all lower to more code).

---

## 6. Formatter (`tyc fmt`)

The formatter is **robust**: across 45 hand-written programs covering every special
form + the full 259-file `examples` corpus + ~30 adversarial probes, `tyc fmt` was
idempotent, never corrupted/emptied a file, and preserved `tyc run` output exactly.
Typhon-only lines are passed through verbatim (ruff never sees them). One defect:

### HIGH — multi-line `freeze let` with `#` inside a string + a closing bracket on the same line fails to parse
```python
freeze let X = {
    "list": ["a#b"],     # the # is the sole trigger
    "after": 1,
}
```
`tyc::parse — Expected ')', found newline` from `tyc check`/`run`/`fmt`. Removing the
`#` checks clean. Root cause: `tyc-syntax/src/preprocess.rs` `bracket_delta_outside_strings`
(~line 1424) treats `#` as a comment without first entering mid-line string state, so the
closing bracket after a `#`-string is never counted and the synthesized `__typhon_freeze__(`
is left unterminated. Same bug *class* as the v0.13.1 `#`-string fix, but in the
nested-bracket continuation path. Fails loudly and leaves the file intact (not silent
corruption), hence HIGH not CRITICAL. Reproducers: `scratch_probe_fmt/REPRO_freeze_*.ty`.

## Recommended pre-launch priority

1. **Narrowing invalidation for non-local places** (§1) — soundness, idiomatic.
2. **`and`/`or` receiver narrowing** false positive (§2) — everyday code, frustrating.
3. **`as!` VM no-op** (§3) — fix the VM to run the structural check, or loudly document &
   reconcile the contradictory docs; at minimum fix the misleading unit-test/changelog.
4. **Cheatsheet `sealed union` + `comptime let` + `tyc explain` registration** (§5) — cheap,
   high-visibility first-impression fixes.
5. VM numeric-key hashing & dataclass hashability (§3) — foundational parity.
6. Stack-overflow guard (§4).
