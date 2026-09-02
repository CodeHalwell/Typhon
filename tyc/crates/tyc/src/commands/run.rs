//! `tyc run` — execute a Typhon program.
//!
//! Default mode: the in-process tree-walking VM from `tyc-vm`. No `.py` is
//! ever written; the runtime is the same Rust binary that hosts the
//! compiler. This is the path you want for scripts, tests, and any program
//! that stays inside Typhon's native semantics.
//!
//! `--compile` mode: the legacy "build then exec CPython" path. Use this
//! when the program reaches into CPython libraries (`numpy`, `requests`,
//! `pydantic`, …) that the VM cannot evaluate natively, or when you want
//! the exact output `tyc build` would produce.
//!
//! When `--compile` is passed:
//!
//! * **Persistent (default).** Builds into the configured `out` dir
//!   (default `build/`).  Subsequent runs hit the incremental Salsa
//!   cache, and `.py.map` sidecars stay on disk so a post-crash
//!   `tyc trace` can map frames back to `.ty`.
//! * **`--temp` (`-t`).** Builds into a fresh `tempfile::tempdir()` that
//!   is removed when the process exits.  The "tyx in-memory" mode — no
//!   project pollution, ideal for quick one-shot iteration.  Trades the
//!   incremental cache and on-disk source map for a clean tree.
//!
//! Exit-code semantics: `tyc run` exits with the program's own exit code
//! (via `process::exit`) so shell pipelines see the child's status verbatim.
//! Build failures, parse errors, and spawn failures surface as normal miette
//! errors with exit code 1.

use std::path::PathBuf;
use std::process::Command;

use clap::Args;
use miette::{miette, Result};
use tempfile::TempDir;

use crate::commands::build::{self, BuildArgs};
use crate::commands::check::{self, CheckArgs};
use crate::config::TyphonConfig;

/// The `--python` default. Recognised as "not explicitly chosen" by
/// [`resolve_interpreter`], which then prefers the project's own venv or a
/// `python3.<minor>` matching `[python] target`.
const DEFAULT_PYTHON: &str = "python3";

/// Arguments for `tyc run`.
#[derive(Args, Debug)]
pub struct RunArgs {
    /// Project directory, or a single `.ty` source file when using the VM
    /// (the default). For `--compile` mode this is always the project
    /// directory.
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Build then exec CPython instead of running the in-process VM.
    /// Use this when your program imports CPython libraries the VM
    /// doesn't speak natively (numpy, requests, …).
    #[arg(long, alias = "no-vm")]
    pub compile: bool,

    /// Entry-point `.py` (relative to the build dir) for `--compile` mode.
    /// Defaults to `main.py`. Requires `--compile`.
    #[arg(
        long,
        value_name = "FILE",
        default_value = "main.py",
        requires = "compile"
    )]
    pub entry: PathBuf,

    /// Python interpreter to use for the compiled path (defaults to the
    /// project's `.venv`, else `python3.<minor>` for `[python] target`,
    /// else `python3`). Applies to `--compile` and to the automatic
    /// fallback the VM takes for an unmodelled import.
    /// `None` when the flag was not given: `--python python3` has to mean
    /// *that* interpreter, not "fall back to the venv", so the default
    /// cannot be baked in as a string.
    #[arg(long, value_name = "PATH")]
    pub python: Option<String>,

    /// Build into a temporary directory that is deleted when the process
    /// exits, instead of the configured `out` dir. No build artifacts
    /// persist on disk — the "tyx in-memory" mode. Implies a fresh build
    /// every invocation. Requires `--compile`.
    #[arg(long, short = 't', conflicts_with = "no_build", requires = "compile")]
    pub temp: bool,

    /// Skip rebuilding; assume the `build/` directory is already current.
    /// Incompatible with `--temp`. Requires `--compile`.
    #[arg(long, requires = "compile")]
    pub no_build: bool,

    /// Never fall back to the compiled path: fail with the VM's own
    /// `ModuleNotFoundError` when the program imports a module the VM does
    /// not model, instead of transparently building and running it under
    /// CPython. Use this to keep a run hermetic, or to find out whether the
    /// VM covers a program's imports.
    #[arg(long, conflicts_with = "compile")]
    pub no_fallback: bool,

    /// Extra arguments forwarded to the program after `--`.
    #[arg(last = true, value_name = "ARGS")]
    pub script_args: Vec<String>,
}

