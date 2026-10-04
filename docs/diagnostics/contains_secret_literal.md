# tyc::contains_secret_literal

Warns when a credential would end up as a string literal in your source or
your build output. It fires in two places, both silenced by
`[strictness] allow-secret-comptime`:

- **`tyc check` / the editor** — a binding initialised from a bare string
  literal (`API_KEY = "sk-…"`, `let DB_PASSWORD: str = "hunter2"`, or a
  `comptime let` with a literal value): the credential is committed with the
  source.
- **`tyc build`** — a `comptime let` whose string value reads
  `env("…")`. `comptime` bindings are evaluated at build time, so the emitted
  Python contains the resolved env-var value as a string literal — anyone
  with the build output can read the secret.

Both decide the same way: the **name** must name a credential and the
**value** must be a string that could be one. A non-string value
(`comptime let TOKEN_LIMIT: int = int(env("N"))`) never warns.

## When a name names a credential

The name is split into words — at underscores, digits, `camelCase` and
`ACRONYMWord` junctions — and squashed words are segmented against the
vocabulary below (`APIKEYS` → `API` `KEYS`, `dbPASSWORD` → `DB` `PASSWORD`;
`MONKEY` and `PASSPORT` do not segment, so they never match `KEY` / `PASS`).
A trailing plural `s` is accepted (`SECRETS`, `TOKENs`).

| Name shape | Examples | Result |
|---|---|---|
| a credential noun: `PASSWORD`, `PASSWD`, `PASSPHRASE`, `SECRET`, `TOKEN`, `CREDENTIAL` | `DB_PASSWORD`, `GITHUB_TOKEN`, `AWS_SECRET_ACCESS_KEY`, `SECRET_KEY_BASE`, `myTokenValue` | **clear** |
| a key noun (`KEY`, `PASS`, `PWD`, `PIN`) right after a secret qualifier (`API`, `ACCESS`, `AUTH`, `CLIENT`, `APP`, `DB`, `PRIVATE`, `SSH`, `SIGNING`, `ENCRYPTION`, `MASTER`, `ADMIN`, …) | `API_KEY`, `OPENAI_API_KEY`, `APIKEYS`, `DB_PASS`, `PRIVKEY`, `SIGNING_KEY` | **clear** |
| a key noun on its own or after another word; `DSN`, `COOKIE`, `AUTHORIZATION`, `WEBHOOK` | `KEY`, `STRIPE_KEY`, `PWD`, `DATABASE_DSN`, `SESSION_COOKIE` | **ambiguous** — the value decides |
| a key noun after a non-secret qualifier (`PUBLIC`, `PRIMARY`, `FOREIGN`, `SORT`, `PARTITION`, `CACHE`, …) | `PUBLIC_KEY`, `PRIMARY_KEY`, `SORT_KEY` | not a credential |
| a metadata word after the noun (`COUNT`, `LIMIT`, `LENGTH`, `TTL`, `TIMEOUT`, `PATH`, `FILE`, `DIR`, `URL`, `NAME`, `TYPE`, `HEADER`, `PREFIX`, `SEPARATOR`, `ID`, `ALGORITHM`, `JAR`, …) | `TOKEN_LIMIT`, `PASSWORD_MIN_LENGTH`, `CREDENTIALS_PATH`, `AUTHORIZATION_URL`, `API_KEY_HEADER`, `AWS_ACCESS_KEY_ID`, `COOKIE_JAR` | not a credential — the name describes the secret |
| a counting word anywhere (`MAX`, `MIN`, `NUM`, `N`, `TOTAL`, `COUNT`) | `MAX_TOKENS`, `tokenCount` | not a credential |

The word sets are `tyc_analyse::SECRET_NAME_KEYWORDS` and its siblings in
`tyc-analyse/src/secrets.rs`, shared by both checks so they cannot drift.

## When a value could be a credential

- A **clear** name warns on any string except a placeholder: empty or
  whitespace only, one repeated character (`"x"`, `"xxxx"`, `"****"`), or a
  template (`"<your token>"`, `"${TOKEN}"`, `"{{ token }}"`).
- An **ambiguous** name warns only on a *credential-shaped* value: a known
  token prefix (`ghp_`, `github_pat_`, `glpat-`, `sk-`, `sk_live_`, `xoxb-`,
  `AKIA`, `AIza`, `hf_`, …), a PEM private key, a URL with an embedded password
  (`postgres://app:hunter2@db/prod`), or a run of at least 20 letters, digits
  and `_-+=.` with both letters and digits and at least 3.5 bits of entropy per
  character (hex digests, base64 keys, random tokens).

## The build-time `env("…")` check

`tyc build` also reads the key of every `env("KEY")` the binding's value
depends on — directly, or through the `comptime def` functions it calls — and
uses the most secret-shaped of the binding name and those keys:

```ty
comptime let DEPLOY_CFG: str = env("AWS_SECRET_ACCESS_KEY")  # warning: the key names a secret
comptime let REGION: str = env("AWS_REGION")                   # fine
comptime let TOKEN_LIMIT: int = int(env("TOKEN_LIMIT"))        # fine: not a string
```

This diagnostic is **warn-level**: it never fails the build. Silence it
project-wide with `[strictness] allow-secret-comptime = true`.

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
