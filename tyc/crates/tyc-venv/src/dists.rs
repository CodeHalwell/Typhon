//! Distribution name → import name resolution.
//!
//! A `[dependencies]` key names a PyPI *distribution*; Python code imports
//! *modules*. The two only coincide by convention, and plenty of packages
//! break it:
//!
//! - a different name outright (`beautifulsoup4` → `bs4`, `pillow` → `PIL`,
//!   `pyyaml` → `yaml`, `scikit-learn` → `sklearn`);
//! - a variant of another distribution (`pydantic-ai-slim` → `pydantic_ai`,
//!   `opencv-python-headless` → `cv2`, `psycopg2-binary` → `psycopg2`);
//! - one module shared by many distributions (namespace packages:
//!   `google-cloud-storage`, `azure-identity`, `opentelemetry-api`);
//! - several modules from one distribution (`setuptools` → `setuptools`,
//!   `pkg_resources`, `_distutils_hack`; `attrs` → `attr`, `attrs`);
//! - stub-only distributions (`types-requests`, `pandas-stubs`), which carry
//!   no runtime module of their own;
//! - editable installs, whose code lives outside `site-packages` and is
//!   reached through a `.pth` path entry or an import-hook finder module;
//! - extras and markers in the key itself (`pydantic-ai-slim[anthropic]`).
//!
//! [`resolve_import_roots`] answers "which top-level modules does this set of
//! declared distributions provide?" from the installed metadata when there is
//! a venv, and from a table of well-known mismatches plus a few conservative
//! name heuristics when the distribution is not installed. Only declared
//! distributions contribute, so a package that happens to be installed
//! locally never quietly passes `tyc check` on one machine and fails on a
//! fresh clone.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// The bare distribution name of a dependency key or PEP 508 requirement:
/// extras (`[anthropic]`), environment markers (`; python_version < "3.14"`),
/// a direct reference (`@ git+https://…`) and a version specifier are all
/// stripped. `pydantic-ai-slim[anthropic]>=1.0` → `pydantic-ai-slim`.
pub fn dependency_name(key: &str) -> &str {
    let key = key.trim();
    let end = key
        .find(['[', ';', '@', '<', '>', '=', '!', '~', ' ', '('])
        .unwrap_or(key.len());
    key[..end].trim()
}

