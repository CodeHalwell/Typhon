//! `tyc install` — materialise embedded tooling assets into a project.
//!
//! Today the single target is `skill`: the `typhon` Claude skill is embedded
//! into the `tyc` binary at build time via `include_str!`, so `tyc install
//! skill` can write the whole `.claude/skills/typhon/` tree into any project
//! with no network access and no dependency on the Typhon source checkout.

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

    /// Overwrite files that already exist.
    #[arg(long)]
    pub force: bool,

    /// Print the files that would be written, then exit without touching disk.
    #[arg(long)]
    pub list: bool,
}

/// The skill, embedded at build time. Each entry is a path relative to the
/// skill root (`.claude/skills/typhon/`) paired with its file contents.
///
/// Paths are resolved relative to *this source file* by `include_str!`, i.e.
/// `tyc/crates/tyc/src/commands/install.rs` → repo root is five levels up.
macro_rules! skill_file {
    ($rel:literal) => {
        (
            $rel,
            include_str!(concat!("../../../../../.claude/skills/typhon/", $rel)),
        )
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

    let mut written = 0usize;
    for (rel, contents) in SKILL_FILES {
        let dest = skill_root.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| miette!("cannot create {}: {}", parent.display(), e))?;
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
    std::fs::write(dest, contents).map_err(|e| miette!("cannot write {}: {}", dest.display(), e))
}
