# tyc::tuple_index_out_of_range

Fires when a constant integer index into a fixed-arity tuple is out of
range. The tuple type carries its element count statically, so the lookup
can be flagged at check time rather than failing with `IndexError` at
runtime.

## Example

```ty
def main() -> None:
    let t: tuple[int, int] = (1, 2)
    print(t[2])  # error: index 2 is out of range for tuple of arity 2
```

## Why

Fixed-arity tuples (`tuple[A, B, C]`) are different from homogeneous tuples
(`tuple[T, ...]`): the former are essentially anonymous records whose
positions are part of the type. A constant index into one is checkable, so
it makes sense to fail at compile time instead of waiting for the runtime
crash.

## Fix

Use an index in range, or change the type to a homogeneous tuple if the
arity is genuinely dynamic:

```ty
def main() -> None:
    let t: tuple[int, int] = (1, 2)
    print(t[0])
```

See https://typhon.dev/lang/diagnostics/tuple_index_out_of_range