/// Normalise a PyPI distribution name per PEP 503: replace runs of
/// `[-_.]+` with a single `-` and lowercase. `tyc` uses this to match
/// `[dependencies]` keys against `.dist-info` directory names regardless
/// of casing or separator drift (`Agent-Framework-Core`,
/// `agent_framework_core`, `agent.framework.core` all normalise the
/// same). Extras and version specifiers are dropped first.
pub fn pep503_normalise(name: &str) -> String {
    let name = dependency_name(name);
    let mut out = String::with_capacity(name.len());
    let mut prev_sep = false;
    for ch in name.chars() {
        if ch == '-' || ch == '_' || ch == '.' {
            if !prev_sep && !out.is_empty() {
                out.push('-');
                prev_sep = true;
            }
        } else {
            out.push(ch.to_ascii_lowercase());
            prev_sep = false;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

/// Well-known distributions whose import name cannot be derived from the
/// distribution name. Consulted only when the distribution is not installed
/// (no venv yet, `--no-sync`, CI before `uv sync`); installed metadata always
/// wins. Keys are PEP 503-normalised.
const KNOWN_DIST_IMPORTS: &[(&str, &[&str])] = &[
    ("absl-py", &["absl"]),
    ("agent-framework-core", &["agent_framework"]),
    ("attrs", &["attr", "attrs"]),
    ("beautifulsoup4", &["bs4"]),
    ("discord-py", &["discord"]),
    ("faiss-cpu", &["faiss"]),
    ("faiss-gpu", &["faiss"]),
    ("google-api-python-client", &["googleapiclient"]),
    ("google-genai", &["google"]),
    ("google-generativeai", &["google"]),
    ("grpcio", &["grpc"]),
    ("ipython", &["IPython"]),
    ("markdown-it-py", &["markdown_it"]),
    ("msgpack-python", &["msgpack"]),
    ("mysqlclient", &["MySQLdb"]),
    ("opencv-contrib-python", &["cv2"]),
    ("opencv-contrib-python-headless", &["cv2"]),
    ("opencv-python", &["cv2"]),
    ("opencv-python-headless", &["cv2"]),
    ("pillow", &["PIL"]),
    ("protobuf", &["google"]),
    ("psycopg2-binary", &["psycopg2"]),
    ("pycryptodome", &["Crypto"]),
    ("pycryptodomex", &["Cryptodome"]),
    ("pygithub", &["github"]),
    ("pyjwt", &["jwt"]),
    ("pymupdf", &["fitz", "pymupdf"]),
    ("pyopenssl", &["OpenSSL"]),
    ("pyserial", &["serial"]),
    ("python-dateutil", &["dateutil"]),
    ("python-docx", &["docx"]),
    ("python-dotenv", &["dotenv"]),
    ("python-jose", &["jose"]),
    ("python-magic", &["magic"]),
    ("python-multipart", &["multipart", "python_multipart"]),
    ("python-pptx", &["pptx"]),
    ("python-telegram-bot", &["telegram"]),
    ("pyusb", &["usb"]),
    (
        "pywin32",
        &["win32api", "win32con", "win32com", "pywintypes"],
    ),
    ("pyyaml", &["yaml"]),
    ("pyzmq", &["zmq"]),
    ("ruamel-yaml", &["ruamel"]),
    ("scikit-image", &["skimage"]),
    ("scikit-learn", &["sklearn"]),
    ("setuptools", &["setuptools", "pkg_resources"]),
    ("tensorflow-cpu", &["tensorflow"]),
    ("tensorflow-gpu", &["tensorflow"]),
    ("websocket-client", &["websocket"]),
];

/// Suffixes that name a *variant build* of a distribution rather than a
/// different package: `pydantic-ai-slim` installs `pydantic_ai`,
/// `psycopg-binary` installs into `psycopg`, `opencv-python-headless` …
const VARIANT_SUFFIXES: &[&str] = &[
    "-slim",
    "-core",
    "-lite",
    "-binary",
    "-headless",
    "-cpu",
    "-gpu",
    "-api",
    "-sdk",
];

/// Distribution prefixes that are namespace packages: every
/// `google-cloud-*` distribution contributes to the `google` namespace.
const NAMESPACE_PREFIXES: &[(&str, &str)] = &[
    ("google-", "google"),
    ("azure-", "azure"),
    ("opentelemetry-", "opentelemetry"),
    ("jaraco-", "jaraco"),
    ("zope-", "zope"),
    ("sphinxcontrib-", "sphinxcontrib"),
];

/// Candidate import roots for a declared distribution that is *not*
/// installed. Deliberately generous on the well-known cases and
/// conservative otherwise: the name itself (underscored), the known-mapping
/// table, a variant-suffix strip, and the namespace prefix.
pub fn guess_import_roots(dist: &str) -> Vec<String> {
    let norm = pep503_normalise(dist);
    let mut out: Vec<String> = Vec::new();
    if norm.is_empty() {
        return out;
    }
    let mut push = |s: &str| {
        if !s.is_empty() && !out.iter().any(|o| o == s) {
            out.push(s.to_owned());
        }
    };
    // Stub-only distributions (PEP 561): `types-requests` / `pandas-stubs`
    // describe `requests` / `pandas`.
    let base = norm
        .strip_prefix("types-")
        .or_else(|| norm.strip_suffix("-stubs"))
        .unwrap_or(&norm);
    push(&base.replace('-', "_"));
    if let Some((_, roots)) = KNOWN_DIST_IMPORTS.iter().find(|(d, _)| *d == base) {
        for r in *roots {
            push(r);
        }
    }
    for suffix in VARIANT_SUFFIXES {
        if let Some(stem) = base.strip_suffix(suffix) {
            if !stem.is_empty() {
                push(&stem.replace('-', "_"));
            }
        }
    }
    if let Some(stem) = base.strip_prefix("python-") {
        push(&stem.replace('-', "_"));
    }
    if let Some(stem) = base.strip_prefix("py") {
        // `pyzmq` style is covered by the table; only strip a `py-` prefix.
        if let Some(stem) = stem.strip_prefix('-') {
            push(&stem.replace('-', "_"));
        }
    }
    for (prefix, ns) in NAMESPACE_PREFIXES {
        if base.starts_with(prefix) {
            push(ns);
        }
    }
    out
}

/// The virtual environment a project's code runs in, in priority order:
///
/// 1. `<project_root>/.venv` — the `uv` / `tyc build` default;
/// 2. `$UV_PROJECT_ENVIRONMENT` (resolved against the project root when
///    relative), which `uv` honours in place of `.venv`;
/// 3. the nearest `.venv` in an ancestor directory — a `uv` workspace or a
///    monorepo keeps one environment at its root;
/// 4. `$VIRTUAL_ENV`, an activated environment.
///
/// A directory only counts when it holds a `pyvenv.cfg`.
pub fn find_venv(project_root: &Path) -> Option<PathBuf> {
    let root = std::path::absolute(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let is_venv = |p: &Path| p.join("pyvenv.cfg").is_file();
    let local = root.join(".venv");
    if is_venv(&local) {
        return Some(local);
    }
    if let Some(env) = std::env::var_os("UV_PROJECT_ENVIRONMENT") {
        let p = PathBuf::from(env);
        let p = if p.is_absolute() { p } else { root.join(p) };
        if is_venv(&p) {
            return Some(p);
        }
    }
    for ancestor in root.ancestors().skip(1) {
        let candidate = ancestor.join(".venv");
        if is_venv(&candidate) {
            return Some(candidate);
        }
    }
    if let Some(env) = std::env::var_os("VIRTUAL_ENV") {
        let p = PathBuf::from(env);
        if p.is_absolute() && is_venv(&p) {
            return Some(p);
        }
    }
    None
}

/// The interpreter inside a venv, probing the native layout first.
pub fn venv_python(venv: &Path) -> Option<PathBuf> {
    let unix = venv.join("bin").join("python");
    let windows = venv.join("Scripts").join("python.exe");
    let candidates = if cfg!(windows) {
        [windows, unix]
    } else {
        [unix, windows]
    };
    candidates.into_iter().find(|c| c.is_file())
}

/// `site-packages` directories of a venv: `lib/pythonX.Y/site-packages`
/// (and `lib64`) on POSIX, `Lib/site-packages` on Windows.
pub fn site_packages_dirs(venv: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for lib in ["lib", "lib64"] {
        if let Ok(entries) = std::fs::read_dir(venv.join(lib)) {
            let mut found: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path().join("site-packages"))
                .filter(|p| p.is_dir())
                .collect();
            found.sort();
            for p in found {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    let win = venv.join("Lib").join("site-packages");
    if win.is_dir() {
        out.push(win);
    }
    out
}

/// An installed distribution's metadata directory, with its normalised name.
struct InstalledDist {
    name: String,
    meta_dir: PathBuf,
    site: PathBuf,
    egg: bool,
}

fn installed_dists(site: &Path) -> Vec<InstalledDist> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(site) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let (stem, egg) = if let Some(stem) = file_name.strip_suffix(".dist-info") {
            (stem, false)
        } else if let Some(stem) = file_name.strip_suffix(".egg-info") {
            (stem, true)
        } else {
            continue;
        };
        // `<dist>-<version>[-pyX.Y]` — the distribution name is everything
        // before the first `-` that starts a version (a digit). Wheel
        // metadata escapes `-` in the name to `_`, so the first such `-` is
        // the separator; egg-info may carry a trailing `-pyX.Y` too.
        let dist = stem
            .match_indices('-')
            .find(|(i, _)| stem[i + 1..].starts_with(|c: char| c.is_ascii_digit()))
            .map(|(i, _)| &stem[..i])
            .unwrap_or(stem);
        out.push(InstalledDist {
            name: pep503_normalise(dist),
            meta_dir: path.clone(),
            site: site.to_path_buf(),
            egg,
        });
    }
    // Legacy `setup.py develop` installs leave `<dist>.egg-link` pointing at
    // the source checkout, whose `<dist>.egg-info` holds the metadata.
    if let Ok(entries) = std::fs::read_dir(site) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_suffix(".egg-link"))
            else {
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(target) = text.lines().next().map(str::trim).filter(|l| !l.is_empty()) else {
                continue;
            };
            let target = PathBuf::from(target);
            if let Ok(inner) = std::fs::read_dir(&target) {
                for e in inner.flatten() {
                    let p = e.path();
                    if p.extension().is_some_and(|x| x == "egg-info") {
                        out.push(InstalledDist {
                            name: pep503_normalise(stem),
                            meta_dir: p,
                            site: target.clone(),
                            egg: true,
                        });
                    }
                }
            }
        }
    }
    out
}

/// Whether `name` is a plausible Python identifier for a top-level module.
fn is_module_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// Top-level modules a directory provides on `sys.path`: packages (any
/// directory holding Python source, which also covers PEP 420 namespace
/// packages) and single-file modules.
fn modules_in_dir(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.is_dir() {
            if is_module_name(name) && dir_has_python(&path, DIR_SCAN_DEPTH) {
                out.push(name.to_owned());
            }
        } else if let Some(stem) = name
            .strip_suffix(".py")
            .or_else(|| name.strip_suffix(".pyi"))
        {
            if is_module_name(stem) && stem != "setup" && stem != "conftest" {
                out.push(stem.to_owned());
            }
        }
    }
    out
}

/// How deep [`dir_has_python`] looks for a Python file under a candidate
/// package directory. Real packages put an `__init__.py` or module at the
/// top; the bound only exists so a pathological tree cannot stall `tyc`.
const DIR_SCAN_DEPTH: usize = 8;

/// Whether `dir` holds any Python (source, stub or extension module) within
/// `depth` levels. Symlinked subdirectories are not followed, so a link
/// cycle (`a -> .`) cannot recurse forever; the candidate directory itself
/// may be a symlink, which the caller has already decided to look through.
fn dir_has_python(dir: &Path, depth: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let p = e.path();
        if p.extension()
            .is_some_and(|x| x == "py" || x == "pyi" || x == "so" || x == "pyd")
        {
            return true;
        }
        depth > 0
            && e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink())
            && dir_has_python(&p, depth - 1)
    })
}