pub fn run(args: RunArgs) -> Result<()> {
    if !args.compile {
        return run_vm(args);
    }
    let mut args = args;
    // `tyc run --compile script.ty` — synthesise a throwaway project
    // around the file so the scripting flow works without `tyc init`:
    // copy the script to `<tmp>/src/main.ty`, write a minimal
    // `typhon.toml`, and continue exactly like a project invocation in
    // `--temp` mode (nothing persists). FINDINGS #40 originally rejected
    // this shape outright; the scaffold makes it just work.
    let mut _scaffold_guard: Option<TempDir> = None;
    let mut scaffold_no_sync = false;
    let mut source_label: Option<String> = None;
    if args.path.is_file() {
        if args.no_build {
            return Err(miette!(
                "--no-build needs an existing project build directory and \
                 cannot be combined with a single-file path; drop --no-build \
                 (or run inside a `tyc init` project)."
            ));
        }
        let src_file = args
            .path
            .canonicalize()
            .map_err(|e| miette!("cannot resolve path '{}': {}", args.path.display(), e))?;
        let scaffold = tempfile::Builder::new()
            .prefix("tyc-script-")
            .tempdir()
            .map_err(|e| miette!("cannot create temp scaffold: {e}"))?;
        std::fs::create_dir_all(scaffold.path().join("src"))
            .map_err(|e| miette!("cannot create temp scaffold src/: {e}"))?;
        std::fs::copy(&src_file, scaffold.path().join("src").join("main.ty"))
            .map_err(|e| miette!("cannot stage '{}': {}", src_file.display(), e))?;
        // The modules the script imports from beside itself come too — the
        // VM loads them on demand, so a compiled run that omitted them would
        // fail on an import the VM resolves.
        for sibling in sibling_modules(&src_file) {
            let Some(name) = sibling.file_name() else {
                continue;
            };
            std::fs::copy(&sibling, scaffold.path().join("src").join(name))
                .map_err(|e| miette!("cannot stage '{}': {}", sibling.display(), e))?;
        }
        // An adjacent package comes too, shape intact.
        for package in sibling_packages(&src_file) {
            let Some(name) = package.file_name() else {
                continue;
            };
            stage_package(&package, &scaffold.path().join("src").join(name))?;
        }
        let name = src_file
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("script")
            .replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "-");
        // `traceback-remap` is default-off for projects (it costs an import
        // in the entry module), but a staged script has no build directory
        // the user can inspect: without it an uncaught exception's traceback
        // names `/tmp/tyc-script-…/build/main.py` lines they cannot read.
        std::fs::write(
            scaffold.path().join("typhon.toml"),
            format!(
                "[project]\nname = \"{name}\"\nversion = \"0.0.0\"\nsrc = \"src\"\nout = \"build\"\n\n[python]\ntarget = \"3.13\"\n\n[emit]\ntraceback-remap = true\n"
            ),
        )
        .map_err(|e| miette!("cannot write temp typhon.toml: {e}"))?;
        // Diagnostics from the build must name the file the user ran, not
        // the staged copy inside the scaffold.
        source_label = Some(args.path.display().to_string());
        args.path = scaffold.path().to_path_buf();
        args.temp = true;
        scaffold_no_sync = true;
        _scaffold_guard = Some(scaffold);
    }
    // 1. Decide where build outputs go.  In `--temp` mode we own a
    //    TempDir guard whose Drop removes the directory; we keep it
    //    alive across the child process by binding it to a local.
    //
    //    For a single-file scaffold the output has to live *inside* that
    //    scaffold: `tyc build` refuses to write outside the project root it
    //    was handed, so a second, sibling temp directory made
    //    `tyc run --compile script.ty` fail outright with "refusing to
    //    write … outside the project root". The scaffold is itself a
    //    TempDir, so nothing persists either way.
    let (out_dir, _tmp_guard): (PathBuf, Option<TempDir>) = if scaffold_no_sync {
        (args.path.join("build"), None)
    } else if args.temp {
        let tmp = tempfile::Builder::new()
            .prefix("tyc-run-")
            .tempdir()
            .map_err(|e| miette!("cannot create temp directory: {e}"))?;
        (tmp.path().to_path_buf(), Some(tmp))
    } else {
        // Resolve the persistent `out` dir the same way `tyc build` does
        // so the entry-point lookup matches what was just emitted.
        let project_root = args
            .path
            .canonicalize()
            .map_err(|e| miette!("cannot resolve path '{}': {}", args.path.display(), e))?;
        let (config_dir, config) = match TyphonConfig::load(&project_root) {
            Ok(Some((toml_path, cfg))) => {
                let dir = toml_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| project_root.clone());
                (dir, cfg)
            }
            Ok(None) => (project_root.clone(), TyphonConfig::default()),
            Err(e) => return Err(miette!("{e}")),
        };
        (config_dir.join(&config.project.out), None)
    };

    // 2. Build the project unless --no-build was passed.  When --temp is
    //    set we always build (clap's conflicts_with already rejected
    //    --no-build + --temp).  Pass an explicit `out` only for --temp;
    //    leaving it None lets `tyc build` pick its own configured dir.
    if !args.no_build {
        build::run(BuildArgs {
            path: args.path.clone(),
            out: if args.temp {
                Some(out_dir.clone())
            } else {
                None
            },
            no_format: false,
            check: false,
            // Single-file scaffolds have no dependencies — skip `uv sync`.
            no_sync: scaffold_no_sync,
            with_ty: false,
            optimise: false,
            source_label: source_label.clone(),
        })?;
    }

    let entry = out_dir.join(&args.entry);
    if !entry.exists() {
        return Err(miette!(
            "entry-point '{}' does not exist; pass --entry or drop --no-build",
            entry.display()
        ));
    }

    // The project root, for locating a `.venv` and reading `[python] target`
    // when picking the interpreter below.
    let project_root_for_python = args
        .path
        .canonicalize()
        .unwrap_or_else(|_| args.path.clone());

    // 3. Decide between two spawn shapes:
    //    (a) script mode: `python build/main.py` — works for single-file
    //        projects where the entry has no relative imports.
    //    (b) module mode: `python -m <pkg>.<module>` — required when the
    //        entry uses `from .x import y` syntax, since Python only
    //        resolves a package's relative imports when the entry is
    //        loaded *as a submodule of that package*. We pick this shape
    //        whenever the build directory holds an `__init__.py` (i.e.
    //        the project is laid out as a package).
    //
    //    For module mode we set the cwd to the build dir's parent so
    //    Python picks up the package automatically; we also stash the
    //    parent in `PYTHONPATH` for good measure.
    let interpreter = resolve_interpreter(&args, &project_root_for_python);
    let mut cmd = Command::new(&interpreter);
    // Decide between script mode (`python entry.py`) and module mode
    // (`python -m pkg.sub.entry`). We use module mode whenever the
    // entry's immediate parent directory has an `__init__.py` — i.e.
    // it's part of a Python package. Walk up from there to find the
    // package root (the first ancestor whose own parent does NOT have
    // an `__init__.py`) so `<out>/mypkg/__init__.py` works as well as
    // the flat `<out>/__init__.py` layout. Review thread copilot on
    // PR #147.
    let module_invocation = entry
        .parent()
        .filter(|p| p.join("__init__.py").exists())
        .map(|entry_parent| {
            let mut segments: Vec<String> = vec![entry
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("main")
                .to_owned()];
            let mut current: Option<&std::path::Path> = Some(entry_parent);
            while let Some(dir) = current {
                if !dir.join("__init__.py").exists() {
                    break;
                }
                let name = match dir.file_name().and_then(|s| s.to_str()) {
                    Some(n) => n.to_owned(),
                    None => break,
                };
                segments.push(name);
                current = dir.parent();
            }
            // `segments` was built leaf → root; the cwd must sit
            // immediately above the topmost package so `-m pkg.sub.x`
            // resolves it.
            let workdir = current
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            segments.reverse();
            (workdir, segments.join("."))
        });
    if let Some((workdir, module_path)) = module_invocation {
        cmd.current_dir(&workdir);
        cmd.arg("-m");
        cmd.arg(module_path);
    } else {
        cmd.arg(&entry);
    }
    cmd.args(&args.script_args);

    let status = cmd
        .status()
        .map_err(|e| miette!("cannot spawn '{}': {e}", interpreter))?;

    // 4. Propagate the child's exit code verbatim, but drop the TempDir
    //    guard first so its Drop runs — process::exit skips destructors.
    let code = status.code().unwrap_or(1);
    drop(_tmp_guard);
    // `process::exit` skips destructors — release the single-file
    // scaffold explicitly too, or every `tyc run --compile script.ty`
    // leaks a temp directory.
    drop(_scaffold_guard);
    std::process::exit(code);
}

