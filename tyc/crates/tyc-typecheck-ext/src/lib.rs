//! Embedded second-stage type checking via Astral's `ty`.
//!
//! `tyc-types` enforces Typhon-specific semantics (let/mut, sealed unions,
//! `Result`/`?`, interface conformance, …). Astral's [`ty`] checks the
//! standard Python typing spec against **typeshed** — the only path that
//! covers C-extension and stdlib APIs that runtime venv introspection can't
//! see. `docs/ty-integration.md` Phase 1 runs `ty` as a subprocess; this
//! crate is Phase 2: embed `ty_project` + `ruff_db` and run the checker
//! **in-process** over the emitted `.py`, skipping the process spawn and the
//! JSON/text round-trip.
//!
//! The `ty` crates pull the entire ruff/ty workspace, so they're behind the
//! `embedded` cargo feature (OFF by default). With the feature off,
//! [`is_available`] returns `false` and callers fall back to the subprocess
//! path; the standard `tyc` build never compiles `ty`.
//!
//! [`ty`]: https://github.com/astral-sh/ty

use std::path::Path;

/// Outcome of an embedded `ty` check over an emitted build directory.
pub struct EmbeddedTyResult {
    /// `ty`'s diagnostics rendered to text in the same format the `ty` CLI
    /// produces (`error[code]: msg\n --> file.py:line:col …`), so the
    /// existing `.py.map` remapper rewrites them to `.ty` source unchanged.
    pub rendered: String,
    /// Number of error/fatal diagnostics (used to fail the build/check).
    pub error_count: usize,
}

/// Whether this build was compiled with the embedded `ty` checker
/// (`--features embedded-ty`). When `false`, [`check_emitted_dir`] errors and
/// callers should use the subprocess `ty check` path instead.
pub fn is_available() -> bool {
    cfg!(feature = "embedded")
}

/// Run `ty` in-process over the emitted Python in `dir`, returning its
/// rendered diagnostics and an error count.
///
/// Mirrors the construction the `ty` CLI performs: discover project metadata
/// for `dir`, build a `ProjectDatabase`, and call `db.check()`. The directory
/// is treated as the project root, so `ty` resolves the emitted modules and
/// (when a venv/pyproject is present) the project's third-party stubs.
#[cfg(feature = "embedded")]
pub fn check_emitted_dir(dir: &Path) -> anyhow::Result<EmbeddedTyResult> {
    use ruff_db::diagnostic::{DisplayDiagnosticConfig, DisplayDiagnostics, Severity};
    use ruff_db::system::{OsSystem, SystemPathBuf};
    use ty_project::{ProjectDatabase, ProjectMetadata};

    let root = SystemPathBuf::from_path_buf(dir.to_path_buf())
        .map_err(|p| anyhow::anyhow!("emitted build path is not valid UTF-8: {:?}", p))?;
    let system = OsSystem::new(&root);
    let metadata = ProjectMetadata::discover(&root, &system)?;
    let db = ProjectDatabase::fallible(metadata, system)?;

    let diagnostics = db.check();
    let error_count = diagnostics
        .iter()
        .filter(|d| matches!(d.severity(), Severity::Error | Severity::Fatal))
        .count();

    // Render in the `ty` CLI's own format so `remap_ty_diagnostics` (which
    // rewrites `*.py:line[:col]` prefixes to `.ty` via the `.py.map`
    // sidecars) handles the output identically to the subprocess path.
    let config = DisplayDiagnosticConfig::new("ty");
    let rendered = DisplayDiagnostics::new(&db, &config, &diagnostics).to_string();

    Ok(EmbeddedTyResult {
        rendered,
        error_count,
    })
}

/// Stub used when the crate is built without the `embedded` feature — the
/// standard `tyc` build. Callers gate on [`is_available`] and never reach
/// this in practice.
#[cfg(not(feature = "embedded"))]
pub fn check_emitted_dir(_dir: &Path) -> anyhow::Result<EmbeddedTyResult> {
    anyhow::bail!(
        "embedded `ty` checker not compiled in — rebuild `tyc` with `--features embedded-ty`, \
         or use the subprocess path (`[checker] external = \"ty\"` with `ty` on PATH)"
    )
}
