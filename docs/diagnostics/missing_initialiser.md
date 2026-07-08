# tyc::missing_initialiser

Fires when `let NAME: T` (or `mut NAME: T`) is written without an `= <expr>`
initialiser. Typhon requires every binding to have a value at declaration —
the Rust-style "declare-then-assign-later" shape is not supported.

## Example

```ty
def main() -> None:
    let x: int            # error: missing `= <expr>`
    x = 1
```

## Why

Without an initialiser, the binding is uninitialised until the first
assignment — which Python would raise as `NameError` if read before then.
Typhon avoids the entire class of bugs by requiring a value at the point of
declaration; the dedicated diagnostic fires earlier than the confusing
`tyc::immutable_assign` you'd otherwise get on the follow-up assignment.

## Fix

Initialise the binding inline:

```ty
def main() -> None:
    let x: int = 1
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/missing_initialiser.md