/// The interpreter to exec the built program with.
///
/// An explicit `--python` always wins. Otherwise the default `python3` is
/// only a last resort: a project targeting 3.13+ must not be run by
/// whatever `python3` happens to be first on `PATH` (3.11 on many
/// systems), because the emitted code uses PEP 695 syntax that older
/// interpreters reject with a `SyntaxError`. Prefer, in order, the
/// project's own `.venv` (which `uv sync` provisions for the configured
/// target), then `python3.<minor>` for that target, then `python3`.
fn resolve_interpreter(args: &RunArgs, project_root: &std::path::Path) -> String {
    if let Some(explicit) = &args.python {
        return explicit.clone();
    }
    // A Windows virtualenv puts its interpreter somewhere else entirely.
    let venv = if cfg!(windows) {
        project_root
            .join(".venv")
            .join("Scripts")
            .join("python.exe")
    } else {
        project_root.join(".venv").join("bin").join("python")
    };
    if venv.exists() {
        return venv.to_string_lossy().into_owned();
    }
    let target = TyphonConfig::load(project_root)
        .ok()
        .flatten()
        .map(|(_, cfg)| cfg.python.target)
        .unwrap_or_default();
    // `3.13` / `3.14t` → `python3.13` / `python3.14`.
    let minor: String = target
        .split('.')
        .nth(1)
        .unwrap_or_default()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if !minor.is_empty() {
        let versioned = format!("python3.{minor}");
        if which_on_path(&versioned) {
            return versioned;
        }
    }
    DEFAULT_PYTHON.to_owned()
}