/// Import roots reachable through a `.pth` file: each plain line is a
/// directory added to `sys.path` (setuptools / hatch / pdm "path" editable
/// installs); an `import __editable___<dist>_finder` line names a generated
/// finder module whose `MAPPING` dict maps import names to source paths
/// (setuptools' strict / compat editable mode).
fn roots_from_pth(site: &Path, pth: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(text) = std::fs::read_to_string(pth) else {
        return out;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("import ") {
            // `import __editable___pkg_1_0_finder; __editable___…install()`
            let module = rest
                .split(|c: char| c == ';' || c == ',' || c.is_whitespace())
                .next()
                .unwrap_or("");
            if module.starts_with("__editable__") {
                out.extend(roots_from_editable_finder(
                    &site.join(format!("{module}.py")),
                ));
            }
            continue;
        }
        let dir = PathBuf::from(line);
        let dir = if dir.is_absolute() {
            dir
        } else {
            site.join(dir)
        };
        out.extend(modules_in_dir(&dir));
    }
    out
}

/// Keys of the `MAPPING = {'pkg': '/src/pkg', …}` literal in a setuptools
/// editable finder module, plus any `NAMESPACES` keys.
fn roots_from_editable_finder(path: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return out;
    };
    for marker in ["MAPPING", "NAMESPACES"] {
        let Some(start) = text
            .find(&format!("{marker}: dict"))
            .or_else(|| text.find(&format!("{marker} =")))
        else {
            continue;
        };
        let Some(open) = text[start..].find('{') else {
            continue;
        };
        let body_start = start + open + 1;
        let Some(close) = text[body_start..].find('}') else {
            continue;
        };
        let body = &text[body_start..body_start + close];
        // Each entry is `'key': value`; the key is the first quoted string
        // after the start or a comma.
        for entry in body.split(',') {
            let entry = entry.trim();
            let quote = match entry.chars().next() {
                Some(q @ ('\'' | '"')) => q,
                _ => continue,
            };
            if let Some(end) = entry[1..].find(quote) {
                let key = &entry[1..1 + end];
                let root = key.split('.').next().unwrap_or(key);
                if is_module_name(root) {
                    out.push(root.to_owned());
                }
            }
        }
    }
    out
}

