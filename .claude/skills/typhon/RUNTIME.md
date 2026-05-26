# Typhon Runtime and VM

Two things live under "runtime" in Typhon: the **generated `typhon_runtime/` package** that ships alongside emitted Python, and the **in-process tree-walking interpreter** (`tyc-vm`) that powers `tyc run`. Both deserve depth — they're easy to confuse but serve different audiences.

---

## Part 1 — The `typhon_runtime` package

`typhon_runtime/` is a **generated package** the build owns. It is written into the project's output directory on every `tyc build` whenever the desugar pass sets `needs_typhon_runtime = true` — i.e. whenever the emitted code references any of:

- `Ok` / `Err` / `Result` types or pattern matches
- `typhon_runtime.tasks.spawn` (from `go` lowering)
- `typhon_runtime.lazy.lazy_let` (from module-level `lazy let`)
- `typhon_runtime.lazy.lazy_import` (from `lazy import` lowering)
- `typhon_runtime.parallel.map_pure` (from `[strictness] auto-parallel`)
- `typhon_runtime.freeze.deep_freeze` (from `freeze let`)

The package source is embedded as `const &str` templates in `tyc/src/commands/build.rs` (`TYPHON_RUNTIME_INIT_PY`, `TASKS_PY`, `LAZY_PY`, `STDLIB_PY`, `RESULT_PY`, `PARALLEL_PY`, `FREEZE_PY`).

### 1.1 Package layout

| File | Surface |
|---|---|
| `__init__.py` | Re-exports `Ok`, `Err`, `Result` so `from typhon_runtime import Ok, Err, Result` works |
| `result.py` | `Ok[T]`, `Err[E]`, `Result[T, E]` dataclasses + `.map`/`.map_err`/`.and_then`/`.or_else` methods (v0.6.0). Frozen, slots-based. CPython repr matches the v0.3.0 O24 fix (`Ok(value=20)` / `Err(error='oops')`). |
| `tasks.py` | `spawn(coro)` — adds the task to a strong-ref `_BACKGROUND` set with a done-callback `discard` to prevent GC mid-flight. Closes the asyncio `create_task` weakref gotcha. |
| `lazy.py` | `_LazyModule` / `lazy_import(name)` — attribute-proxy with double-checked locking. `_LazyValue` / `lazy_let(factory)` — sentinel-cached single-shot evaluator. |
| `parallel.py` | `map_pure(fn, iterable)` — `concurrent.futures.ThreadPoolExecutor`-backed parallel map; degrades to sequential on GIL-locked CPython. Used by list/set/dict comprehension rewrites under `auto-parallel`. |
| `freeze.py` | `deep_freeze(obj)` — recursively replaces `list → tuple`, `dict → MappingProxyType`, `set → frozenset`; raises `TypeError` at startup on un-freezable values (file handles, sockets, generators, non-frozen dataclasses). |
| `stdlib.py` | Internal helpers used by lowering passes |

### 1.2 User-facing contract

**The runtime is generated; users do not edit it and do not pip-install it.** It is regenerated on every `tyc build`. There is no PyPI package — every project ships its own copy alongside the emitted `.py`. Anyone consuming the emitted code from outside Typhon just imports `typhon_runtime` like any other package in the project's `src` / `build` tree.

Users should:

- **Treat `typhon_runtime/` as a build artifact.** Don't add it to source control — let `tyc build` regenerate it.
- **Use `from typhon_runtime import Ok, Err, Result` in `.dty` stubs** if the stub describes Typhon-aware code.
- **Never reach into `typhon_runtime.tasks._BACKGROUND` etc.** — implementation detail, may change.

### 1.3 `Ok` / `Err` / `Result` shape

```python
@dataclass(slots=True, frozen=True)
class Ok(Generic[T]):
    value: T

    def map[U](self, f: Callable[[T], U]) -> "Result[U, Any]":
        return Ok(f(self.value))

    def map_err[F](self, f: Callable[[Any], F]) -> "Ok[T]":
        return self

    def and_then[U, E](self, f: Callable[[T], "Result[U, E]"]) -> "Result[U, E]":
        return f(self.value)

    def or_else[U, E](self, f: Callable[[Any], "Result[U, E]"]) -> "Ok[T]":
        return self


@dataclass(slots=True, frozen=True)
class Err(Generic[E]):
    error: E

    def map[U](self, f: Callable[[Any], U]) -> "Err[E]":
        return self

    def map_err[F](self, f: Callable[[E], F]) -> "Err[F]":
        return Err(f(self.error))

    def and_then[T, F](self, f: Callable[[Any], "Result[T, F]"]) -> "Err[E]":
        return self

    def or_else[T, F](self, f: Callable[[E], "Result[T, F]"]) -> "Result[T, F]":
        return f(self.error)


Result = Ok[T] | Err[E]
```

