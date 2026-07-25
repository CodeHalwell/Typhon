# tyc::type_mismatch

Fires when an expression of one type is supplied where a different type was
expected — function arguments, return values, assignments, container element
types, etc.

## Example

```ty
def double(n: int) -> int:
    return n * 2

def main() -> None:
    let result: int = double("3")  # error: expected `int`, found `str`
    print(result)
```

A reassignment variant fires when `mut x: T = ...` is followed by
`x = <value of some other type>`. `mut` allows new values of the same
declared type, never a re-typing.

## Why

Typhon's type checker is strict-by-default: an annotation is a contract, not
a hint. Permitting an `str` where `int` was promised would silently propagate
a wrong-typed value across the program and surface as a runtime `TypeError`
far from the source of the problem.

## Reading a fixed-arity tuple at a non-constant index

`tuple[int, str]` holds a different type in each slot, so the type a read
produces depends on *which* slot is read. With a literal index the compiler
resolves the slot exactly; with a variable index it cannot, so the read is
typed as the **union of every slot type**:

```ty
def main() -> None:
    let t: tuple[int, str] = (1, "abc")
    mut i: int = 1
    let a: int = t[i]  # error: expected `int`, found `int | str`
    let b: str = t[i]  # error: expected `str`, found `int | str`
    print(a, b)
```

Both fail, which is the point — they are the same expression, so they cannot
both be right. The `int | str` in the message means *the index is not
statically known*, not that the tuple changed shape. Three ways out:

1. index with a literal when you know the slot — `let v: int = t[0]`;
2. annotate the union and narrow before use —
   `let v: int | str = t[i]` then `if isinstance(v, int): …`;
3. use a homogeneous `tuple[int, ...]` (or a `list[int]`) when every slot
   holds the same type — an element read on those is `int` at any index.

A homogeneous fixed-arity tuple (`tuple[int, int]`) collapses to its single
element type, so nothing widens there.

## Fix

Convert the expression to the expected type, or update the surrounding
annotation if the call site is the source of truth:

```ty
def main() -> None:
    let result: int = double(int("3"))  # ok
    print(result)
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/type_mismatch.md
