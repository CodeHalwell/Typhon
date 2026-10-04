//! `tyc install` — materialise embedded tooling assets into a project.
//!
//! Today the single target is `skill`: the `typhon` Claude skill is embedded
//! into the `tyc` binary at build time via `include_str!`, so `tyc install
//! skill` can write the whole `.claude/skills/typhon/` tree into any project
//! with no network access and no dependency on the Typhon source checkout.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use miette::{miette, Result};

/// Arguments for `tyc install`.
#[derive(Args, Debug)]
pub struct InstallArgs {
    #[command(subcommand)]
    pub target: InstallTarget,
}

/// What to install.
#[derive(Subcommand, Debug)]
pub enum InstallTarget {
    /// Write the embedded `typhon` Claude skill into `.claude/skills/typhon/`.
    Skill(SkillArgs),
}

/// Arguments for `tyc install skill`.
#[derive(Args, Debug)]
pub struct SkillArgs {
    /// Project root to install into (defaults to the current directory).
    /// The skill is written to `<DIR>/.claude/skills/typhon/`.
    #[arg(long, short = 'd', value_name = "DIR", default_value = ".")]
    pub dir: PathBuf,

    /// Overwrite an existing installed skill. Without it, the command refuses
    /// when `.claude/skills/typhon/SKILL.md` already exists (so a customised
    /// copy is never clobbered); with it, the whole tree is rewritten.
    #[arg(long)]
    pub force: bool,

    /// Print the files that would be written, then exit without touching disk.
    #[arg(long)]
    pub list: bool,
}

/// The skill, embedded at build time. Each entry is a path relative to the
/// skill root paired with its file contents.
///
/// The embedded source of truth is vendored **inside this crate** at
/// `tyc/crates/tyc/skill/`, so the compiler builds standalone (a packaged
/// `tyc` crate or a checkout of just `tyc/` has everything it needs — the
/// `include_str!` paths never climb out of the workspace). The repo's own
/// `.claude/skills/typhon/` is the *installed* copy of this tree, kept in sync
/// by running `tyc install skill` at the repo root; `skill/README.md` records
/// that contract.
///
/// Paths resolve relative to *this source file*: `src/commands/` is two levels
/// below the crate root, so `../../skill/` reaches the vendored tree.
macro_rules! skill_file {
    ($rel:literal) => {
        ($rel, include_str!(concat!("../../skill/", $rel)))
    };
}

/// The full skill manifest. Adding a new sibling doc or reference example is a
/// one-line addition here.
const SKILL_FILES: &[(&str, &str)] = &[
    skill_file!("SKILL.md"),
    skill_file!("REFERENCE.md"),
    skill_file!("CLI.md"),
    skill_file!("PITFALLS.md"),
    skill_file!("DIAGNOSTICS.md"),
    skill_file!("COOKBOOK.md"),
    skill_file!("RUNTIME.md"),
    skill_file!("PACKAGING.md"),
    skill_file!("references/README.md"),
    skill_file!("references/01-hello-world.ty"),
    skill_file!("references/02-variables-and-types.ty"),
    skill_file!("references/03-control-flow.ty"),
    skill_file!("references/04-collections.ty"),
    skill_file!("references/05-functions-and-generics.ty"),
    skill_file!("references/06-classes-and-models.ty"),
    skill_file!("references/07-error-handling.ty"),
    skill_file!("references/08-sealed-unions-match.ty"),
    skill_file!("references/09-interfaces.ty"),
    skill_file!("references/10-pipes-and-guards.ty"),
    skill_file!("references/11-comptime-config.ty"),
    skill_file!("references/12-file-io-json.ty"),
    skill_file!("references/13-async-gather-and-go.ty"),
    skill_file!("references/14-newtype-ids.ty"),
    skill_file!("references/15-enums.ty"),
    skill_file!("references/16-linked-list-generics.ty"),
    skill_file!("references/17-state-machine.ty"),
    skill_file!("references/18-iterators-generators.ty"),
    skill_file!("references/19-context-managers.ty"),
    skill_file!("references/20-boundary-casts.ty"),
];

/// Where the skill lands inside a project.
const SKILL_SUBDIR: &str = ".claude/skills/typhon";

pub fn run(args: InstallArgs) -> Result<()> {
    match args.target {
        InstallTarget::Skill(skill_args) => run_skill(skill_args),
    }
}