The exact emission may differ slightly; the contract is: `Ok.value`, `Err.error`, the four combinators, frozen, slot-based, hashable.

### 1.4 `tasks.spawn` shape

```python
_BACKGROUND: set[asyncio.Task] = set()

def spawn(coro: Coroutine[Any, Any, T]) -> asyncio.Task[T]:
    task = asyncio.create_task(coro)
    _BACKGROUND.add(task)
    task.add_done_callback(_BACKGROUND.discard)
    return task
```

The strong-ref registry plus the done-callback closes the well-known asyncio gotcha (event loop holds only weak refs to tasks; a fire-and-forget task whose handle is dropped can be GC'd mid-flight).

### 1.5 `lazy_let` shape

```python
_SENTINEL = object()

class _LazyValue(Generic[T]):
    __slots__ = ("_factory", "_value", "_lock")

    def __init__(self, factory: Callable[[], T]) -> None:
        self._factory = factory
        self._value: Any = _SENTINEL
        self._lock = threading.Lock()

    def __call__(self) -> T:
        if self._value is _SENTINEL:
            with self._lock:
                if self._value is _SENTINEL:
                    self._value = self._factory()
        return self._value  # type: ignore[return-value]


def lazy_let(factory: Callable[[], T]) -> Callable[[], T]:
    return _LazyValue(factory)
```

Thread-safe one-shot. Module-level `lazy let CFG: Config = load()` lowers to `CFG = lazy_let(lambda: load())`; the first access pays the load cost, subsequent accesses are memory reads.

### 1.6 `deep_freeze` shape

```python
def deep_freeze(value: Any) -> Any:
    if isinstance(value, (int, float, complex, str, bytes, bool, type(None))):
        return value
    if isinstance(value, tuple):
        return tuple(deep_freeze(v) for v in value)
    if isinstance(value, list):
        return tuple(deep_freeze(v) for v in value)
    if isinstance(value, frozenset):
        return frozenset(deep_freeze(v) for v in value)
    if isinstance(value, set):
        return frozenset(deep_freeze(v) for v in value)
    if isinstance(value, dict):
        return types.MappingProxyType({
            deep_freeze(k): deep_freeze(v)
            for k, v in value.items()
        })
    if dataclasses.is_dataclass(value) and getattr(type(value), "__dataclass_params__", None) is not None:
        params = type(value).__dataclass_params__
        if getattr(params, "frozen", False):
            return value
    raise TypeError(f"deep_freeze: value of type {type(value).__name__} is not freezable")
```

Frozen dataclasses pass through unchanged. File handles, sockets, generators, non-frozen dataclasses, and anything else without a clean immutable equivalent raise `TypeError` at startup so the failure is loud.

---

## Part 2 — The in-process VM (`tyc-vm`)

`tyc-vm` is a Rust tree-walking interpreter that runs `.ty` source **without ever emitting Python**. It is the **default** mode for `tyc run`. There is no `build/`, no `typhon_runtime.py`, no CPython spawn.

### 2.1 Architecture

```
.ty source
  → tyc-syntax::preprocess (expand ?, |>, gather:, go, with-chains, lazy)
  → tyc-syntax::parse_module
  → tyc-vm::Interpreter
      ├── env.rs       lexical scope chains with global/nonlocal
      ├── value.rs     Value enum (Rc-based), Python-compatible eq/ord/hash
      ├── interp.rs    statement + expression evaluators
      ├── builtins.rs  native fns, method dispatch, stdlib modules
      └── ffi.rs       file-handle shim (no PyO3 yet)
```

The VM's `Value` enum covers `Int(i64)`, `Float`, `Bool`, `Str`, `Bytes`, `None`, `List`, `Tuple`, `Dict`, `Set`, `FrozenSet`, `Function`, `Method`, `Class`, `Instance`, `NativeFunction`, `Module`, `Iterator`, `Generator`, `Coroutine`, `Task`, `Ok`/`Err`. Reference counting via `Rc<RefCell<...>>` — no GC, no thread sharing.

### 2.2 Why a VM at all?

- **Snappy iteration loop.** No build step, no `typhon_runtime/` materialisation, no CPython spawn. `tyc run hello.ty` is sub-100ms cold-start for typical hello-world programs.
- **Build-free CI.** `tyc check && tyc run -- some-test-input` exercises the program without emitting Python.
- **Education / REPL.** The VM backs `tyc repl` (in compile-then-exec mode today, but designed for future direct integration).

### 2.3 Supported language surface

All standard literals, including f-strings with `{x:.2f}` width / precision / commas. `let`, `mut`, walrus `:=`, augmented assigns, tuple/list unpacking with starred targets.

Full `if`/`elif`/`else`/`while`/`for`/`break`/`continue`/`return`/`pass`. `match` with literal / capture / wildcard / sequence / class patterns, including native `Ok(x)` / `Err(e)` matching. Arm bodies execute against the parent env (v0.3.1 N9 fix — pattern captures lift into the parent env on accept, matching CPython).

Functions with positional/keyword/default/`*args`/`**kwargs`/closures, recursion (256-frame default; configurable). Classes with annotated-field constructor synth, explicit `__init__`, methods (in `class` body or sibling `impl Foo:`), single inheritance.

Decorators recognised: `@pure`, `@dataclass`, `@gatherable`, `@override`, `@final`, `@staticmethod`, `@classmethod` (no-ops); `@memo` / `@cache` / `@lru_cache` (value-keyed memoisation).

`try`/`except T as e`/`else`/`finally`/`raise`. `Result` ADT with native `Ok(...)` / `Err(...)` and `?`. Imports of native stdlib (table below). Comprehensions (eagerly materialised in v1).

A broad built-in surface:
`print`, `len`, `range`, `str`, `int`, `float`, `bool`, `list`, `tuple`, `dict`, `set`, `frozenset`, `repr`, `type`, `isinstance`, `abs`, `min`, `max`, `sum`, `sorted`, `reversed`, `enumerate`, `zip`, `map`, `filter`, `all`, `any`, `next`, `iter`, `hex`, `bin`, `oct`, `chr`, `ord`, `round`, `input`, `hash`, `id`, `callable`, `open` (text-read only).

Built-in method dispatch on `str` / `list` / `dict` / `set` / `tuple` / `int.bit_length` / `float.is_integer`. Set operators `|` / `&` / `-` / `^`.

### 2.4 Native stdlib modules

| Module | Surface |
|---|---|
| `math` | `pi`, `e`, `inf`, `nan`, `sqrt`, `floor`, `ceil`, `log` (with base), `log2`, `log10`, `exp`, `sin`, `cos`, `tan`, `pow`, `fabs` |
| `os` | `getenv`, `environ`, `path.exists`, `path.isfile`, `path.isdir` |
| `sys` | `argv`, `platform`, `version`, `exit(code)` |
| `json` | `dumps`, `loads` (full JSON 7159 surface) |
| `time` | `time()`, `sleep()`, `monotonic()` |
| `random` | `random()`, `randint(a, b)`, `seed(n)` — xorshift PRNG, **not** cryptographic |
| `typhon_runtime` | `Ok`, `Err`, `tasks.spawn` (synchronous shim), `lazy.lazy_let`, `lazy.lazy_import` |

### 2.5 Not supported (surfaces as `NotImplementedError` or `ImportError`)

- **`async def` / `await` / `gather:` / `go`** — VM is synchronous-only today.
- **Real generator functions with `yield`** — generator-expression shape works but materialises eagerly.
- **Template strings** (`t"…"`).
- **IPython escapes** (`%foo`, `!bar`, etc.).
- **`with` statements other than `open()`** — full ctx-manager support is a follow-up.
- **Mapping / class-keyword patterns in `match`**.
- **Bigint arithmetic** — ints are `i64`; overflow raises `OverflowError`.
- **Most third-party packages** — `numpy`, `requests`, `pandas`, `pydantic`, etc.

### 2.6 Fallback rule

`tyc run` falls back to `--compile` (alias `--no-vm`) whenever the program imports a module the VM can't speak natively. The VM raises `ImportError` with a pointer to `--compile`.

As of **v0.3.1**, `tyc run` (VM mode) gates on the static `tyc check` pipeline first — unresolved names, type mismatches, and arity errors fail the same way `tyc check` would. Set `TYC_SKIP_CHECK=1` to bypass for stress harnesses; `--compile` has no equivalent bypass because the build pipeline always type-checks.

```bash
tyc run                    # default — VM
tyc run --compile          # build-then-exec via CPython
tyc run --compile --temp   # build into a tempdir deleted on exit
TYC_SKIP_CHECK=1 tyc run   # bypass pre-VM check
```

### 2.7 Performance and roadmap

The VM hits roughly CPython 3.13 speed on arithmetic microbenchmarks; single-threaded `Rc`, no parallelism. A bytecode VM and PyO3 FFI for unsupported modules are on the roadmap.

### 2.8 Diagnostics

Runtime failures surface as:

```text
Traceback (most recent call last):
KIND: MESSAGE
```

The traceback names frames but not source lines (full source-line tracebacks inside the VM are a tracked follow-up). For full fidelity today, use `tyc run --compile` then `tyc trace` on the captured traceback — that path is fully `.py.map`-aware.

### 2.9 When to use which mode

| Situation | Use |
|---|---|
| Quick iteration on pure-Typhon code (no numpy / requests / etc.) | `tyc run` (default VM) |
| Need a third-party library | `tyc run --compile` |
| Production deployment | `tyc build` then run `python build/main.py` as usual |
| CI smoke test for compile correctness | `tyc check && tyc build --check` |
| Stepping through emitted code | `tyc debug` (Typhon-aware pdb wrapper) |
| Capturing a Python traceback and mapping back to `.ty` | `python build/main.py 2> err.log && tyc trace err.log` |

---

## Part 3 — `.py.map` v2 source maps

Every emitted `.py` ships with a `<out>/.sourcemaps/<rel>.py.map` sidecar (v0.6.1 — legacy adjacent `<out>/<rel>.py.map` layout still readable). The map is **v2**: a per-statement `(out_line → ty_line)` table. Format is JSON.

Consumers:

- **`tyc trace`** — reads a captured Python traceback from stdin or a file and rewrites every `path.py:LINE[:COL]` reference back to `.ty`. Paths with spaces use a longest-candidate walk-left lookup (v0.5.0). Cached per-line.
- **`tyc debug --break <ty-file>:<line>`** — translates the Typhon source location through `.py.map` and injects `-c "break build/main.py:N"` into the debugger session.
- **`tyc debug` source-mapping wrapper** (v0.5.0) — loads every `.py.map` at startup, overrides `pdb.Pdb`'s `do_list`, `do_where`, `format_stack_entry`, and `prompt`. The entire debugger UI reads `.ty` paths and source slices. `--raw-pdb` opts out.
- **`tyc ty`** (v0.5.0) — same loader as `tyc trace` (shared `commands/source_map.rs`). Rewrites `ty`'s `*.py:LINE[:COL]` diagnostics to `.ty` coordinates. `--raw` opts out.
- **`tyc lsp`** — cross-file go-to-definition crosses the `.ty` / `.py` boundary via the resolver's `resolved_module` query plus `.py.map`.

Lines without a recognisable `.py:` reference (summary text, blank lines, snippet excerpts) are forwarded unchanged.

---

## Part 4 — `.dty` stubs and `.pyi` emission

`.dty` is Typhon's stub format. Bodies omitted; signatures + class/method declarations + annotated fields only. Every `.dty` next to the project compiles to a PEP 561 `.pyi` during `tyc build` — always on (the previously-documented `[emit] pyi-stubs` toggle has been removed).

Workflow:

1. Author `foo.dty` with declarations only.
2. `tyc build` emits `foo.pyi` so mypy / pyright / Pyrefly / `ty` see the Typhon surface API.
3. `tyc check --stubs` parses every `.dty` and diffs its surface against the `.ty`/`.py` implementation — missing-in-impl, missing-in-stub, signature-mismatch → `tyc::stub_mismatch`. Severity controlled by `[strictness] stub-check`.
4. `tyc stubtest` runs `python -m mypy.stubtest` against every emitted `.pyi`. Catches dynamically-created members the AST can't see (`__init_subclass__` injection, metaclass-driven member registration, Pydantic auto-generated fields).

**Influence on the checker.** Cross-module shape extraction consumes both `.ty` and `.dty` on equal footing. **When both define the same name, stubs win.** This makes `.dty` the natural way to describe foreign Python libraries to the Typhon checker.

Example `.dty`:

```python
# src/stubs/redis.dty
class Redis:
    host: str
    port: int

impl Redis:
    def get(self, key: str) -> str?
    def set(self, key: str, value: str) -> bool
    def delete(self, *keys: str) -> int
    def keys(self, pattern: str = "*") -> list[str]
```

---

## Part 5 — Install paths and binary distribution

Pre-built binaries ship on every GitHub Release.

| OS | Arch | Target triple | Archive |
|---|---|---|---|
| macOS | Apple Silicon (`arm64`) | `aarch64-apple-darwin` | `.tar.gz` |
| macOS | Intel (`x86_64`) | `x86_64-apple-darwin` | `.tar.gz` |
| Linux | `x86_64` | `x86_64-unknown-linux-gnu` | `.tar.gz` |
| Linux | `aarch64` | `aarch64-unknown-linux-gnu` | `.tar.gz` |
| Windows | `x86_64` | `x86_64-pc-windows-msvc` | `.zip` |

Linux artifacts are built on Ubuntu 22.04 / glibc 2.35. Ubuntu 22.04+, Debian 12+, Fedora 36+, RHEL 9+, Arch all supported. Alpine/MUSL: install `gcompat` or build from source.

### macOS / Linux

```bash
curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh
```

The script detects OS+arch, resolves the latest tag via the GitHub API, downloads the tarball + `SHA256SUMS`, verifies with `shasum -a 256 -c` or `sha256sum -c`, extracts, installs `tyc` to `$HOME/.local/bin` (no `sudo`). On macOS it clears `com.apple.quarantine` so Gatekeeper doesn't prompt. Re-running upgrades in place.

Env vars / flags: `TYPHON_VERSION=v0.8.1`, `TYPHON_INSTALL_DIR=/opt/typhon/bin`, or `sh install.sh --version=v0.8.1 --dir=/opt/typhon/bin`.

### Windows (PowerShell)

```powershell
iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex
```

Detects arch, downloads zip + `SHA256SUMS`, verifies with `Get-FileHash`, extracts to `%LOCALAPPDATA%\Programs\Typhon\` (no admin), adds the dir to user-level `PATH`. Env vars / flags: `$env:TYPHON_VERSION = 'v0.8.1'`, `$env:TYPHON_INSTALL_DIR = 'C:\Tools\Typhon'`, or `.\install.ps1 -Version v0.8.1 -InstallDir C:\Tools\Typhon -NoPath`.

### Build from source

FreeBSD, MUSL Linux, Windows ARM64, or development:

```bash
cd tyc
cargo build --release    # ./target/release/tyc
```

Requires Rust 1.94+. CPython 3.13+ is required at runtime to *execute* emitted code; `tyc` itself does not need Python.

### Code signing

- **macOS**: ad-hoc signed (`codesign --sign -`) — avoids the `killed: 9` failure mode on Apple Silicon but doesn't establish Gatekeeper trust.
- **Windows**: not yet Authenticode-signed; SmartScreen may warn on manual download.
- **Linux**: unsigned (verify SHA-256).

### Uninstall

```bash
rm $HOME/.local/bin/tyc                         # macOS / Linux
Remove-Item "$env:LOCALAPPDATA\Programs\Typhon\tyc.exe"  # Windows (and clean the PATH entry)
```

---

## Part 6 — When to read each section

- **Building a Typhon program that uses `Result` or `go` or `lazy let`** — skim Part 1 to know what gets generated and where.
- **Asking "why does my program work with `tyc run` but fail with `tyc run --compile`?"** — read Part 2.5 (not-supported list) and 2.6 (fallback rule).
- **Setting up a debugger session** — read Part 3 (`.py.map`) and `CLI.md` `tyc debug`.
- **Writing a `.dty` for a foreign Python library** — read Part 4.
- **Installing or upgrading `tyc`** — read Part 5.
