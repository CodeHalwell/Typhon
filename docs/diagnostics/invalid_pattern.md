# tyc::invalid_pattern

A `match` pattern that the Python **grammar** accepts but the CPython
**compiler** rejects. The emitted `.py` is then not valid Python at all:
`tyc build` reports success and writes a file that raises `SyntaxError`
the moment anything imports it.

Two shapes fire this:

## Two `*rest` captures in one sequence pattern

```typhon
match xs:
    case [*a, *b]:      # tyc::invalid_pattern
        return 1
```

CPython: `SyntaxError: multiple starred names in sequence pattern`. A
sequence pattern binds at most one `*rest`, because two would not have a
unique split point.

Keep the first `*rest` and match the remaining elements positionally, or
split the case:

```typhon
match xs:
    case [first, *rest]:
        return first + len(rest)
```

## The same capture name bound twice in one pattern

```typhon
match xs:
    case [a, a]:        # tyc::invalid_pattern
        return a
```

CPython: `SyntaxError: multiple assignments to name 'a' in pattern`. A
pattern binds each name once — it does not mean "these two elements are
equal". Rename one capture and compare in a guard if equality is what you
meant:

```typhon
match xs:
    case [a, b] if a == b:
        return a
```

Alternatives of a `|` pattern are checked independently, because each
binds on its own path — `case [a] | (a,):` is legal and required to bind
the same set of names on both sides.

## Why this is an error, not a warning

Every other Typhon diagnostic is about a program that *runs* and does the
wrong thing. This one is about a program that cannot be imported at all,
so there is nothing to warn about: the build would produce an artifact
that is useless by construction. It was found by the differential
harness's `compileall` gate — see `scripts/vm-differential.sh`.
