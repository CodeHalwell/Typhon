# 1. Hello, world

The shortest useful Typhon program, end to end: install the compiler, scaffold a project, write code, compile it, run the emitted Python.

## Install `tyc`

`tyc` is a single Rust binary. Build it from source:

```bash
git clone https://github.com/CodeHalwell/Typhon.git
cd Typhon/tyc
cargo build --release
```

The binary lands at `tyc/target/release/tyc`. Put it on your `PATH`, or alias it:

```bash
alias tyc="$PWD/target/release/tyc"
```

Confirm it works:

```bash
tyc --help
```

## Scaffold a project

```bash
tyc init hello
cd hello
```

`tyc init` produces:

```
hello/
├── typhon.toml      # project config
├── src/
│   └── main.ty      # entry point
└── tests/
```

`typhon.toml` looks like this:

```toml
[project]
name = "hello"
version = "0.1.0"
src = "src"
out = "build"

[python]
target = "3.13"

[emit]
class-default = "dataclass"
format = true
```

## Your first program

Edit `src/main.ty`:

```python
def main() -> None:
    print("Hello, world")

if __name__ == "__main__":
    main()
```

Three things to notice:

- **Return types are required.** `-> None` is not optional — Typhon's `[strictness] no-implicit-any` defaults to `true`. Omit it and `tyc check` complains.
- **`print` works.** Built-in Python functions are available without ceremony; Typhon is a superset of typed Python.
- **`if __name__ == "__main__":` is unchanged.** Typhon emits idiomatic Python; module entrypoints look identical to a `.py` file.

## Check, build, run

```bash
tyc check src/    # parse + type-check, no output
tyc build         # full pipeline → build/main.py
python build/main.py
# Hello, world
```

`tyc build` runs every stage: lex, parse, resolve, type-check, desugar, emit, and (if `[emit] format = true`) post-process with `ruff format`.

## What does the emitted Python look like?

`build/main.py`:

```python
def main() -> None:
    print("Hello, world")


if __name__ == "__main__":
    main()
```

For "hello, world" the input and output are byte-identical bar formatting. That's the point: **every `.ty` file emits valid, idiomatic `.py`**. There's no Typhon runtime to install on production servers — Typhon is a build-time tool, like TypeScript.

## A slightly less trivial example

Take a name from `argv` and greet it:

```python
import sys

def main() -> None:
    let name: str = sys.argv[1] if len(sys.argv) > 1 else "world"
    print(f"Hello, {name}")

if __name__ == "__main__":
    main()
```

New things:

- **`let name: str = ...`** — an immutable local binding with an explicit type. Use `mut` if you want to reassign it later. (Guide 2 goes deep on this.)
- **Top-level imports** — `import sys` is unchanged from Python. Typhon adds `lazy import` for deferred loading (guide 10), but plain `import` still works.

Compile and run:

```bash
tyc build
python build/main.py Alice
# Hello, Alice
```

## Common first-time errors

**Forgetting the return type:**

```python
def main():           # ❌ missing return annotation
    print("Hello")
```

```
error[tyc::missing_annotation]: `return type` on `main` is missing a type annotation
 ┌─ src/main.ty:1:5
 │
1 │ def main():
 │     ^^^^ annotation required here
 = help: Typhon's Rule 1: annotate every parameter and return type. For a function that returns nothing, write `-> None`.
```

Fix: write `def main() -> None:`.

**Using `=` for a binding without `let` or `mut`:**

```python
def main() -> None:
    name = "Alice"    # ❌ missing let/mut
    print(name)
```

```
error[tyc::missing_binding_kind]: local bindings must be declared with `let` or `mut`
 ┌─ src/main.ty:2:5
 │
2 │     name = "Alice"
 │     ^^^^ add `let` (immutable) or `mut` (mutable) here
```

Fix: `let name: str = "Alice"` or `mut name: str = "Alice"`. (Top-level module bindings default to `let` automatically; locals are explicit.)

## What you've learned

- How to install and invoke `tyc`.
- The shape of a Typhon project: `typhon.toml`, `src/`, `tests/`.
- The three commands you'll use daily: `tyc check`, `tyc build`, `tyc fmt`.
- That Typhon emits clean Python with no runtime dependency.

Next: [Values and types](02-values-and-types.md) — `let` vs `mut`, the type system, and what "non-nullable by default" means in practice.