fn run_skill(args: SkillArgs) -> Result<()> {
    let skill_root = args.dir.join(SKILL_SUBDIR);

    if args.list {
        println!(
            "{} file(s) would be written to {}:",
            SKILL_FILES.len(),
            skill_root.display()
        );
        for (rel, _) in SKILL_FILES {
            println!("  {}", rel);
        }
        return Ok(());
    }

    // Guard an existing, possibly-customised copy unless --force is set.
    let sentinel = skill_root.join("SKILL.md");
    if sentinel.exists() && !args.force {
        return Err(miette!(
            "{} already exists; re-run with --force to overwrite the installed skill",
            sentinel.display()
        ));
    }

    // Preflight the entire manifest before the first asset is written. Walk
    // from the explicit project root so a linked .claude directory cannot
    // redirect either directory creation or writes outside that root.
    std::fs::create_dir_all(&args.dir)
        .map_err(|e| miette!("cannot create {}: {e}", args.dir.display()))?;
    for (rel, _) in SKILL_FILES {
        let mut dest = args.dir.clone();
        for part in Path::new(SKILL_SUBDIR).join(rel).components() {
            dest.push(part);
            if std::fs::symlink_metadata(&dest).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(miette!(
                    "refusing to install through symlink '{}'",
                    dest.display()
                ));
            }
        }
    }

    // Create each unique destination directory once. The manifest shares a
    // handful of directories (the root and `references/`), so re-running
    // create_dir_all per file would be redundant I/O; the set keeps it general
    // for any future nested entry without re-querying directories we've made.
    let mut created_dirs: HashSet<PathBuf> = HashSet::new();
    let mut written = 0usize;
    for (rel, contents) in SKILL_FILES {
        let dest = skill_root.join(rel);
        if let Some(parent) = dest.parent() {
            if created_dirs.insert(parent.to_path_buf()) {
                std::fs::create_dir_all(parent)
                    .map_err(|e| miette!("cannot create {}: {}", parent.display(), e))?;
            }
        }
        write_file(&dest, contents)?;
        written += 1;
    }

    println!(
        "Installed the `typhon` skill ({} files) into {}",
        written,
        skill_root.display()
    );
    println!("  SKILL.md + 7 reference docs");
    println!("  references/ (index + 20 example programs)");
    println!();
    println!("Claude Code will discover it automatically as the `typhon` skill.");

    Ok(())
}

fn write_file(dest: &Path, contents: &str) -> Result<()> {
    tyc_format::atomic_write(dest, contents.as_bytes())
        .map_err(|e| miette!("cannot write {}: {}", dest.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(dir: &Path, force: bool, list: bool) -> SkillArgs {
        SkillArgs {
            dir: dir.to_path_buf(),
            force,
            list,
        }
    }

    #[cfg(unix)]
    #[test]
    fn review_symlink_preflight_preserves_every_asset_and_victim() {
        for force in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("project");
            let skill = root.join(SKILL_SUBDIR);
            std::fs::create_dir_all(&skill).unwrap();
            let victim = tmp.path().join("victim.md");
            std::fs::write(&victim, "original").unwrap();
            std::os::unix::fs::symlink(&victim, skill.join("REFERENCE.md")).unwrap();
            assert!(run_skill(args(&root, force, false)).is_err());
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
            assert!(!skill.join("SKILL.md").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn review_symlink_parent_is_refused_before_directory_creation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(".claude")).unwrap();
        assert!(run_skill(args(&root, true, false)).is_err());
        assert!(!outside.join("skills").exists());
    }

    #[test]
    fn list_does_not_touch_disk() {
        let tmp = tempfile::tempdir().unwrap();
        run_skill(args(tmp.path(), false, true)).unwrap();
        assert!(!tmp.path().join(SKILL_SUBDIR).exists());
    }

    #[test]
    fn install_writes_every_manifest_file() {
        let tmp = tempfile::tempdir().unwrap();
        run_skill(args(tmp.path(), false, false)).unwrap();
        let root = tmp.path().join(SKILL_SUBDIR);
        for (rel, _) in SKILL_FILES {
            assert!(root.join(rel).is_file(), "missing {rel}");
        }
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        run_skill(args(tmp.path(), false, false)).unwrap();
        // A second install without --force must refuse via the sentinel guard.
        assert!(run_skill(args(tmp.path(), false, false)).is_err());
    }

    #[test]
    fn force_overwrites_existing_install() {
        let tmp = tempfile::tempdir().unwrap();
        run_skill(args(tmp.path(), false, false)).unwrap();
        let sentinel = tmp.path().join(SKILL_SUBDIR).join("SKILL.md");
        std::fs::write(&sentinel, "tampered").unwrap();
        run_skill(args(tmp.path(), true, false)).unwrap();
        let restored = std::fs::read_to_string(&sentinel).unwrap();
        assert_ne!(restored, "tampered");
        assert!(restored.contains("Typhon"));
    }
}
