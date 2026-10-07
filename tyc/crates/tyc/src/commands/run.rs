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
    let (out_dir, _tmp_guard, project_root_for_python): (PathBuf, Option<TempDir>, PathBuf) =
        if scaffold_no_sync {
            (args.path.join("build"), None, args.path.clone())
        } else if args.temp {
            let tmp = tempfile::Builder::new()
                .prefix("tyc-run-")
                .tempdir()
                .map_err(|e| miette!("cannot create temp directory: {e}"))?;
            let root = args
                .path
                .canonicalize()
                .unwrap_or_else(|_| args.path.clone());
            let root = match TyphonConfig::load(&root) {
                Ok(Some((toml_path, _))) => {
                    toml_path.parent().map(|p| p.to_path_buf()).unwrap_or(root)
                }
                _ => root,
            };
            (tmp.path().to_path_buf(), Some(tmp), root)
        } else {
            // Resolve the persistent `out` dir the same way `tyc build` does
            // so the entry-point lookup matches what was just emitted.
            let invocation_root = args
                .path
                .canonicalize()
                .map_err(|e| miette!("cannot resolve path '{}': {}", args.path.display(), e))?;
            let (config_dir, config) = match TyphonConfig::load(&invocation_root) {
                Ok(Some((toml_path, cfg))) => {
                    let dir = toml_path
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| invocation_root.clone());
                    (dir, cfg)
                }
                Ok(None) => (invocation_root.clone(), TyphonConfig::default()),
                Err(e) => return Err(miette!("{e}")),
            };
            let out = config_dir.join(&config.project.out);
            (out, None, config_dir)
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
        if let Some(missing) = unmodelled_references(&args.path, &entry) {
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

/// Modules the program imports, and `module.attr` names it reads, that the
/// VM cannot serve — or `None` when every import is either VM-modelled or a
/// module of this project, and every attribute read on a modelled module is
/// one its VM model exports (or one the program assigns itself).
///
/// Scans the same file set the pre-run check covers, so an unmodelled
/// import in a sibling module is caught before the entry starts running.
/// The attribute half closes the gap the import scan leaves: a missing
/// *attribute* of a modelled module (`math.nextafter` before the VM had
/// it) is otherwise a silent-until-runtime `AttributeError` on a program
/// the compiled path runs fine.
fn unmodelled_references(path: &std::path::Path, entry: &std::path::Path) -> Option<Vec<String>> {
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
    let mut exports = ModelledExports::default();
    for file in &files {
        let Some(module) = parse_for_scan(file) else {
            // Falling back to the compiled path is the safe answer for a
            // file this scan cannot read: the build reports the parse
            // error properly, and the VM never starts on an unknown import.
            missing.insert(format!("<unparsed {}>", file.display()));
            continue;
        };
        missing.extend(unmodelled_attribute_references(&module, &mut exports));
        if tyc_vm::module_has_eager_generator(&module) {
            missing.insert("a generator whose yield the VM cannot suspend".into());
        }
        for root in tyc_resolve::collect_imported_roots(&module) {
            if root == "re" {
                missing.insert("re (Python regular-expression semantics)".into());
                continue;
            }
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
fn imported_roots(file: &std::path::Path) -> Option<std::collections::BTreeSet<String>> {
    parse_for_scan(file).map(|module| tyc_resolve::collect_imported_roots(&module))
}

/// `file` parsed for the pre-run scans, or `None` when it cannot be read or
/// parsed.
///
/// Runs the same expansion chain the VM's entry point does, so a file using
/// `?`, `|>` or `gather:` is read rather than silently skipped.
fn parse_for_scan(file: &std::path::Path) -> Option<ruff_python_ast::ModModule> {
    let text = std::fs::read_to_string(file).ok()?;
    let expanded = tyc_syntax::preprocess::expand_all(&text);
    let prep = tyc_syntax::preprocess::preprocess(&expanded);
    let parsed = tyc_syntax::parse_module(&prep.python_source).ok()?;
    Some(parsed.into_syntax())
}

/// What each VM-modelled module exports, asked of the VM itself and cached
/// for one scan (instantiating a shim module is not free).
#[derive(Default)]
struct ModelledExports {
    probe: Option<tyc_vm::Interpreter>,
    cache: std::collections::HashMap<String, Option<std::collections::BTreeSet<String>>>,
}

impl ModelledExports {
    /// The names `module` (possibly dotted) exports under the VM, or `None`
    /// when the VM does not serve it as a module.
    fn names(&mut self, module: &str) -> Option<&std::collections::BTreeSet<String>> {
        if !self.cache.contains_key(module) {
            let probe = self.probe.get_or_insert_with(tyc_vm::Interpreter::new);
            let names = tyc_vm::modelled_module_exports(probe, module);
            self.cache.insert(module.to_owned(), names);
        }
        self.cache.get(module).and_then(|names| names.as_ref())
    }
}

/// `module.attr` reads in `module` whose target the VM does not export,
/// spelled `module.attr` (with the real module name, whatever the program
/// bound it to).
///
/// Deliberately conservative — a false positive only costs the VM's fast
/// path, but a program must never be sent down the compiled path for an
/// attribute that would have worked, and never kept in the VM for one that
/// would not:
///
/// * only a *bare name* receiver counts, and only one bound by `import mod`
///   / `import mod as alias` to a VM-modelled module. A name that anything
///   else binds anywhere in the file (a `from` import, a parameter, a
///   `let`, a `for` target, a `def`, an `except … as`, a match capture) is
///   left alone: it may not be the module where it is read.
/// * dotted reads walk down through submodules the VM also serves
///   (`os.path.join`), and stop at the first non-module (`datetime.datetime`
///   is a class the VM cannot enumerate this way).
/// * an attribute the program itself assigns or `setattr`s on the module is
///   the program's, not the VM's to export.
fn unmodelled_attribute_references(
    module: &ruff_python_ast::ModModule,
    exports: &mut ModelledExports,
) -> std::collections::BTreeSet<String> {
    use ruff_python_ast::visitor::Visitor;

    let mut scan = AttributeScan::default();
    for stmt in &module.body {
        scan.visit_stmt(stmt);
    }
    let mut missing = std::collections::BTreeSet::new();
    for (root, chain, read_at) in &scan.loads {
        if scan.shadowed.contains(root) {
            continue;
        }
        let Some(target) = scan.aliases.get(root) else {
            continue;
        };
        let mut module_name = target.clone();
        let mut owned = root.clone();
        for attr in chain {
            owned.push('.');
            owned.push_str(attr);
            // The program's own store exempts a read only when it comes
            // first in the file: `print(re.purge); re.purge = …` still
            // reads the VM's `re` before the program has defined anything.
            if scan
                .program_defined
                .get(&owned)
                .is_some_and(|defined_at| defined_at < read_at)
            {
                break;
            }
            let Some(names) = exports.names(&module_name) else {
                break;
            };
            if !names.contains(attr) {
                missing.insert(format!("{module_name}.{attr}"));
                break;
            }
            module_name.push('.');
            module_name.push_str(attr);
        }
    }
    // CPython builtins the VM lacks (`exec`, `memoryview`, `globals`), read
    // as a bare name the program does not bind itself.
    {
        let probe = exports.probe.get_or_insert_with(tyc_vm::Interpreter::new);
        for name in &scan.name_loads {
            if !scan.shadowed.contains(name) && tyc_vm::unmodelled_builtin(probe, name) {
                missing.insert(format!("builtin {name}"));
            }
        }
    }
    for attr in UNMODELLED_ATTRIBUTES {
        if scan.attribute_names.contains(*attr) && !scan.shadowed.contains(*attr) {
            missing.insert(format!(".{attr}"));
        }
    }
    // Keyword arguments the VM's builtin, module function or builtin-type
    // method would reject (or silently ignore) where CPython accepts them.
    for (callee, keywords) in &scan.keyword_calls {
        let (module, function): (Option<String>, &str) = match callee {
            KeywordCallee::Name(name) => {
                if scan.shadowed.contains(name) {
                    continue;
                }
                (None, name.as_str())
            }
            KeywordCallee::Chain(root, chain) => {
                let last = chain.last().map(String::as_str).unwrap_or_default();
                let module = (!scan.shadowed.contains(root))
                    .then(|| scan.aliases.get(root))
                    .flatten()
                    .map(|target| {
                        let mut path = target.clone();
                        for attr in &chain[..chain.len() - 1] {
                            path.push('.');
                            path.push_str(attr);
                        }
                        path
                    })
                    .filter(|path| exports.names(path).is_some());
                match module {
                    Some(path) => (Some(path), last),
                    // A method call: a builtin-type method binds keywords to
                    // its CPython signature in the VM, and anything else
                    // binds them to its own parameters.
                    None => continue,
                }
            }
        };
        let probe = exports.probe.get_or_insert_with(tyc_vm::Interpreter::new);
        for kw in keywords {
            if tyc_vm::call_accepts_keyword(probe, module.as_deref(), function, kw) == Some(false) {
                let callee = match &module {
                    Some(m) => format!("{m}.{function}"),
                    None => function.to_owned(),
                };
                missing.insert(format!("{callee}({kw}=…)"));
            }
        }
    }
    // `from re import purge` would fail at the import itself: a member a
    // modelled module does not export is the same gap as `re.purge`, unless
    // it is a submodule the VM also serves (`from os import path`).
    for (module, member) in &scan.from_imports {
        let present = exports.names(module).map(|names| names.contains(member));
        match present {
            None | Some(true) => continue,
            Some(false) => {}
        }
        if exports.names(&format!("{module}.{member}")).is_some() {
            continue;
        }
        missing.insert(format!("{module}.{member}"));
    }
    // Task scheduling (see `ASYNC_SCHEDULING`). `go` and `gather:` arrive
    // here already expanded, to `typhon_runtime.tasks.spawn(…)` and an
    // `asyncio.TaskGroup` (or `asyncio.gather`).
    for (root, chain, _) in &scan.loads {
        if root == "typhon_runtime"
            && chain.len() == 2
            && chain[0] == "tasks"
            && chain[1] == "spawn"
        {
            missing.insert("go (CPython task scheduling)".into());
            continue;
        }
        let Some(first) = chain.first() else {
            continue;
        };
        if !scan.shadowed.contains(root)
            && scan.aliases.get(root).is_some_and(|m| m == "asyncio")
            && ASYNC_SCHEDULING.contains(&first.as_str())
        {
            missing.insert(format!("asyncio.{first} (CPython task scheduling)"));
        }
    }
    for (module, member) in &scan.from_imports {
        if module == "asyncio" && ASYNC_SCHEDULING.contains(&member.as_str()) {
            missing.insert(format!("asyncio.{member} (CPython task scheduling)"));
        }
    }
    missing
}

/// One file's import aliases, bare-name attribute chains, and everything
/// that disqualifies a name from being read as a module.
#[derive(Default)]
struct AttributeScan {
    /// Bound name → the module it names, for `import mod` / `import mod as
    /// alias` on VM-modelled modules.
    aliases: std::collections::HashMap<String, String>,
    /// Names bound by anything other than one consistent `import` — never
    /// trusted as module receivers.
    shadowed: std::collections::HashSet<String>,
    /// `(root name, attribute chain, byte offset)` for every `root.a.b`
    /// read.
    loads: Vec<(String, Vec<String>, usize)>,
    /// Dotted paths (`root.a`, `root.a.b`) the program assigns, deletes or
    /// `setattr`s itself, with the byte offset of the first such store.
    program_defined: std::collections::HashMap<String, usize>,
    /// `(module, member)` for every `from module import member` where the
    /// module is one the VM models.
    from_imports: Vec<(String, String)>,
    /// Every bare name read — checked against the CPython builtins the VM
    /// lacks.
    name_loads: std::collections::BTreeSet<String>,
    /// Every attribute name read or called, on any receiver.
    attribute_names: std::collections::BTreeSet<String>,
    /// Calls passing keyword arguments: the callee and the keyword names.
    keyword_calls: Vec<(KeywordCallee, Vec<String>)>,
}

/// What a keyword-passing call calls, as far as the syntax tells.
enum KeywordCallee {
    /// `f(k=…)`.
    Name(String),
    /// `root.a.f(k=…)`: a module function when `root` names a module, else
    /// a method `f` of whatever `root.a` is (left to the VM, which binds a
    /// builtin-type method's keywords to its CPython signature).
    Chain(String, Vec<String>),
}

/// Attributes of builtin values the VM does not model, which a program
/// reaches only by name (`e.add_note(…)`, `e.__notes__`).
const UNMODELLED_ATTRIBUTES: &[&str] = &["add_note", "__notes__"];

/// `asyncio` members whose effect depends on CPython's event-loop
/// scheduling. The VM runs a coroutine to completion as soon as it is
/// created, so a program that creates tasks, waits on several, or bounds
/// an await in time interleaves (and times out) differently: a task body
/// printed before `create_task` returned, `wait_for` ignored its timeout.
const ASYNC_SCHEDULING: &[&str] = &[
    "Barrier",
    "BoundedSemaphore",
    "Condition",
    "Event",
    "LifoQueue",
    "Lock",
    "PriorityQueue",
    "Queue",
    "Runner",
    "Semaphore",
    "TaskGroup",
    "all_tasks",
    "as_completed",
    "create_task",
    "current_task",
    "ensure_future",
    "gather",
    "get_event_loop",
    "get_running_loop",
    "new_event_loop",
    "run_coroutine_threadsafe",
    "shield",
    "timeout",
    "timeout_at",
    "to_thread",
    "wait",
    "wait_for",
];

impl AttributeScan {
    fn define(&mut self, path: String, at: usize) {
        self.program_defined
            .entry(path)
            .and_modify(|first| *first = (*first).min(at))
            .or_insert(at);
    }

    fn bind_import(&mut self, bound: &str, module: &str) {
        match self.aliases.get(bound) {
            Some(existing) if existing != module => {
                self.shadowed.insert(bound.to_owned());
            }
            Some(_) => {}
            None => {
                if tyc_vm::models_module(module) {
                    self.aliases.insert(bound.to_owned(), module.to_owned());
                } else {
                    // An unmodelled module is the import scan's business; a
                    // project module's attributes are its own.
                    self.shadowed.insert(bound.to_owned());
                }
            }
        }
    }
}

/// `root.a.b` as `(root, [a, b])` when the receiver chain bottoms out in a
/// bare name; `None` for `f().x`, `xs[0].y` and the like.
fn attribute_chain(attr: &ruff_python_ast::ExprAttribute) -> Option<(String, Vec<String>)> {
    use ruff_python_ast::Expr;
    let mut chain = vec![attr.attr.as_str().to_owned()];
    let mut value = attr.value.as_ref();
    loop {
        match value {
            Expr::Attribute(inner) => {
                chain.push(inner.attr.as_str().to_owned());
                value = inner.value.as_ref();
            }
            Expr::Name(name) => {
                chain.reverse();
                return Some((name.id.as_str().to_owned(), chain));
            }
            _ => return None,
        }
    }
}

impl<'a> ruff_python_ast::visitor::Visitor<'a> for AttributeScan {
    fn visit_stmt(&mut self, stmt: &'a ruff_python_ast::Stmt) {
        use ruff_python_ast::Stmt;
        match stmt {
            Stmt::Import(imp) => {
                for alias in &imp.names {
                    let module = alias.name.as_str();
                    match &alias.asname {
                        Some(asname) => self.bind_import(asname.as_str(), module),
                        // `import a.b.c` binds `a`, to the package `a`.
                        None => {
                            let root = module.split('.').next().unwrap_or(module);
                            self.bind_import(root, root);
                        }
                    }
                }
            }
            Stmt::ImportFrom(imp) => {
                let modelled_source = match (&imp.module, imp.level) {
                    (Some(module), 0) if tyc_vm::models_module(module.as_str()) => {
                        Some(module.as_str().to_owned())
                    }
                    _ => None,
                };
                for alias in &imp.names {
                    let bound = alias
                        .asname
                        .as_ref()
                        .map(|a| a.as_str())
                        .unwrap_or(alias.name.as_str());
                    self.shadowed.insert(bound.to_owned());
                    if let Some(module) = &modelled_source {
                        if alias.name.as_str() != "*" {
                            self.from_imports
                                .push((module.clone(), alias.name.as_str().to_owned()));
                        }
                    }
                }
            }
            Stmt::FunctionDef(f) => {
                self.shadowed.insert(f.name.as_str().to_owned());
            }
            Stmt::ClassDef(c) => {
                self.shadowed.insert(c.name.as_str().to_owned());
            }
            Stmt::TypeAlias(t) => {
                if let ruff_python_ast::Expr::Name(n) = t.name.as_ref() {
                    self.shadowed.insert(n.id.as_str().to_owned());
                }
            }
            _ => {}
        }
        ruff_python_ast::visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a ruff_python_ast::Expr) {
        use ruff_python_ast::{Expr, ExprContext};
        match expr {
            // A store or delete of the bare name rebinds it.
            Expr::Name(n) if !matches!(n.ctx, ExprContext::Load) => {
                self.shadowed.insert(n.id.as_str().to_owned());
            }
            Expr::Attribute(a) => {
                self.attribute_names.insert(a.attr.as_str().to_owned());
                if let Some((root, chain)) = attribute_chain(a) {
                    let at = a.range.start().to_usize();
                    if matches!(a.ctx, ExprContext::Load) {
                        self.loads.push((root, chain, at));
                    } else {
                        // `mod.x = …` / `del mod.x`: every prefix the store
                        // touches is the program's own.
                        let mut path = root;
                        for attr in &chain {
                            path.push('.');
                            path.push_str(attr);
                        }
                        self.define(path, at);
                    }
                    // The chain is names and attributes only: nothing left
                    // to walk.
                    return;
                }
            }
            Expr::Name(n) => {
                self.name_loads.insert(n.id.as_str().to_owned());
            }
            // `setattr(mod, "x", …)` / `delattr(mod, "x")` with a literal
            // name — the dynamic spelling of the store above.
            Expr::Call(call) => {
                let keywords: Vec<String> = call
                    .arguments
                    .keywords
                    .iter()
                    .filter_map(|k| k.arg.as_ref().map(|a| a.as_str().to_owned()))
                    .collect();
                // A `**mapping` splat names keywords the scan cannot see.
                let splat = call.arguments.keywords.iter().any(|k| k.arg.is_none());
                if !keywords.is_empty() && !splat {
                    let callee = match call.func.as_ref() {
                        Expr::Name(n) => Some(KeywordCallee::Name(n.id.as_str().to_owned())),
                        Expr::Attribute(a) => attribute_chain(a)
                            .map(|(root, chain)| KeywordCallee::Chain(root, chain)),
                        _ => None,
                    };
                    if let Some(callee) = callee {
                        self.keyword_calls.push((callee, keywords));
                    }
                }
                if let Expr::Name(func) = call.func.as_ref() {
                    if matches!(func.id.as_str(), "setattr" | "delattr") {
                        if let [Expr::Name(target), Expr::StringLiteral(name), ..] =
                            call.arguments.args.as_ref()
                        {
                            self.define(
                                format!("{}.{}", target.id.as_str(), name.value.to_str()),
                                call.range.start().to_usize(),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
        ruff_python_ast::visitor::walk_expr(self, expr);
    }

    fn visit_parameters(&mut self, parameters: &'a ruff_python_ast::Parameters) {
        for param in parameters.iter() {
            self.shadowed.insert(param.name().as_str().to_owned());
        }
        ruff_python_ast::visitor::walk_parameters(self, parameters);
    }

    fn visit_except_handler(&mut self, handler: &'a ruff_python_ast::ExceptHandler) {
        let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
        if let Some(name) = &h.name {
            self.shadowed.insert(name.as_str().to_owned());
        }
        ruff_python_ast::visitor::walk_except_handler(self, handler);
    }

    fn visit_pattern(&mut self, pattern: &'a ruff_python_ast::Pattern) {
        use ruff_python_ast::Pattern;
        match pattern {
            Pattern::MatchAs(p) => {
                if let Some(name) = &p.name {
                    self.shadowed.insert(name.as_str().to_owned());
                }
            }
            Pattern::MatchStar(p) => {
                if let Some(name) = &p.name {
                    self.shadowed.insert(name.as_str().to_owned());
                }
            }
            Pattern::MatchMapping(p) => {
                if let Some(rest) = &p.rest {
                    self.shadowed.insert(rest.as_str().to_owned());
                }
            }
            _ => {}
        }
        ruff_python_ast::visitor::walk_pattern(self, pattern);
    }
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

    /// Write `source` as a lone `.ty` file and run the pre-run scan on it.
    fn scan_source(source: &str) -> Option<Vec<String>> {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("probe.ty");
        std::fs::write(&entry, source).unwrap();
        unmodelled_references(&entry, &entry)
    }

    #[test]
    fn scan_routes_unmodelled_builtins_and_keywords() {
        let got = scan_source("exec(\"x = 1\")\nprint(memoryview(b\"a\"))\n").unwrap_or_default();
        assert!(got.contains(&"builtin exec".to_owned()), "{got:?}");
        assert!(got.contains(&"builtin memoryview".to_owned()), "{got:?}");
        // A name the program binds itself is its own.
        assert_eq!(
            scan_source("def exec(s: str) -> None:\n    pass\nexec(\"x\")\n"),
            None
        );
        let got =
            scan_source("import json\nprint(json.dumps({}, default=str))\n").unwrap_or_default();
        assert!(got.contains(&"json.dumps(default=…)".to_owned()), "{got:?}");
        let got = scan_source("e = ValueError(\"v\")\ne.add_note(\"n\")\n").unwrap_or_default();
        assert!(got.contains(&".add_note".to_owned()), "{got:?}");
        // Keywords the VM binds stay on the VM.
        assert_eq!(
            scan_source("import json\nimport math\nprint(round(2.5, ndigits=0), int(\"ff\", base=16), math.prod([2], start=3), json.loads(\"{}\", object_hook=dict), \"a b\".split(maxsplit=1))\n"),
            None
        );
    }

    #[test]
    fn scan_routes_eager_generators_to_cpython() {
        // Two yields in one expression: the VM would run the body eagerly.
        let eager =
            "def g() -> object:\n    print(\"start\")\n    yield (yield 1)\nprint(list(g()))\n";
        assert_eq!(
            scan_source(eager),
            Some(vec![
                "a generator whose yield the VM cannot suspend".to_owned()
            ])
        );
        // The same check reaches methods and nested functions.
        let method = "plain class C:\n    def g(self) -> object:\n        def inner() -> object:\n            yield (yield 1)\n        return inner()\nprint(C())\n";
        assert!(scan_source(method).is_some());
        // An ordinary generator runs lazily and stays on the VM.
        let lazy = "def g() -> object:\n    mut n = 0\n    while True:\n        yield n\n        n += 1\nprint(next(g()))\n";
        assert_eq!(scan_source(lazy), None);
    }

    #[test]
    fn scan_routes_task_scheduling_to_cpython() {
        let task = "import asyncio\nasync def w() -> None:\n    await asyncio.sleep(0)\nasync def main() -> None:\n    let t = asyncio.create_task(w())\n    print(\"created\")\n    await t\nasyncio.run(main())\n";
        let got = scan_source(task).unwrap_or_default();
        assert!(
            got.contains(&"asyncio.create_task (CPython task scheduling)".to_owned()),
            "{got:?}"
        );
        let from = "from asyncio import wait_for\nimport asyncio\nasync def w() -> int:\n    await asyncio.sleep(0)\n    return 1\nasync def main() -> None:\n    print(await wait_for(w(), 0.5))\nasyncio.run(main())\n";
        let got = scan_source(from).unwrap_or_default();
        assert!(
            got.contains(&"asyncio.wait_for (CPython task scheduling)".to_owned()),
            "{got:?}"
        );
        let go = "import asyncio\nasync def w() -> None:\n    await asyncio.sleep(0)\nasync def main() -> None:\n    go w()\n    await asyncio.sleep(0)\nasyncio.run(main())\n";
        let got = scan_source(go).unwrap_or_default();
        assert!(
            got.contains(&"go (CPython task scheduling)".to_owned()),
            "{got:?}"
        );
        let gather = "import asyncio\nasync def w(n: int) -> int:\n    await asyncio.sleep(0)\n    return n\nasync def main() -> None:\n    gather:\n        a = w(1)\n        b = w(2)\n    print(a, b)\nasyncio.run(main())\n";
        let got = scan_source(gather).unwrap_or_default();
        assert!(
            got.contains(&"asyncio.TaskGroup (CPython task scheduling)".to_owned()),
            "{got:?}"
        );
        // Plain sequential awaits schedule nothing: they stay on the VM.
        let sequential = "import asyncio\nasync def w() -> int:\n    await asyncio.sleep(0)\n    return 1\nasync def main() -> None:\n    let v = await w()\n    print(v)\nasyncio.run(main())\n";
        assert_eq!(scan_source(sequential), None);
    }

    #[test]
    fn attribute_scan_reports_a_missing_attribute_of_a_modelled_module() {
        // The module is modelled, the attribute is not: the program would
        // die with `AttributeError` in the VM, so it takes the compiled path,
        // and the note names `module.attr`.
        let missing = scan_source("import math\n\nprint(math.no_such_function(1.0))\n");
        assert_eq!(missing, Some(vec!["math.no_such_function".to_owned()]));
        // The real module name is reported, not the alias.
        let missing = scan_source("import math as m\n\nprint(m.no_such_function(1.0))\n");
        assert_eq!(missing, Some(vec!["math.no_such_function".to_owned()]));
    }

    #[test]
    fn attribute_scan_walks_dotted_submodules() {
        assert_eq!(
            scan_source("import os\n\nprint(os.path.join(\"a\", \"b\"))\n"),
            None
        );
        assert_eq!(
            scan_source("import os.path as p\n\nprint(p.join(\"a\", \"b\"))\n"),
            None
        );
        assert_eq!(
            scan_source("import os\n\nprint(os.path.no_such_helper(\"a\"))\n"),
            Some(vec!["os.path.no_such_helper".to_owned()])
        );
        // A non-module member ends the walk: the VM cannot enumerate a class
        // this way, so nothing below it is judged.
        assert_eq!(
            scan_source("import datetime\n\nprint(datetime.datetime.now().year)\n"),
            None
        );
    }

    #[test]
    fn attribute_scan_never_false_positives_on_modelled_names() {
        // Every name the VM's model exports passes, including one that only
        // exists through the live namespace of a shim module.
        assert_eq!(
            scan_source(
                "import math\nimport sys\nimport json\n\n\
                 print(math.isclose(1.0, 1.0), sys.modules is not None, json.dumps([1]))\n"
            ),
            None
        );
    }

    #[test]
    fn attribute_scan_checks_from_imported_members_of_modelled_modules() {
        // `from json import detect_encoding` fails at the import under the
        // VM exactly as `json.detect_encoding` would at the read, so it takes
        // the compiled path too. (`re` is no example: any `re` import now
        // takes the compiled path, see `importing_re_takes_the_compiled_path`.)
        assert_eq!(
            scan_source("from json import detect_encoding\n\nprint(detect_encoding)\n"),
            Some(vec!["json.detect_encoding".to_owned()])
        );
        // A member the VM has, and a submodule the VM serves, are fine.
        assert_eq!(
            scan_source("from math import isclose\n\nprint(isclose(1.0, 1.0))\n"),
            None
        );
        assert_eq!(
            scan_source("from os import path\n\nprint(path.join(\"a\", \"b\"))\n"),
            None
        );
        // A project module is the import scan's business, not this one's.
        assert_eq!(
            scan_source("from .sibling import thing\n\nprint(thing)\n"),
            None
        );
    }

    #[test]
    fn attribute_scan_exempts_a_program_defined_attribute_only_after_its_store() {
        // The store comes first: the read is the program's own attribute.
        assert_eq!(
            scan_source("import json\n\njson.detect_encoding = 1\nprint(json.detect_encoding)\n"),
            None
        );
        // The read comes first: it still reaches the VM's `json`, which has
        // no `detect_encoding`, so the program takes the compiled path.
        assert_eq!(
            scan_source("import json\n\nprint(json.detect_encoding)\njson.detect_encoding = 1\n"),
            Some(vec!["json.detect_encoding".to_owned()])
        );
    }

    #[test]
    fn attribute_scan_ignores_from_imports_and_program_defined_attributes() {
        // A name a `from` import binds is never read as a module.
        assert_eq!(
            scan_source("from math import isclose\n\nprint(isclose(1.0, 1.0))\n"),
            None
        );
        // An attribute the program sets on the module is the program's.
        assert_eq!(
            scan_source("import sys\n\nsys.custom_flag = 1\nprint(sys.custom_flag)\n"),
            None
        );
        assert_eq!(
            scan_source("import sys\n\nsetattr(sys, \"custom_flag\", 1)\nprint(sys.custom_flag)\n"),
            None
        );
    }

    #[test]
    fn importing_re_takes_the_compiled_path() {
        // The VM's `re` runs on Rust's regex engine, whose `$`, empty-match,
        // lookaround and flag semantics differ from Python's (W5-13), so a
        // program that imports `re` runs on CPython under plain `tyc run`.
        assert_eq!(
            scan_source("import re\n\nprint(re.fullmatch(\"a\", \"a\") is not None)\n"),
            Some(vec!["re (Python regular-expression semantics)".to_owned()])
        );
    }

    #[test]
    fn attribute_scan_leaves_a_rebound_alias_alone() {
        // The alias is a parameter (or any other binding) somewhere in the
        // file: a read through it may not be the module, so it is not judged.
        assert_eq!(
            scan_source(
                "import json\n\ndef f(json: int) -> int:\n    return json.no_such_method()\n\n\
                 print(f(1))\n"
            ),
            None
        );
        assert_eq!(
            scan_source("import math\n\nfor math in [1, 2]:\n    print(math.no_such)\n"),
            None
        );
        // A project module's attributes are its own, never judged either.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("helper.ty"), "pub let value: int = 1\n").unwrap();
        let entry = dir.path().join("probe.ty");
        std::fs::write(
            &entry,
            "import helper\n\nprint(helper.value, helper.missing)\n",
        )
        .unwrap();
        assert_eq!(unmodelled_references(&entry, &entry), None);
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
