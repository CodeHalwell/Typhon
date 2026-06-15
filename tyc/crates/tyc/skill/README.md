# Vendored `typhon` skill (embedded source of truth)

This directory is the **source of truth** for the `typhon` Claude skill that
`tyc install skill` writes into a project. It lives inside the `tyc` crate so
the compiler builds standalone — `include_str!` in
`../src/commands/install.rs` only ever reads paths under the crate, never out
into the wider monorepo.

The set of files embedded into the binary is the manifest in
`../src/commands/install.rs` (`SKILL_FILES`): `SKILL.md`, the seven sibling
reference docs, and everything under `references/`. This `README.md` is *not*
embedded — it documents the directory, it isn't part of the installed skill.

## Relationship to `.claude/skills/typhon/`

The repository's own `.claude/skills/typhon/` is the **installed copy** of this
tree — what Claude Code discovers when working in the Typhon repo. It is
regenerated from here:

```bash
# from the repo root, after editing files in this directory and rebuilding tyc
tyc install skill --force
```

So the workflow is: **edit the skill here**, rebuild `tyc` (re-embeds the
change), then `tyc install skill --force` to refresh `.claude/skills/typhon/`.
The two trees should stay byte-identical; a divergence means someone edited the
installed copy directly instead of this source.