/// Whether `name` resolves to an executable on `PATH`.
fn which_on_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    // On Windows an executable is only found with one of `PATHEXT`'s
    // suffixes appended: `python3.13` never matches `python3.13.exe`.
    let mut names: Vec<String> = vec![name.to_owned()];
    if cfg!(windows) {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
        for ext in exts.split(';').filter(|e| !e.is_empty()) {
            names.push(format!("{name}{ext}"));
        }
    }
    std::env::split_paths(&paths).any(|dir| names.iter().any(|n| dir.join(n).is_file()))
}

/// Default execution path — the in-process tree-walking VM. Resolves the
/// entry-point source file from `args.path` (a `.ty` file directly, or the
/// project root containing `src/main.ty`), then evaluates it. The script's
/// `sys.argv` is populated from `args.script_args`, with `argv[0]` set to
/// the entry-point path.
///
/// Before stepping the VM we run the same static checker `tyc check`
/// would run, so Typhon-specific diagnostics (`unknown_name`,
/// `pattern_shadows_outer`, `unsafe_value_leak`, `blocking_in_async`, …)
/// surface consistently with `tyc build` instead of crashing the VM
/// with a runtime `NameError` later. The `TYC_SKIP_CHECK=1` env var
/// disables this for the rare case where you want the legacy
/// run-only-the-VM behaviour (mostly: probing the VM against
/// deliberately-broken inputs in stress harnesses).
fn run_vm(args: RunArgs) -> Result<()> {
    let entry = resolve_vm_entry(&args.path)?;
    // `tyc run` is contractually a drop-in for `tyc build` + CPython, and
    // the VM models a documented subset of the stdlib. Rather than dying
    // with `ModuleNotFoundError` on a program the compiled path runs fine,
    // take that path automatically. The decision is made *before* the
    // pre-run check and before any user code runs, so a program never
    // half-executes and then restarts, and its diagnostics are reported
    // once (by the build) rather than by both paths.
    if !args.no_fallback {
        if let Some(missing) = unmodelled_imports(&args.path, &entry) {
            eprintln!(
                "note: `{}` {} not modelled by the in-process VM — running via \
                 `--compile` (build + CPython) so the program behaves as it \
                 does after `tyc build`. Pass `--no-fallback` to require the VM.",
                missing.join("`, `"),
                if missing.len() == 1 { "is" } else { "are" },
            );
            let mut compiled = args;
            compiled.compile = true;
            return run(compiled);
        }
    }
    if std::env::var_os("TYC_SKIP_CHECK").is_none() {
        check::run(CheckArgs {
            paths: vm_check_scope(&args.path, &entry),
            stubs: false,
            quiet_success: true,
            with_ty: false,
        })?;
    }
    let code = tyc_vm::run_file(&entry, &args.script_args).map_err(|e| miette!("{e}"))?;
    std::process::exit(code);
}

