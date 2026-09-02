# Security Policy

Typhon is pre-1.0 (currently the `v1.0.0-alpha.x` series). We take security reports
seriously and will respond as quickly as we can.

## Reporting a vulnerability

Please **do not** open a public issue for a security vulnerability.

Instead, use GitHub's private vulnerability reporting:
**[Report a vulnerability](https://github.com/CodeHalwell/Typhon/security/advisories/new)**
(Security → Advisories → Report a vulnerability on the repository).

Include, where you can: the affected component (compiler crate, VM, LSP,
installer, generated runtime), a minimal reproduction, and the impact you
observed. We aim to acknowledge reports within a few days.

## Trust model — please read

Typhon is a compiler and language runtime, so several ordinary commands
**execute code from the project you run them on**. Treat a cloned Typhon
project the same way you would treat any other untrusted source tree —
i.e. as code you are about to run:

- **`tyc build` runs `uv sync` by default**, installing the dependencies
  declared in `typhon.toml`. Installing a package can execute arbitrary code
  (build backends, `setup.py`, wheel hooks). Use `--no-sync` / `TYC_NO_SYNC=1`
  to skip it.
- **`tyc check`, `tyc build`, and the LSP introspect installed dependencies**
  by importing them in a subprocess to recover their type signatures.
  Importing a package runs its top-level module code. Introspection prefers a
  project-local `.venv` interpreter if one is present.
- **`tyc run` and the emitted Python execute your program** with full process
  privileges, exactly like running `python` yourself.
- **`comptime` evaluates expressions at build time** and can read environment
  variables via `env(...)`, inlining the resolved values into the build
  artifact. Do not place secrets in `comptime let`; read them at runtime via
  `os.environ[...]`. The `tyc::contains_secret_literal` lint is a best-effort
  name heuristic, not a guarantee.

None of this is unusual for a language toolchain (it mirrors `pip install` and
running the code), but it is worth stating plainly: **opening or building an
untrusted Typhon project can run that project's code.**

To inspect an untrusted project without executing any of its dependencies:

- **`TYC_NO_INTROSPECT=1`** disables venv dependency introspection entirely, in
  both the CLI and the language server. Third-party calls fall back to being
  type-checked leniently, but no project package is imported.
  (Before v1.0.0-alpha.7 the language server had its own introspection path
  that did not read this variable, so the kill-switch only covered the CLI. The
  editor now shares the CLI's interpreter discovery, so the guarantee holds on
  both surfaces.)
- **`--no-sync` / `TYC_NO_SYNC=1`** on `tyc build` skips `uv sync`, so no
  dependency is installed.

Set both when working with code you don't trust, and avoid attaching the
language server until you do.

Independently of those switches, introspection is limited to an allow-list:
the Python standard library plus the packages the project declares in
`[dependencies]` / `[dev-dependencies]`. A module that is merely *named* by a
`.ty` file — including one sitting next to it in the repository — is never
imported. Both introspection subprocesses (the signature introspection behind
`tyc check` / `tyc build` / editor diagnostics, and the editor's completion
introspection) also run in an empty scratch directory rather than the project
root, so a file named after a stdlib module cannot shadow it on `sys.path`.
That scratch directory is created fresh per process with an unpredictable
name (never adopting a pre-existing directory, and mode `0700` on Unix), so
another local user cannot pre-plant a shadowing file under the shared system
temp directory either; it is removed again when the process is done with it.
If no such private directory can be created, introspection is disabled
rather than run somewhere shared.

## Supported versions

During the alpha series, only the latest released version receives fixes.
