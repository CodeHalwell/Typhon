# tyc::possibly_unbound

**Warning.** A function-local name is read where the definite-assignment
pass cannot see an assignment on every path that reaches the read — or can
see that no assignment does.

## Example

```ty
def first_word(s: str) -> str:
    if s:
        word = s.split()[0]
    return word           # warning: `word` is not assigned on every path

def after_handler() -> str:
    try:
        int("x")
    except ValueError as e:
        pass
    return str(e)         # warning: `e` is unbound once its handler finishes

def drop(x: int) -> int:
    mut n: int = x
    del n
    return n              # warning: `n` was deleted before this read
```

## Why

CPython raises `UnboundLocalError` (or `NameError`) at runtime on the path
that skipped the assignment: the empty-string call above, the handler that
ran, the `del`. Python binds `except ... as e` only inside the handler and
unbinds it afterwards, and a `for` loop over an empty iterable never binds
its target, so `for x in xs: ...` followed by a read of `x` is the same
shape.

The pass is the one behind `tyc::use_of_uninitialised` (which covers
declare-only `let NAME: T` bindings and is an error): assignments are
tracked structurally, intersected across `if` / `match` / `try` arms,
discarded across loop bodies that may run zero times, and a `return` /
`raise` / `break` / `continue` arm drops out of the intersection. A
`while True:` body counts as running at least once up to its first
`break`. Reads inside nested functions, lambdas and comprehension bodies
are not checked (closures resolve their names when they are called).

Two certainties share the code, distinguished by the message:

- **"is not assigned on every path that reaches here"** — some path
  assigns it, some path does not.
- **"is read before any assignment reaches it"**, **"was deleted before
  this read"**, **"is unbound once its `except ... as` handler finishes"**
  — no path assigns it; the read fails whenever it runs.

It is advice-level because a program whose runtime never takes the missing
path keeps working; the fix is nearly always a default before the branch
or a `return` in the arm that has nothing to assign.

## Fix

```ty
def first_word(s: str) -> str:
    word: str = ""
    if s:
        word = s.split()[0]
    return word

def after_handler() -> str:
    message: str = "ok"
    try:
        int("x")
    except ValueError as e:
        message = str(e)
    return message
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/possibly_unbound.md