/// Top-level import roots one installed distribution provides.
fn roots_of(dist: &InstalledDist) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // 1. `top_level.txt` (setuptools wheels, every egg-info).
    if let Ok(content) = std::fs::read_to_string(dist.meta_dir.join("top_level.txt")) {
        for line in content.lines() {
            let line = line.trim();
            let root = line.split(['/', '\\', '.']).next().unwrap_or(line);
            if is_module_name(root) {
                out.push(root.to_owned());
            }
        }
    }
    // 2. The file manifest: `RECORD` (wheels) or `installed-files.txt`
    //    (egg-info, paths relative to the metadata directory).
    let (manifest, base) = if dist.egg {
        (
            std::fs::read_to_string(dist.meta_dir.join("installed-files.txt")).ok(),
            dist.meta_dir.clone(),
        )
    } else {
        (
            std::fs::read_to_string(dist.meta_dir.join("RECORD")).ok(),
            dist.site.clone(),
        )
    };
    if let Some(manifest) = manifest {
        for line in manifest.lines() {
            let raw = match line.strip_prefix('"') {
                // PEP 376: a path containing a comma is double-quoted.
                Some(quoted) => quoted.split('"').next().unwrap_or(""),
                None => line.split(',').next().unwrap_or(""),
            };
            let raw = raw.trim().replace('\\', "/");
            if raw.is_empty() || raw.starts_with('/') {
                continue;
            }
            // Resolve egg-info's `../pkg/__init__.py` against the metadata
            // directory, then re-express it relative to site-packages.
            let rel: String = if dist.egg {
                let joined = normalise_rel(&base, &raw);
                match joined.strip_prefix(&dist.site) {
                    Ok(r) => r.to_string_lossy().replace('\\', "/"),
                    Err(_) => continue,
                }
            } else {
                if raw.starts_with("../") {
                    continue;
                }
                raw
            };
            let mut parts = rel.split('/');
            let head = parts.next().unwrap_or("");
            let nested = parts.next().is_some();
            if head.ends_with(".dist-info")
                || head.ends_with(".egg-info")
                || head.ends_with(".data")
            {
                continue;
            }
            if !nested && head.ends_with(".pth") {
                // Editable installs: the code lives elsewhere.
                out.extend(roots_from_pth(&dist.site, &dist.site.join(head)));
                continue;
            }
            let module = if nested {
                // A file inside a top-level directory: a package (regular or
                // namespace) when it is Python source or an extension module.
                let is_code = [".py", ".pyi", ".so", ".pyd"]
                    .iter()
                    .any(|ext| rel.ends_with(ext));
                if !is_code {
                    continue;
                }
                head
            } else {
                // A top-level single-file module or extension (`foo.py`,
                // `foo.cpython-313-x86_64-linux-gnu.so`).
                match head
                    .strip_suffix(".py")
                    .or_else(|| head.strip_suffix(".pyi"))
                    .or_else(|| {
                        (head.ends_with(".so") || head.ends_with(".pyd"))
                            .then(|| head.split('.').next().unwrap_or(""))
                    }) {
                    Some(stem) => stem,
                    None => continue,
                }
            };
            // `__editable___x_finder.py`, `_virtualenv.py`, `__pycache__` are
            // install plumbing, not something user code imports.
            if module.starts_with("__") || !is_module_name(module) {
                continue;
            }
            out.push(module.to_owned());
        }
    }
    // 3. Stub-only distributions: `foo-stubs/` provides types for `foo`.
    out = out
        .into_iter()
        .map(|r| {
            match r
                .strip_suffix("-stubs")
                .or_else(|| r.strip_suffix("_stubs"))
            {
                Some(base) => base.to_owned(),
                None => r,
            }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// `base.join(rel)` with `..` / `.` components folded lexically.
fn normalise_rel(base: &Path, rel: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out
}

/// Every top-level import root the declared distributions provide.
///
/// `declared` holds dependency keys as written (`pydantic-ai-slim[anthropic]`)
/// or already normalised; both work. Installed metadata in the project's venv
/// (see [`find_venv`]) is authoritative for each distribution it finds; a
/// declared distribution that is not installed falls back to
/// [`guess_import_roots`]. `stub-only` distributions (`types-x`, `x-stubs`)
/// resolve to the module they describe.
pub fn resolve_import_roots<'a>(
    project_root: &Path,
    declared: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let declared: HashSet<String> = declared
        .into_iter()
        .map(pep503_normalise)
        .filter(|d| !d.is_empty())
        .collect();
    let mut out: BTreeSet<String> = BTreeSet::new();
    if declared.is_empty() {
        return Vec::new();
    }
    let mut resolved: HashSet<String> = HashSet::new();
    if let Some(venv) = find_venv(project_root) {
        for site in site_packages_dirs(&venv) {
            for dist in installed_dists(&site) {
                if !declared.contains(&dist.name) {
                    continue;
                }
                let roots = roots_of(&dist);
                if !roots.is_empty() {
                    resolved.insert(dist.name.clone());
                    out.extend(roots);
                }
            }
        }
    }
    for dist in &declared {
        if !resolved.contains(dist) {
            out.extend(guess_import_roots(dist));
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_name_strips_extras_markers_and_specifiers() {
        assert_eq!(
            dependency_name("pydantic-ai-slim[anthropic]"),
            "pydantic-ai-slim"
        );
        assert_eq!(dependency_name("requests>=2,<3"), "requests");
        assert_eq!(dependency_name("pkg @ git+https://x/y"), "pkg");
        assert_eq!(dependency_name("tomli; python_version < '3.11'"), "tomli");
        assert_eq!(
            pep503_normalise("Pydantic_AI.Slim[logfire]"),
            "pydantic-ai-slim"
        );
    }

    #[test]
    fn guesses_cover_variants_tables_namespaces_and_stubs() {
        let g = guess_import_roots("pydantic-ai-slim");
        assert!(g.contains(&"pydantic_ai".to_owned()), "{g:?}");
        assert!(guess_import_roots("beautifulsoup4").contains(&"bs4".to_owned()));
        assert!(guess_import_roots("opencv-python-headless").contains(&"cv2".to_owned()));
        assert!(guess_import_roots("google-cloud-storage").contains(&"google".to_owned()));
        assert!(guess_import_roots("types-requests").contains(&"requests".to_owned()));
        assert!(guess_import_roots("pandas-stubs").contains(&"pandas".to_owned()));
        assert!(guess_import_roots("python-dotenv").contains(&"dotenv".to_owned()));
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn venv(root: &Path) -> PathBuf {
        let venv = root.join(".venv");
        write(&venv.join("pyvenv.cfg"), "home = /usr\n");
        let site = venv.join("lib").join("python3.13").join("site-packages");
        std::fs::create_dir_all(&site).unwrap();
        site
    }

    #[test]
    fn record_maps_a_renamed_distribution() {
        let tmp = tempfile::tempdir().unwrap();
        let site = venv(tmp.path());
        write(
            &site.join("pydantic_ai_slim-2.64.0.dist-info/RECORD"),
            "../../../bin/pai,sha256=x,1\n\
             pydantic_ai/.agents/SKILL.md,sha256=x,1\n\
             pydantic_ai/__init__.py,sha256=x,1\n\
             pydantic_ai_slim-2.64.0.dist-info/RECORD,,\n",
        );
        let roots = resolve_import_roots(tmp.path(), ["pydantic-ai-slim[anthropic]"]);
        assert!(roots.contains(&"pydantic_ai".to_owned()), "{roots:?}");
    }

    #[test]
    fn namespace_packages_extensions_and_stub_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let site = venv(tmp.path());
        // PEP 420 namespace: no `google/__init__.py`.
        write(
            &site.join("google_cloud_storage-2.0.0.dist-info/RECORD"),
            "google/cloud/storage/__init__.py,,\n",
        );
        // A top-level C extension module.
        write(
            &site.join("ujson-5.0.0.dist-info/RECORD"),
            "ujson.cpython-313-x86_64-linux-gnu.so,,\n",
        );
        // A stub-only distribution.
        write(
            &site.join("types_requests-2.0.0.dist-info/RECORD"),
            "requests-stubs/__init__.pyi,,\n",
        );
        let roots = resolve_import_roots(
            tmp.path(),
            ["google-cloud-storage", "ujson", "types-requests"],
        );
        for want in ["google", "ujson", "requests"] {
            assert!(
                roots.contains(&want.to_owned()),
                "{want} missing: {roots:?}"
            );
        }
    }

    #[test]
    fn editable_installs_through_pth_paths_and_finders() {
        let tmp = tempfile::tempdir().unwrap();
        let site = venv(tmp.path());
        // Path-style editable (hatch / pdm / setuptools compat).
        let src = tmp.path().join("checkout").join("src");
        write(&src.join("mylib").join("__init__.py"), "");
        write(
            &site.join("mylib-0.1.0.dist-info/RECORD"),
            "_mylib.pth,,\nmylib-0.1.0.dist-info/RECORD,,\n",
        );
        write(&site.join("_mylib.pth"), &format!("{}\n", src.display()));
        // Finder-style editable (setuptools strict mode).
        write(
            &site.join("other_dist-1.0.dist-info/RECORD"),
            "__editable__.other_dist-1.0.pth,,\n__editable___other_dist_1_0_finder.py,,\n",
        );
        write(
            &site.join("__editable__.other_dist-1.0.pth"),
            "import __editable___other_dist_1_0_finder; __editable___other_dist_1_0_finder.install()\n",
        );
        write(
            &site.join("__editable___other_dist_1_0_finder.py"),
            "MAPPING: dict[str, str] = {'otherpkg': '/x/otherpkg'}\nNAMESPACES: dict[str, list[str]] = {}\n",
        );
        let roots = resolve_import_roots(tmp.path(), ["mylib", "other-dist"]);
        assert!(roots.contains(&"mylib".to_owned()), "{roots:?}");
        assert!(roots.contains(&"otherpkg".to_owned()), "{roots:?}");
    }

    #[test]
    fn egg_info_top_level_and_ancestor_venv() {
        let tmp = tempfile::tempdir().unwrap();
        // The venv lives at the workspace root; the project is nested.
        let site = venv(tmp.path());
        let project = tmp.path().join("packages").join("app");
        std::fs::create_dir_all(&project).unwrap();
        write(
            &site.join("PyYAML-6.0-py3.13.egg-info/top_level.txt"),
            "_yaml\nyaml\n",
        );
        let roots = resolve_import_roots(&project, ["pyyaml"]);
        assert!(roots.contains(&"yaml".to_owned()), "{roots:?}");
    }

    #[test]
    fn undeclared_installed_distributions_do_not_leak() {
        let tmp = tempfile::tempdir().unwrap();
        let site = venv(tmp.path());
        write(&site.join("bs4_extra-1.0.dist-info/top_level.txt"), "bs4\n");
        let roots = resolve_import_roots(tmp.path(), ["requests"]);
        assert!(!roots.contains(&"bs4".to_owned()), "{roots:?}");
    }

    /// Regression: the interpreter is spawned from a private scratch
    /// directory, so the path must be absolute even when the project root
    /// was given relatively (`tyc check` from the project directory passes
    /// `.`). A relative `./.venv/bin/python` failed to exec and every
    /// third-party module went unchecked.
    #[test]
    fn venv_paths_are_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        venv(tmp.path());
        write(&tmp.path().join(".venv/bin/python"), "");
        let found = find_venv(tmp.path()).unwrap();
        assert!(found.is_absolute());
        let py = crate::discover_python(tmp.path()).unwrap();
        assert!(py.is_absolute() && py.starts_with(tmp.path()), "{py:?}");
    }

    #[cfg(unix)]
    #[test]
    fn module_scan_survives_symlink_cycles() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("loopy");
        std::fs::create_dir_all(&pkg).unwrap();
        std::os::unix::fs::symlink(".", pkg.join("a")).unwrap();
        std::os::unix::fs::symlink(".", pkg.join("b")).unwrap();
        // No Python anywhere: the scan must terminate and report nothing.
        assert!(modules_in_dir(tmp.path()).is_empty());
        write(&pkg.join("core").join("mod.py"), "");
        assert_eq!(modules_in_dir(tmp.path()), vec!["loopy".to_owned()]);
    }
}
