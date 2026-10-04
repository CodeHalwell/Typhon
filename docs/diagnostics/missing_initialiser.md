# tyc::missing_initialiser

**Not currently emitted.** The code is registered and listed by
`tyc explain --list`, but no check in the current compiler reports it.

It was written for a rule that every `let NAME: T` must carry an
`= <expr>` initialiser. That rule was relaxed in v0.7.0: a declare-only
binding is legal, and the first assignment on each path is its
initialiser.

```ty
def pick(cond: bool) -> int:
    let x: int
    if cond:
        x = 5
    else:
        x = 10
    return x        # ok: every path assigns `x` before the read
```

A read on a path that has not assigned the binding reports
`tyc::use_of_uninitialised`, and a second assignment reports
`tyc::immutable_assign`. A declaration that is never assigned or read
produces no diagnostic.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/missing_initialiser.md
