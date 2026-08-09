# tyc::contains_secret_literal

Warns when a `comptime let` binding's name contains a secret-shaped keyword.
`comptime` bindings are evaluated at build time, so the emitted Python contains
the resolved env-var value as a string literal — anyone with the build output
can read the secret.

## Recognised names

The keyword table is `tyc_analyse::SECRET_NAME_KEYWORDS`, shared by this lint
and the `tyc build` scan so the two cannot drift:

`PASSPHRASE`, `API_PASSWORD`, `APIPASSWORD`, `API_SECRET`, `APISECRET`,
`API_TOKEN`, `APITOKEN`, `PASSWORD`, `SECRET`, `TOKEN`, `API_KEY`, `APIKEY`,
`PRIVKEY`, `KEY`, `PWD`, `PASS`.

The table is ordered longest-first, so a name matching more than one keyword
reports the most specific: `KEY_APIKEY` reports `APIKEY`, not `KEY`, and
`SSH_PRIVKEY` reports `PRIVKEY`.

A keyword only matches when it sits on a **word boundary** — otherwise `MONKEY`
would match `KEY` and `PASSPORT` would match `PASS`. A boundary is the start or
end of the name, an underscore, a digit, or a case junction:

| Name | Matches | Why |
|---|---|---|
| `API_KEY` | `API_KEY` | underscore-separated |
| `myTokenValue` | `TOKEN` | `lower`→`Upper` on both sides |
| `myPASSWORD123` | `PASSWORD` | digit closes the word (v1.0.0-alpha.8) |
| `foo123TOKEN` | `TOKEN` | digit opens the word (v1.0.0-alpha.8) |
| `dbPASSWORDString` | `PASSWORD` | `UPPER`→`TitleCase` junction (v1.0.0-alpha.8) |
| `MONKEY` | — | `N` before `KEY` is not a boundary |
| `PASSPORT` | — | `P` after `PASS` is not a boundary |

Because the boundary rule is what stops `PASSPORT` matching `PASS`, it also
stopped `PASSPHRASE` (v1.0.0-alpha.6) and `PRIVKEY` (v1.0.0-alpha.8), so both
have their own entries in the table.

This diagnostic is **warn-level**: a newly-flagged name warns, it never fails
the build. Silence it project-wide with `[strictness] allow-secret-comptime`.

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
