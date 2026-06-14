# Lesson 1 — Getting started

*Zero to Hero · Lesson 1 of 10*

## What Typhon is (and isn't)

Typhon is a **statically-typed, stricter superset of Python that compiles to clean CPython 3.13+.** Think TypeScript, but for Python:

- You write `.ty` files. The compiler, `tyc`, type-checks them and emits ordinary, idiomatic `.py`.
- **Production installs nothing Typhon-specific.** The emitted Python runs on a stock interpreter. Only when you use a handful of features (`Result`, `go`, `freeze let`, …) does the build drop a small self-contained `typhon_runtime/` package next to your output — no PyPI dependency, ever.
- **Not all Python is valid Typhon.** Typhon adds rules — every type annotated, `let`/`mut` on locals, errors as values — that catch a whole class of bugs before runtime.

The entire toolchain — compiler, type checker, formatter, language server, debugger wrapper, REPL, and an in-process interpreter — is **one Rust binary** called `tyc`.

The payoff: Python's ecosystem and readability, with a type system strong enough to make whole categories of `None`-bugs, missing-case bugs, and silent-error bugs *unrepresentable*.

## Install `tyc`

Install a pre-built binary:

```bash
# macOS / Linux
curl -sSL https://raw.githubusercontent.com/codehalwell/typhon/main/install.sh | sh

# Windows (PowerShell)
iwr -useb https://raw.githubusercontent.com/codehalwell/typhon/main/install.ps1 | iex
```

Or build from source (from the repo root):

```bash
cd tyc && cargo build --release
alias tyc="$PWD/target/release/tyc"
tyc --help
```

## Your first program

Scaffold a project:

```bash
tyc init hello && cd hello
```

You get a `typhon.toml`, a `src/main.ty`, and a `tests/` directory. Here is the canonical first program (`examples/01-hello-world/hello.ty`):

```python
import sys


def main() -> None:
    let name: str = sys.argv[1] if len(sys.argv) > 1 else "world"
    print(f"Hello, {name}!")


if __name__ == "__main__":
    main()
```

Three things to notice already:

- `-> None` is **mandatory**. Typhon has no implicit `Any`; a function that returns nothing says so.
- `let name: str = ...` is an **immutable local binding** with an explicit type. Locals always declare `let` or `mut`.
- `import sys`, f-strings, and `if __name__ == "__main__":` are **unchanged from Python.** Typhon is a superset.

## Check, run, build

Here's the inner loop you'll run a thousand times:

```bash
tyc check src/      # parse + type-check, no files written  (your fast feedback)
tyc run             # execute directly in the in-process VM (no .py emitted)
tyc build           # full pipeline → build/main.py
python build/main.py Alice
# Hello, Alice!
```

`tyc run` executes your program in a built-in tree-walking interpreter — no Python process, no files on disk. `tyc build` emits the `.py` for shipping. The emitted `build/main.py` is byte-identical to the input bar formatting. That's the whole point: **every `.ty` file emits valid, idiomatic `.py`.**

## The mental model: eight rules

Typhon looks like Python, but eight rules diverge — and they're the source of every "but this works in Python!" surprise. Internalise these and the rest of this series is just detail.

1. **Every parameter and return type is annotated.** No inference fallback. `def f(a, b):` → `tyc::missing_annotation`.
2. **Local bindings declare `let` (immutable) or `mut` (mutable).** A bare `x = 1` inside a function is an error. (Module-level top bindings default to `let`.)
3. **`T` cannot hold `None`.** Nullable is a separate type, `T?`. You must *narrow* before use.
4. **Methods live in `impl` blocks, not in the `class` body.** The constructor is generated — never write `__init__`.
5. **`Any` only enters through an `unsafe:` region or a `.dty` stub.** No accidental dynamic typing.
6. **`match` on a sealed union must be exhaustive.** Miss a variant → compile error.
7. **Errors flow as `Result[T, E]`, not exceptions.** `?` propagates them cleanly.
8. **Declare-only `let NAME: T` must be definitely assigned** on every path before it's read.

The rest of these lessons are these eight rules, one at a time, with the syntax that supports them.

## Common mistakes

**Forgetting the return type:**

```python
def main():           # ❌ tyc::missing_annotation
    print("Hello")
```

Fix: `def main() -> None:`.

**A bare local binding:**

```python
def main() -> None:
    name = "Alice"    # ❌ tyc::missing_binding_kind
```

Fix: `let name: str = "Alice"` (or `mut` if you reassign it).

## Try it

1. Run `tyc init playground` and `cd` into it.
2. Make `main` print today's weekday. (Hint: `import datetime`, then `datetime.date.today().strftime("%A")` — bind the result with `let`.)
3. Run `tyc check src/`, then `tyc run`.
4. Deliberately delete the `-> None` and re-run `tyc check`. Read the diagnostic. Run `tyc explain missing_annotation`.

## What you learned

- What Typhon is: typed Python in, clean Python out, no runtime dependency.
- How to install `tyc`, scaffold a project, and the `check` / `run` / `build` loop.
- The eight rules that define the language.

**Next:** [Lesson 2 — Values and types](lesson-02-values-and-types.md), where `let`, `mut`, and the non-nullable `T?` form come into focus.
