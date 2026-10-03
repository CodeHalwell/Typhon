# tyc::contains_secret_literal

Warns when a `comptime let` binding's name contains a secret-shaped keyword.
`comptime` bindings are evaluated at build time, so the emitted Python contains
the resolved env-var value as a string literal — anyone with the build output
can read the secret.

## Recognised names

The keyword table is `tyc_analyse::SECRET_NAME_KEYWORDS`, shared by this lint
and the `tyc build` scan so the two cannot drift:

`PASSPHRASE`, `AUTHORIZATION_TOKEN`, `AUTHORIZATIONTOKEN`, `AUTHORIZATION`,
`CREDENTIALS`, `CREDENTIAL`, `WEBHOOK_SECRET`, `WEBHOOKSECRET`, `WEBHOOK`,
`SIGNING`, `COOKIE`, `DB_PASSWORD`, `DBPASSWORD`, `DB_PASS`, `DBPASS`,
`DB_PWD`, `DBPWD`, `API_PASSWORD`, `APIPASSWORD`, `DB_SECRET`, `DBSECRET`,
`API_SECRET`, `APISECRET`, `APP_SECRET`, `APPSECRET`, `CLIENT_SECRET`,
`CLIENTSECRET`, `JWT_SECRET`, `JWTSECRET`, `SECRET_KEY`, `SECRETKEY`,
`PERSONAL_ACCESS_TOKEN`, `PERSONALACCESSTOKEN`, `OAUTH_TOKEN`, `OAUTHTOKEN`,
`GITHUB_TOKEN`, `GITHUBTOKEN`, `ACCESS_TOKEN`, `ACCESSTOKEN`, `AUTH_TOKEN`,
`GH_TOKEN`, `GHTOKEN`, `AUTHTOKEN`, `BEARER_TOKEN`, `BEARERTOKEN`,
`CSRF_TOKEN`, `CSRFTOKEN`, `JWT_TOKEN`, `JWTTOKEN`, `API_TOKEN`, `APITOKEN`,
`OAUTH_SECRET`, `OAUTHSECRET`, `ACCESS_PASSWORD`, `BEARER_PASSWORD`,
`CLIENT_PASSWORD`, `SECRET_PASSWORD`, `ACCESSPASSWORD`, `BEARERPASSWORD`,
`CLIENTPASSWORD`, `SECRETPASSWORD`, `AUTH_PASSWORD`, `CSRF_PASSWORD`,
`APP_PASSWORD`, `AUTHPASSWORD`, `CSRFPASSWORD`, `JWT_PASSWORD`, `APPPASSWORD`,
`JWTPASSWORD`, `PASSWORD`, `ACCESS_SECRET`, `BEARER_SECRET`, `SECRET_SECRET`,
`ACCESSSECRET`, `BEARERSECRET`, `SECRETSECRET`, `SECRET_TOKEN`, `AUTH_SECRET`,
`CSRF_SECRET`, `SECRETTOKEN`, `SECRET_PASS`, `AUTHSECRET`, `CSRFSECRET`,
`SECRETPASS`, `SECRET_PWD`, `SECRETPWD`, `SECRET`, `REFRESH_TOKEN`,
`SESSION_TOKEN`, `REFRESHTOKEN`, `SESSIONTOKEN`, `CLIENT_TOKEN`,
`CLIENTTOKEN`, `APP_TOKEN`, `APPTOKEN`, `DB_TOKEN`, `ID_TOKEN`, `DBTOKEN`,
`IDTOKEN`, `TOKEN`, `PRIVATE_KEY`, `PRIVATEKEY`, `PUBLIC_KEY`, `PUBLICKEY`,
`SSH_KEY`, `SSHKEY`, `API_KEY`, `APIKEY`, `APP_KEY`, `APPKEY`, `PRIVKEY`,
`ENCRYPTION_KEY`, `ENCRYPTIONKEY`, `ACCESS_KEY`, `BEARER_KEY`, `CLIENT_KEY`,
`MASTER_KEY`, `ACCESSKEY`, `BEARERKEY`, `CLIENTKEY`, `MASTERKEY`, `AUTH_KEY`,
`CSRF_KEY`, `AUTHKEY`, `CSRFKEY`, `JWT_KEY`, `DB_KEY`, `JWTKEY`, `DBKEY`,
`KEY`, `ACCESS_PWD`, `BEARER_PWD`, `CLIENT_PWD`, `ACCESSPWD`, `BEARERPWD`,
`CLIENTPWD`, `AUTH_PWD`, `CSRF_PWD`, `API_PWD`, `APP_PWD`, `AUTHPWD`,
`CSRFPWD`, `JWT_PWD`, `APIPWD`, `APPPWD`, `JWTPWD`, `PWD`, `ACCESS_PASS`,
`BEARER_PASS`, `CLIENT_PASS`, `ACCESSPASS`, `BEARERPASS`, `CLIENTPASS`,
`AUTH_PASS`, `CSRF_PASS`, `API_PASS`, `APP_PASS`, `AUTHPASS`, `CSRFPASS`,
`JWT_PASS`, `APIPASS`, `APPPASS`, `JWTPASS`, `PASS`, `DSN`.

The table is ordered longest-first, so a name matching more than one keyword
reports the most specific: `KEY_APIKEY` reports `APIKEY`, not `KEY`, and
`SSH_PRIVKEY` reports `PRIVKEY`.

A keyword only matches when it sits on a **word boundary** — otherwise `MONKEY`
would match `KEY` and `PASSPORT` would match `PASS`. A boundary is the start or
end of the name, an underscore, a digit, or a case junction in either
direction:

| Name | Matches | Why |
|---|---|---|
| `API_KEY` | `API_KEY` | underscore-separated |
| `myTokenValue` | `TOKEN` | `lower`→`Upper` on both sides |
| `myPASSWORD123` | `PASSWORD` | digit closes the word (v1.0.0-alpha.8) |
| `foo123TOKEN` | `TOKEN` | digit opens the word (v1.0.0-alpha.8) |
| `dbPASSWORDString` | `DBPASSWORD` | `UPPER`→`TitleCase` junction (v1.0.0-alpha.8); reported as `DBPASSWORD` since that entry joined the table in v1.0.0-alpha.9 |
| `dbPASSWORDstring` | `DBPASSWORD` | `UPPER`→`lower` closes the word (v1.0.0-alpha.9) |
| `TOKENs` | `TOKEN` | same rule — a lowercase letter after an uppercase keyword character (v1.0.0-alpha.9) |
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