/// Modules the program imports that the VM cannot serve, or `None` when
/// every import is either VM-modelled or a module of this project.
///
/// Scans the same file set the pre-run check covers, so an unmodelled
/// import in a sibling module is caught before the entry starts running.
fn unmodelled_imports(path: &std::path::Path, entry: &std::path::Path) -> Option<Vec<String>> {
    let scope = vm_check_scope(path, entry);
    let mut files: Vec<PathBuf> = Vec::new();
    for p in scope {
        if p.is_dir() {
            files.extend(crate::commands::util::collect_ty_files(&p).unwrap_or_default());
        } else {
            files.push(p);
        }
    }
    // A bare file outside a project has only itself in scope, but the VM
    // still loads `.ty` modules sitting beside it. Without them here, a
    // `from helper import …` looked like an unmodelled external module and
    // sent a perfectly runnable program down the compiled path — which then
    // failed, since the scaffold stages only the entry.
    for sibling in sibling_modules(entry) {
        if !files.contains(&sibling) {
            files.push(sibling);
        }
    }
    // A sibling `.ty` is a module of this project, not an external import.
    let project_roots: std::collections::HashSet<String> = files
        .iter()
        .filter_map(|f| f.file_stem().and_then(|s| s.to_str()).map(str::to_owned))
        .collect();
    // An adjacent *package* is local too, but the VM will load every module
    // in it — so its files are scanned as well. They are added after
    // `project_roots` is computed, so a submodule's name does not start
    // standing in for a top-level import.
    for package in sibling_packages(entry) {
        for file in crate::commands::util::collect_ty_files(&package).unwrap_or_default() {
            if !files.contains(&file) {
                files.push(file);
            }
        }
    }
    let mut missing: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for file in &files {
        let Some(roots) = imported_roots(file) else {
            // Falling back to the compiled path is the safe answer for a
            // file this scan cannot read: the build reports the parse
            // error properly, and the VM never starts on an unknown import.
            missing.insert(format!("<unparsed {}>", file.display()));
            continue;
        };
        for root in roots {
            if tyc_vm::models_module(&root) || project_roots.contains(&root) {
                continue;
            }
            // A directory next to the entry is a project package.
            if entry
                .parent()
                .is_some_and(|dir| dir.join(&root).join("__init__.ty").exists())
            {
                continue;
            }
            missing.insert(root);
        }
    }
    if missing.is_empty() {
        None
    } else {
        Some(missing.into_iter().collect())
    }
}

/// The `.ty` files beside `entry` that the program reaches by import,
/// transitively — the modules the VM would load on demand.
///
/// Only plain siblings resolve here (`from helper import x` →
/// `<entry dir>/helper.ty`); a package directory is recognised separately by
/// its `__init__.ty`, and a project invocation has already widened to the
/// whole `src` tree.
fn sibling_modules(entry: &std::path::Path) -> Vec<PathBuf> {
    let Some(dir) = entry.parent() else {
        return Vec::new();
    };
    let mut seen: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    let mut queue: Vec<PathBuf> = vec![entry.to_path_buf()];
    while let Some(file) = queue.pop() {
        for root in imported_roots(&file).unwrap_or_default() {
            let candidate = dir.join(format!("{root}.ty"));
            if candidate.is_file() && seen.insert(candidate.clone()) {
                queue.push(candidate);
            }
        }
    }
    seen.into_iter().collect()
}

/// The import roots `file` names, or `None` when it cannot be read or parsed.
///
/// Runs the same expansion chain the VM's entry point does, so a file using
/// `?`, `|>` or `gather:` is read rather than silently skipped.
fn imported_roots(file: &std::path::Path) -> Option<std::collections::BTreeSet<String>> {
    let text = std::fs::read_to_string(file).ok()?;
    let expanded = tyc_syntax::preprocess::expand_all(&text);
    let prep = tyc_syntax::preprocess::preprocess(&expanded);
    let parsed = tyc_syntax::parse_module(&prep.python_source).ok()?;
    Some(tyc_resolve::collect_imported_roots(&parsed.into_syntax()))
}

