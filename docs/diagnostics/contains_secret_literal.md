# tyc::contains_secret_literal

Warns when a `comptime let` binding's name matches a secret-suffix heuristic
(`*KEY`, `*TOKEN`, `*PASSWORD`, `*PASSPHRASE`, `*SECRET`, `*PASS`, `*PWD`). `comptime`
bindings are evaluated at build time, so the emitted Python contains the
resolved env-var value as a string literal — anyone with the build output
can read the secret.

## Example

```ty
comptime let API_KEY: str = env("MY_API_KEY")  # warning: secret inlined
```

After build the emitted Python becomes literally `API_KEY = "sk-…"`.

## Why

`comptime` exists for build-time constants (feature flags, banner strings,
schema versions). Inlining a secret turns the build artifact into a
plaintext credential store, which leaks the moment the artifact is shared or
checked into version control.

## Fix

Read the env var at runtime instead, so the secret stays in the deployment
environment and never lands in the build output:

```ty
import os

let API_KEY: str = os.environ["MY_API_KEY"]
```

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/contains_secret_literal.md