/// Package directories beside `entry` that the program reaches by import — a
/// `<root>/__init__.ty` next to it, or next to one of its siblings.
///
/// The VM loads these on demand exactly as it loads a plain sibling, so the
/// unmodelled-import scan has to look inside them (a package importing
/// `sqlite3` must send the program down the compiled path, not into a VM
/// that dies on it) and the scaffold has to stage them.
fn sibling_packages(entry: &std::path::Path) -> Vec<PathBuf> {
    let Some(dir) = entry.parent() else {
        return Vec::new();
    };
    let mut sources: Vec<PathBuf> = vec![entry.to_path_buf()];
    sources.extend(sibling_modules(entry));
    let mut packages: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    for file in sources {
        for root in imported_roots(&file).unwrap_or_default() {
            let candidate = dir.join(&root);
            if candidate.join("__init__.ty").is_file() {
                packages.insert(candidate);
            }
        }
    }
    packages.into_iter().collect()
}

/// Copy a package's `.ty` sources into the temp scaffold, preserving shape.
fn stage_package(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(|e| miette!("cannot create '{}': {e}", to.display()))?;
    let entries =
        std::fs::read_dir(from).map_err(|e| miette!("cannot read '{}': {e}", from.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            stage_package(&path, &target)?;
        } else if path.extension().is_some_and(|e| e == "ty") {
            std::fs::copy(&path, &target)
                .map_err(|e| miette!("cannot stage '{}': {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Decide which path(s) the pre-run `tyc check` should cover.
///
/// The VM loads sibling modules from the project source root, so the
/// gating check must resolve the *same* module graph the VM will execute.
/// Checking the entry file in isolation made every `from sibling import …`
/// trip `tyc::unknown_module`, and the unresolved imports cascaded into
/// false errors that blocked execution (e.g. an exhaustive `match` over an
/// imported sealed union degrading to `tyc::missing_return`) — even when
/// `tyc check src/` was green.
///
/// When the entry lives inside the configured `[project] src` tree we check
/// that whole tree (matching `tyc check src/`). For a bare single-file
/// invocation with no surrounding project — or an entry outside the src
/// tree — we keep checking just the entry file.
fn vm_check_scope(path: &std::path::Path, entry: &std::path::Path) -> Vec<PathBuf> {
    let probe = if path.is_dir() {
        path.canonicalize().ok()
    } else {
        path.canonicalize()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    };
    if let Some(dir) = probe {
        if let Ok(Some((toml_path, cfg))) = TyphonConfig::load(&dir) {
            if let Some(root) = toml_path.parent() {
                let src = root.join(&cfg.project.src);
                // Only widen to the src tree when the entry is genuinely
                // inside it; otherwise the entry's own diagnostics would
                // be skipped entirely.
                if let (Ok(src_c), Ok(entry_c)) = (src.canonicalize(), entry.canonicalize()) {
                    if entry_c.starts_with(&src_c) {
                        return vec![src_c];
                    }
                }
            }
        }
    }
    vec![entry.to_path_buf()]
}

/// Resolve a Typhon entry point from a user-supplied path. If the path is a
/// file, use it directly. Otherwise treat it as a project directory and look
/// up `[project] src` in `typhon.toml` (defaulting to `src/`) to find
/// `main.ty`. `.dty` files are stubs, not runnable code, so we never pick one.
fn resolve_vm_entry(path: &std::path::Path) -> Result<PathBuf> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    // Consult typhon.toml to honour a custom `[project] src` directory.
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let src_dir = match TyphonConfig::load(&canonical) {
        Ok(Some((toml_path, cfg))) => {
            let project_root = toml_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| canonical.clone());
            project_root.join(&cfg.project.src)
        }
        _ => canonical.join("src"),
    };
    let candidates = [src_dir.join("main.ty"), path.join("main.ty")];
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    Err(miette!(
        "no Typhon entry point found under '{}': pass a .ty file directly, \
         or run inside a project whose [project] src directory contains main.ty",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(clap::Parser, Debug)]
    struct WrapRun {
        #[command(flatten)]
        args: RunArgs,
    }

    #[test]
    fn vm_check_scope_widens_to_project_src_tree() {
        // A project-directory invocation must check the whole `src` tree so
        // sibling imports resolve (the bug: checking `main.ty` alone fired a
        // false `unknown_module` + knock-on errors that blocked `tyc run`).
        let project = tempfile::tempdir().unwrap();
        let src = project.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            project.path().join("typhon.toml"),
            "[project]\nname = \"u\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n",
        )
        .unwrap();
        std::fs::write(src.join("main.ty"), "def main() -> None:\n    pass\n").unwrap();
        let entry = src.join("main.ty");

        // Directory invocation → check the src tree.
        let scope = vm_check_scope(project.path(), &entry);
        assert_eq!(scope, vec![src.canonicalize().unwrap()]);

        // Passing the entry file (inside the project) widens too.
        let scope = vm_check_scope(&entry, &entry);
        assert_eq!(scope, vec![src.canonicalize().unwrap()]);
    }

    #[test]
    fn vm_check_scope_falls_back_to_entry_for_bare_file() {
        // A single `.ty` with no surrounding project must check just itself
        // (no regression from the old behaviour).
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("scratch.ty");
        std::fs::write(&entry, "let x: int = 1\n").unwrap();
        let scope = vm_check_scope(&entry, &entry);
        assert_eq!(scope, vec![entry.clone()]);
    }

    #[test]
    fn args_default_to_python3_and_main_py() {
        let parsed = <WrapRun as clap::Parser>::try_parse_from(["run"]).unwrap();
        assert_eq!(parsed.args.python, None);
        assert_eq!(
            <WrapRun as clap::Parser>::try_parse_from(["run", "--python", "python3"])
                .unwrap()
                .args
                .python
                .as_deref(),
            Some("python3")
        );
        assert_eq!(parsed.args.entry, PathBuf::from("main.py"));
        assert_eq!(parsed.args.path, PathBuf::from("."));
        assert!(parsed.args.script_args.is_empty());
        assert!(!parsed.args.no_build);
        assert!(!parsed.args.temp);
    }

    #[test]
    fn script_args_pass_through_after_double_dash() {
        let parsed = <WrapRun as clap::Parser>::try_parse_from([
            "run",
            "--",
            "--flag",
            "value",
            "positional",
        ])
        .unwrap();
        assert_eq!(
            parsed.args.script_args,
            vec!["--flag".to_string(), "value".into(), "positional".into()]
        );
    }

    #[test]
    fn temp_flag_parses_with_short_alias() {
        // --temp is a compile-mode flag and requires --compile.
        let parsed = <WrapRun as clap::Parser>::try_parse_from(["run", "--compile", "-t"]).unwrap();
        assert!(parsed.args.temp);

        let parsed =
            <WrapRun as clap::Parser>::try_parse_from(["run", "--compile", "--temp"]).unwrap();
        assert!(parsed.args.temp);
    }

    #[test]
    fn temp_requires_compile() {
        let err = <WrapRun as clap::Parser>::try_parse_from(["run", "--temp"])
            .expect_err("--temp without --compile must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("requires") || msg.contains("required"),
            "expected a 'requires' error, got: {msg}"
        );
    }

    #[test]
    fn temp_and_no_build_are_mutually_exclusive() {
        let err =
            <WrapRun as clap::Parser>::try_parse_from(["run", "--compile", "--temp", "--no-build"])
                .expect_err("--temp + --no-build must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("cannot be used with") || msg.contains("conflicts"),
            "expected a conflict error, got: {msg}"
        );
    }

    #[test]
    fn compile_mode_single_file_with_no_build_is_rejected() {
        // `tyc run --compile script.ty` now scaffolds a throwaway project
        // (the scripting flow), but `--no-build` makes no sense against a
        // fresh scaffold — verify the combination still fails with an
        // actionable message (and not the old 'foo.ty/src' build error).
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("foo.ty");
        std::fs::write(&file, "let x: int = 1\n").unwrap();
        let args = RunArgs {
            path: file,
            compile: true,
            entry: PathBuf::from("main.py"),
            python: None,
            temp: false,
            no_build: true,
            no_fallback: false,
            script_args: vec![],
        };
        let err = run(args).expect_err("--compile --no-build on a single file must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("--no-build"),
            "error should mention --no-build, got: {msg}"
        );
        assert!(
            !msg.contains("source directory"),
            "old build-side message should no longer appear: {msg}"
        );
    }

    #[test]
    fn missing_entry_returns_error_when_no_build() {
        let tmp = tempfile::tempdir().unwrap();
        let args = RunArgs {
            path: tmp.path().to_path_buf(),
            compile: true,
            entry: PathBuf::from("main.py"),
            python: None,
            temp: false,
            no_build: true,
            no_fallback: false,
            script_args: vec![],
        };
        let err = run(args).expect_err("missing entry must fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("does not exist"),
            "expected 'does not exist' error, got: {msg}"
        );
    }
}
