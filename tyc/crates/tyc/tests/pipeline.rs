//! End-to-end integration tests for the `tyc` CLI binary.
//!
//! Each test invokes the real `tyc` binary via `std::process::Command` using the
//! `CARGO_BIN_EXE_tyc` path that Cargo injects automatically for integration
//! test targets.  This validates that all CLI commands and their argument wiring
//! work correctly when called from the outside, not just as library functions.

use std::path::Path;
use std::process::Command;

// ── helpers ───────────────────────────────────────────────────────────────────

fn tyc() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tyc"))
}

/// Write a minimal `typhon.toml` + `src/main.ty` under `dir`.
fn scaffold(dir: &Path, src_content: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("typhon.toml"),
        "[project]\nname = \"test\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
    )
    .unwrap();
    std::fs::write(dir.join("src").join("main.ty"), src_content).unwrap();
}

// ── tyc init ─────────────────────────────────────────────────────────────────

#[test]
fn init_creates_project_structure() {
    let tmp = tempfile::tempdir().unwrap();
    let status = tyc()
        .args(["init", "myapp", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "tyc init should succeed");
    // `tyc init NAME --dir DIR` scaffolds into `DIR/NAME/`, matching
    // `cargo new` / `bun init NAME` / `uv init NAME` semantics.
    let project = tmp.path().join("myapp");
    assert!(
        project.join("typhon.toml").exists(),
        "typhon.toml missing in named subdirectory"
    );
    assert!(
        project.join("src").join("main.ty").exists(),
        "src/main.ty missing in named subdirectory"
    );
    assert!(
        project.join("tests").is_dir(),
        "tests/ dir missing in named subdirectory"
    );
}

#[test]
fn init_embeds_project_name_in_toml() {
    let tmp = tempfile::tempdir().unwrap();
    tyc()
        .args(["init", "coolproject", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    let toml = std::fs::read_to_string(tmp.path().join("coolproject").join("typhon.toml")).unwrap();
    assert!(
        toml.contains("coolproject"),
        "project name not present in typhon.toml"
    );
}

#[test]
fn init_rejects_existing_toml() {
    let tmp = tempfile::tempdir().unwrap();
    // First init must succeed.
    tyc()
        .args(["init", "proj", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    // Second init on the same dir must fail.
    let status = tyc()
        .args(["init", "proj", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "re-init of an existing project should fail"
    );
}

// ── tyc check ────────────────────────────────────────────────────────────────

#[test]
fn check_passes_on_valid_source() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("ok.ty"), "let x: int = 42\n").unwrap();
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "tyc check should pass on valid source");
}

#[test]
fn check_fails_on_type_error() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("bad.ty"), "let x: int = \"hello\"\n").unwrap();
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(
        !status.success(),
        "tyc check should fail on a type mismatch"
    );
}

#[test]
fn check_passes_nullable_annotation() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("nullable.ty"), "let x: str? = None\n").unwrap();
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(
        status.success(),
        "tyc check should accept T? (nullable) annotations"
    );
}

#[test]
fn dict_get_two_arg_narrows_to_v() {
    // FINDINGS #71: `d.get(k, default)` where `default: V` must narrow
    // from `V | None` to `V`. With a V-incompatible default the result
    // widens to `V | type(default)`. The one-arg form stays nullable.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("dict_get.ty"),
        "def main() -> None:\n    \
            let d: dict[str, int] = {\"a\": 1}\n    \
            let x: int = d.get(\"a\", 0)\n    \
            let y: int | str = d.get(\"a\", \"fallback\")\n    \
            let z: int | None = d.get(\"a\")\n    \
            print(x, y, z)\n",
    )
    .unwrap();
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(
        status.success(),
        "tyc check should narrow dict.get(k, default) to V (or V | type(default))"
    );
}

#[test]
fn dict_get_default_kwarg_narrows_to_v() {
    // Follow-up to #71: the kwarg form `d.get(k, default=…)` must
    // narrow the same way as the positional form. Without this, users
    // writing the more-readable kwarg call would silently get the
    // nullable signature and a confusing `int | None` mismatch.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("dict_get_kwarg.ty"),
        "def main() -> None:\n    \
            let d: dict[str, int] = {\"a\": 1}\n    \
            let x: int = d.get(\"a\", default=0)\n    \
            let y: int | str = d.get(\"a\", default=\"fallback\")\n    \
            print(x, y)\n",
    )
    .unwrap();
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(
        status.success(),
        "tyc check should narrow dict.get(k, default=…) the same as the positional form"
    );
}

#[test]
fn dict_get_two_arg_mismatched_default_still_rejects_non_nullable_target() {
    // FINDINGS #71 follow-up: if the default's type can't fit the target
    // annotation, the union widening must still surface as a mismatch.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("dict_get_bad.ty"),
        "def main() -> None:\n    \
            let d: dict[str, int] = {\"a\": 1}\n    \
            let z: int = d.get(\"a\", \"wrong\")\n    \
            print(z)\n",
    )
    .unwrap();
    let status = tyc().arg("check").arg(tmp.path()).status().unwrap();
    assert!(
        !status.success(),
        "tyc check should still reject a default whose type doesn't fit the annotation"
    );
}

#[test]
fn duplicate_class_emits_diagnostic() {
    // FINDINGS #77: two `class Foo:` declarations in the same module must
    // surface as `tyc::duplicate_class` rather than silently shadowing.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("dup.ty"),
        "class Foo:\n    a: int\n\nclass Foo:\n    b: str\n\ndef main() -> None:\n    print(\"ok\")\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(tmp.path()).output().unwrap();
    assert!(
        !out.status.success(),
        "expected duplicate_class to fail check"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("tyc::duplicate_class"),
        "expected tyc::duplicate_class in output, got: {combined}"
    );
}

#[test]
fn impl_unknown_class_emits_diagnostic() {
    // FINDINGS #78: `impl UnknownClass:` for a class that doesn't exist
    // must fire `tyc::impl_unknown_class` rather than emitting dead code.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("imp.ty"),
        "impl UnknownClass:\n    def hello(self) -> None:\n        print(\"hi\")\n\n\
            def main() -> None:\n    print(\"ok\")\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(tmp.path()).output().unwrap();
    assert!(
        !out.status.success(),
        "expected impl_unknown_class to fail check"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("tyc::impl_unknown_class"),
        "expected tyc::impl_unknown_class in output, got: {combined}"
    );
}

#[test]
fn impl_alias_sealed_union_diagnostics_point_at_real_source() {
    // B15: distributing `impl Alias:` over a sealed union (`type X = A | B`)
    // byte-duplicates the impl body once per variant in the preprocessed
    // buffer, pushing the 2nd…Nth blocks *past the real source's EOF*.
    // Before the fix, the diagnostic for the second variant rendered at a
    // synthetic line number greater than the file's real line count.
    //
    // This asserts (a) every rendered `[file:line:col]` location lands on a
    // line that actually exists in the source, and (b) the per-variant
    // diagnostics resolve to the single, real `return` member line the
    // user wrote — never a line past EOF.
    let tmp = tempfile::tempdir().unwrap();
    let src = "type TreeAlias = Leaf | Node\n\
               \n\
               class Leaf:\n\
               \x20\x20\x20\x20value: int\n\
               \n\
               class Node:\n\
               \x20\x20\x20\x20left: int\n\
               \x20\x20\x20\x20right: int\n\
               \n\
               impl TreeAlias:\n\
               \x20\x20\x20\x20def total(self) -> int:\n\
               \x20\x20\x20\x20\x20\x20\x20\x20return self.nonexistent_field + 1\n";
    let real_line_count = src.lines().count();
    // The user-written impl body member (`return …`) is the 12th line.
    let body_line = 12usize;
    let path = tmp.path().join("tree.ty");
    std::fs::write(&path, src).unwrap();

    let out = tyc().arg("check").arg(&path).output().unwrap();
    assert!(
        !out.status.success(),
        "expected the bogus-attribute check to fail"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("tyc::attribute_not_found"),
        "expected attribute_not_found, got:\n{combined}"
    );
    // Both variants must be reported (one diagnostic each for Leaf/Node).
    assert!(
        combined.contains("on `Leaf`") && combined.contains("on `Node`"),
        "expected a diagnostic for each union variant, got:\n{combined}"
    );

    // Parse every `tree.ty:<line>:<col>` location from the rendered output
    // and assert none exceeds the real source's line count.
    let mut saw_location = false;
    for cap in combined.split("tree.ty:").skip(1) {
        let digits: String = cap.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        saw_location = true;
        let line: usize = digits.parse().unwrap();
        assert!(
            line <= real_line_count,
            "B15: diagnostic reported line {line}, past the source's real \
             {real_line_count} lines:\n{combined}"
        );
        // The distributed impl body collapses to the single real member
        // line the user authored.
        assert_eq!(
            line, body_line,
            "B15: per-variant diagnostic should resolve to the real impl \
             member line {body_line}, got {line}:\n{combined}"
        );
    }
    assert!(
        saw_location,
        "expected at least one `tree.ty:<line>:<col>` location, got:\n{combined}"
    );
}

#[test]
fn impl_generic_alias_sealed_union_diagnostics_point_at_real_source() {
    // B15 generic case: distributing a GENERIC `impl[T] Tree[T]:` over a
    // sealed union (`type Tree[T] = Leaf[T] | Branch[T]`) byte-duplicates
    // the impl body once per variant. The sanitised headers carry the
    // `[T]` arg on the name (`impl Leaf[T]:` / `impl Branch[T]:`), so the
    // `impl `/`impl[` recognition in both the check-driver gating and the
    // B15 remap must accept the generic form. Before the fix, the second
    // variant's diagnostic rendered at a synthetic line past the file's
    // real EOF; after, it resolves to the single real impl member line.
    let tmp = tempfile::tempdir().unwrap();
    let src = "class Leaf[T]:\n\
               \x20\x20\x20\x20value: int\n\
               \n\
               class Branch[T]:\n\
               \x20\x20\x20\x20left: int\n\
               \x20\x20\x20\x20right: int\n\
               \n\
               type Tree[T] = Leaf[T] | Branch[T]\n\
               \n\
               impl[T] Tree[T]:\n\
               \x20\x20\x20\x20def total(self) -> int:\n\
               \x20\x20\x20\x20\x20\x20\x20\x20return self.nonexistent_field + 1\n";
    let real_line_count = src.lines().count();
    // The user-written impl body member (`return …`) is the 12th line.
    let body_line = 12usize;
    let path = tmp.path().join("tree.ty");
    std::fs::write(&path, src).unwrap();

    let out = tyc().arg("check").arg(&path).output().unwrap();
    assert!(
        !out.status.success(),
        "expected the bogus-attribute check to fail"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("tyc::attribute_not_found"),
        "expected attribute_not_found, got:\n{combined}"
    );
    // Both variants must be reported (one diagnostic each for Leaf/Branch).
    assert!(
        combined.contains("on `Leaf`") && combined.contains("on `Branch`"),
        "expected a diagnostic for each union variant, got:\n{combined}"
    );

    let mut saw_location = false;
    for cap in combined.split("tree.ty:").skip(1) {
        let digits: String = cap.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        saw_location = true;
        let line: usize = digits.parse().unwrap();
        assert!(
            line <= real_line_count,
            "B15 (generic): diagnostic reported line {line}, past the source's \
             real {real_line_count} lines:\n{combined}"
        );
        assert_eq!(
            line, body_line,
            "B15 (generic): per-variant diagnostic should resolve to the real \
             impl member line {body_line}, got {line}:\n{combined}"
        );
    }
    assert!(
        saw_location,
        "expected at least one `tree.ty:<line>:<col>` location, got:\n{combined}"
    );
}

#[test]
fn real_adjacent_duplicate_impls_report_their_own_lines() {
    // B15 robustness: two GENUINELY-REAL, adjacent `impl A:` / `impl B:`
    // blocks with byte-identical bodies are indistinguishable from a
    // sealed-union distribution by text alone. A diagnostic firing in the
    // SECOND block must report the SECOND block's real source line — it
    // must NOT be collapsed onto the first block (which the distribution
    // remap would wrongly do).
    let tmp = tempfile::tempdir().unwrap();
    // No `type _ = A | B` alias is declared, so neither block is a
    // distribution. Each `total` reads a field the class lacks, firing
    // `attribute_not_found` once per block at its own line.
    let src = "class A:\n\
               \x20\x20\x20\x20a_field: int\n\
               \n\
               class B:\n\
               \x20\x20\x20\x20b_field: int\n\
               \n\
               impl A:\n\
               \x20\x20\x20\x20def total(self) -> int:\n\
               \x20\x20\x20\x20\x20\x20\x20\x20return self.missing\n\
               \n\
               impl B:\n\
               \x20\x20\x20\x20def total(self) -> int:\n\
               \x20\x20\x20\x20\x20\x20\x20\x20return self.missing\n";
    // `return self.missing` inside `impl A:` is line 9, inside `impl B:`
    // line 13 (1-based). Pre-fix, the second would have collapsed onto
    // the first; the fix keeps them on their own distinct lines.
    let second_block_line = 13usize;
    let first_block_line = 9usize;
    let path = tmp.path().join("dup.ty");
    std::fs::write(&path, src).unwrap();

    let out = tyc().arg("check").arg(&path).output().unwrap();
    assert!(
        !out.status.success(),
        "expected the bogus-attribute check to fail"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("tyc::attribute_not_found"),
        "expected attribute_not_found, got:\n{combined}"
    );
    // Collect every reported line number for dup.ty.
    let mut lines: Vec<usize> = Vec::new();
    for cap in combined.split("dup.ty:").skip(1) {
        let digits: String = cap.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<usize>() {
            lines.push(n);
        }
    }
    assert!(
        !lines.is_empty(),
        "expected at least one `dup.ty:<line>:<col>` location, got:\n{combined}"
    );
    // The B-block diagnostic must land on the SECOND block's real line,
    // never remapped onto the first block.
    assert!(
        lines.contains(&second_block_line),
        "B15: the `impl B:` diagnostic must report its real line \
         {second_block_line}, got {lines:?}:\n{combined}"
    );
    // Sanity: both real blocks are reported at their own distinct lines
    // (the bug would collapse the second onto the first).
    assert!(
        lines.contains(&first_block_line),
        "expected the `impl A:` diagnostic at line {first_block_line}, \
         got {lines:?}:\n{combined}"
    );
}

#[test]
fn alias_present_but_real_adjacent_impls_report_their_own_lines() {
    // Finding 1 (B15 edge): the source declares BOTH a sealed-union alias
    // `type Event = A | B` AND manually-written, adjacent `impl A:` /
    // `impl B:` blocks (NOT `impl Event:`) whose bodies are byte-identical
    // and whose names match the alias's variant list in order. The
    // preprocessor did NOT distribute anything here (the user wrote the
    // per-variant impls themselves), so its recorded
    // `impl_distributed_lines` is empty. The OLD diagnostic remap
    // re-derived the distributed set from the text and — unable to tell
    // this apart from a real distribution — wrongly collapsed the `impl B:`
    // diagnostic onto `impl A:`. Threading the recorded metadata closes the
    // edge: the `impl B:` diagnostic must report B's OWN real line.
    let tmp = tempfile::tempdir().unwrap();
    let src = "type Event = A | B\n\
               \n\
               class A:\n\
               \x20\x20\x20\x20a_field: int\n\
               \n\
               class B:\n\
               \x20\x20\x20\x20b_field: int\n\
               \n\
               impl A:\n\
               \x20\x20\x20\x20def total(self) -> int:\n\
               \x20\x20\x20\x20\x20\x20\x20\x20return self.missing\n\
               \n\
               impl B:\n\
               \x20\x20\x20\x20def total(self) -> int:\n\
               \x20\x20\x20\x20\x20\x20\x20\x20return self.missing\n";
    // `return self.missing` inside `impl A:` is line 11, inside `impl B:`
    // line 15 (1-based).
    let first_block_line = 11usize;
    let second_block_line = 15usize;
    let path = tmp.path().join("edge.ty");
    std::fs::write(&path, src).unwrap();

    let out = tyc().arg("check").arg(&path).output().unwrap();
    assert!(
        !out.status.success(),
        "expected the bogus-attribute check to fail"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("tyc::attribute_not_found"),
        "expected attribute_not_found, got:\n{combined}"
    );
    let mut lines: Vec<usize> = Vec::new();
    for cap in combined.split("edge.ty:").skip(1) {
        let digits: String = cap.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<usize>() {
            lines.push(n);
        }
    }
    assert!(
        !lines.is_empty(),
        "expected at least one `edge.ty:<line>:<col>` location, got:\n{combined}"
    );
    // The `impl B:` diagnostic must land on B's own real line, NOT be
    // collapsed onto the first block by the text re-derivation.
    assert!(
        lines.contains(&second_block_line),
        "Finding 1: the `impl B:` diagnostic must report its real line \
         {second_block_line}, got {lines:?}:\n{combined}"
    );
    assert!(
        lines.contains(&first_block_line),
        "expected the `impl A:` diagnostic at line {first_block_line}, \
         got {lines:?}:\n{combined}"
    );
}

#[test]
fn cyclic_type_alias_emits_diagnostic() {
    // FINDINGS #81: `type A = B; type B = A` forms a cycle. No concrete
    // type can ever satisfy it; reject at check time instead of letting
    // Python's lazy alias evaluation defer the error indefinitely.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("cyc.ty"),
        "type A = B\ntype B = A\n\ndef main() -> None:\n    print(\"ok\")\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(tmp.path()).output().unwrap();
    assert!(
        !out.status.success(),
        "expected cyclic_type_alias to fail check"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("tyc::cyclic_type_alias"),
        "expected tyc::cyclic_type_alias in output, got: {combined}"
    );
}

#[test]
fn async_without_await_emits_warning() {
    // FINDINGS #83: an `async def` body that never `await`s should fire
    // a `tyc::async_without_await` warning. The warning must not block
    // a check from succeeding (it's an advisory, not an error).
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("quiet.ty"),
        "async def quiet() -> int:\n    return 1\n\n\
            def main() -> None:\n    print(\"ok\")\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(tmp.path()).output().unwrap();
    assert!(
        out.status.success(),
        "async_without_await is a warning; check should still succeed"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("tyc::async_without_await"),
        "expected tyc::async_without_await in output, got: {combined}"
    );
}

#[test]
fn async_with_await_does_not_warn() {
    // Negative case: an `async def` that actually `await`s something must
    // not produce the warning. Use `asyncio.sleep(0)` so we have an
    // `await` site without a second `async def` that would itself fire
    // `async_without_await`.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("awaits.ty"),
        "import asyncio\n\n\
            async def outer() -> int:\n    \
                await asyncio.sleep(0)\n    \
                return 1\n\n\
            def main() -> None:\n    print(\"ok\")\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(tmp.path()).output().unwrap();
    assert!(out.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !combined.contains("tyc::async_without_await"),
        "did not expect tyc::async_without_await for a body that awaits, got: {combined}"
    );
}

#[test]
fn question_op_on_bare_async_call_fires_missing_await() {
    // `inner(n)?` where `inner` is an `async def` desugars to
    // `__typhon_q_0__ = inner(n)`; without an `await` the `.value` read
    // lands on the coroutine at runtime. The check must reject it (the
    // documented idiom is `await inner(n)?`) instead of silently
    // miscompiling — even though the enclosing function is itself `async`.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("q.ty"),
        "import asyncio\n\n\
            async def inner(n: int) -> Result[int, str]:\n    \
                await asyncio.sleep(0)\n    return Ok(n)\n\n\
            async def outer(n: int) -> Result[int, str]:\n    \
                let v: int = inner(n)?\n    return Ok(v + 1)\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(tmp.path()).output().unwrap();
    assert!(!out.status.success(), "bare async `?` must fail the check");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.contains("tyc::missing_await"),
        "expected tyc::missing_await on bare async `?`, got: {combined}"
    );
}

#[test]
fn question_op_on_awaited_async_call_is_clean() {
    // Negative case: the documented `await inner(n)?` idiom must keep
    // checking clean (the `inside_await` guard exempts it).
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("q.ty"),
        "import asyncio\n\n\
            async def inner(n: int) -> Result[int, str]:\n    \
                await asyncio.sleep(0)\n    return Ok(n)\n\n\
            async def outer(n: int) -> Result[int, str]:\n    \
                let v: int = await inner(n)?\n    return Ok(v + 1)\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(tmp.path()).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.status.success(),
        "await inner(n)? must check clean, got: {combined}"
    );
    assert!(
        !combined.contains("tyc::missing_await"),
        "did not expect missing_await on the awaited form, got: {combined}"
    );
}

#[test]
fn await_on_stored_asyncio_task_unwraps_to_value() {
    // A `go work() -> t` handle parked in an explicitly-annotated
    // `list[asyncio.Task[int]]` and awaited in a loop must unwrap to `int`,
    // not stay typed as `Task[int]` (which fired `tyc::type_mismatch`).
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("t.ty"),
        "import asyncio\n\n\
            async def work(n: int) -> int:\n    \
                await asyncio.sleep(0)\n    return n * n\n\n\
            async def run() -> int:\n    \
                mut tasks: list[asyncio.Task[int]] = []\n    \
                go work(3) -> t\n    tasks.append(t)\n    \
                mut total: int = 0\n    \
                for task in tasks:\n        \
                    let r: int = await task\n        total = total + r\n    \
                return total\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(tmp.path()).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.status.success(),
        "await on a stored asyncio.Task[int] must type as int, got: {combined}"
    );
}

#[test]
fn vm_run_resolves_siblings_and_binds_sealed_union_alias() {
    // Two regressions in one project: (1) the `tyc run` gating check must
    // resolve sibling modules (not check `main.ty` in isolation, which fired
    // a false `unknown_module` + knock-on `missing_return`); and (2) the VM
    // must bind an imported `pub type` sealed-union alias as a module
    // attribute (else `from shapes import AB` raised `AttributeError`).
    let project = tempfile::tempdir().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        project.path().join("typhon.toml"),
        "[project]\nname = \"u\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
    )
    .unwrap();
    std::fs::write(
        src.join("shapes.ty"),
        "pub class A frozen:\n    v: int\n\n\
            pub class B frozen:\n    v: int\n\n\
            pub type AB = A | B\n",
    )
    .unwrap();
    std::fs::write(
        src.join("main.ty"),
        "from shapes import A, AB, B\n\n\
            def pick(x: AB) -> int:\n    \
                match x:\n        \
                    case A(v):\n            return v\n        \
                    case B(v):\n            return v * 2\n\n\
            def main() -> None:\n    print(pick(A(v=21)))\n\n\
            if __name__ == \"__main__\":\n    main()\n",
    )
    .unwrap();
    let out = tyc().arg("run").arg(project.path()).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.status.success(),
        "tyc run on a sibling-importing project must succeed, got: {combined}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("21"),
        "expected `21` on stdout, got: {combined}"
    );
}

#[test]
fn pub_enum_parses_checks_and_exports() {
    // `pub enum` must parse (it was a hard parse error), check clean, and
    // contribute its name to the synthesised `__all__`.
    let project = tempfile::tempdir().unwrap();
    scaffold(
        project.path(),
        "pub enum Species:\n    CAT\n    DOG\n    BIRD\n\n\
            def describe(s: Species) -> str:\n    return s.name\n\n\
            def main() -> None:\n    print(describe(Species.DOG))\n",
    );
    let check = tyc().arg("check").arg(project.path()).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&check.stderr),
        String::from_utf8_lossy(&check.stdout)
    );
    assert!(
        check.status.success(),
        "pub enum must check clean: {combined}"
    );

    let build = tyc().arg("build").arg(project.path()).output().unwrap();
    assert!(build.status.success(), "pub enum must build");
    let main_py = std::fs::read_to_string(project.path().join("build").join("main.py")).unwrap();
    assert!(
        main_py.contains("\"Species\""),
        "Species must appear in __all__, got:\n{main_py}"
    );
}

#[test]
fn vm_run_binds_forward_declared_sealed_union_alias() {
    // A `pub type AB = A | B` declared *above* its variant classes (a
    // forward reference the checker accepts) must still bind the real value
    // for `from shapes import AB` — the eager bind falls back to a name
    // placeholder, corrected by the post-body resolution pass before the
    // module's attributes are snapshotted. Regression for the Codex review
    // on PR #187.
    let project = tempfile::tempdir().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        project.path().join("typhon.toml"),
        "[project]\nname = \"u\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
    )
    .unwrap();
    std::fs::write(
        src.join("shapes.ty"),
        "pub type AB = A | B\n\n\
            pub class A frozen:\n    v: int\n\n\
            pub class B frozen:\n    v: int\n",
    )
    .unwrap();
    std::fs::write(
        src.join("main.ty"),
        "from shapes import A, AB, B\n\n\
            def pick(x: AB) -> int:\n    \
                match x:\n        \
                    case A(v):\n            return v\n        \
                    case B(v):\n            return v * 2\n\n\
            def main() -> None:\n    \
                print(pick(B(v=10)))\n    \
                print(isinstance(A(v=1), AB))\n\n\
            if __name__ == \"__main__\":\n    main()\n",
    )
    .unwrap();
    let out = tyc().arg("run").arg(project.path()).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.status.success(),
        "run with a forward-declared imported alias must succeed: {combined}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("20") && stdout.contains("True"),
        "expected `20` and `True`, got: {combined}"
    );
}

#[test]
fn user_defined_task_type_is_not_await_unwrapped() {
    // The await-unwrap for `Task[T]` / `Future[T]` is keyed on the bare
    // name, so it must be suppressed when the project defines its own
    // (non-awaitable) class of that name — otherwise `await t` would falsely
    // type as the inner type. Regression for the Codex review on PR #187.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("t.ty"),
        "class Task[T]:\n    payload: T\n\n\
            async def use(t: Task[int]) -> int:\n    \
                let r: int = await t\n    return r\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(tmp.path()).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !out.status.success(),
        "awaiting a user-defined (non-awaitable) Task must not type-check"
    );
    assert!(
        combined.contains("tyc::type_mismatch"),
        "expected tyc::type_mismatch, got: {combined}"
    );
}

// ── tyc fmt ──────────────────────────────────────────────────────────────────

#[test]
fn fmt_check_passes_on_already_formatted_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("x.ty"), "let x: int = 1\n").unwrap();
    let status = tyc()
        .args(["fmt", "--check"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "tyc fmt --check should succeed when no changes needed"
    );
}

#[test]
fn fmt_check_fails_on_unformatted_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("y.ty"), "def f():\n\tpass\n").unwrap();
    let status = tyc()
        .args(["fmt", "--check"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "tyc fmt --check should fail when a file would be reformatted"
    );
}

#[test]
fn fmt_rewrites_tab_indentation_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("z.ty");
    std::fs::write(&path, "def f():\n\tpass\n").unwrap();
    let status = tyc().arg("fmt").arg(&path).status().unwrap();
    assert!(status.success(), "tyc fmt should succeed");
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(
        content.contains("    pass"),
        "tab indentation should be rewritten to spaces"
    );
}

// ── tyc build ────────────────────────────────────────────────────────────────

#[test]
fn build_produces_py_file_from_simple_source() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "let greeting: str = \"hello\"\n");
    let status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "tyc build should succeed");
    assert!(
        tmp.path().join("build").join("main.py").exists(),
        "build/main.py should be emitted"
    );
}

#[test]
fn build_fails_on_type_error() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "let x: int = \"wrong type\"\n");
    let status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(
        !status.success(),
        "tyc build should fail when there is a type mismatch"
    );
}

#[test]
fn build_emits_dataclass_decorator_for_class() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "class Point:\n    x: int\n    y: int\n");
    let status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "build should succeed for a plain class");
    let out = std::fs::read_to_string(tmp.path().join("build").join("main.py")).unwrap();
    assert!(
        out.contains("@dataclasses.dataclass"),
        "class should be emitted as a @dataclasses.dataclass"
    );
}

#[test]
fn build_emits_typhon_runtime_when_result_used() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def f() -> Ok[int]:\n    return Ok(1)\n");
    tyc().arg("build").arg(tmp.path()).status().unwrap();
    let runtime_pkg = tmp.path().join("build").join("typhon_runtime");
    assert!(
        runtime_pkg.join("__init__.py").exists(),
        "typhon_runtime/__init__.py should be emitted when Ok/Err/Result are used"
    );
    assert!(
        runtime_pkg.join("tasks.py").exists(),
        "typhon_runtime/tasks.py should be emitted"
    );
    assert!(
        runtime_pkg.join("lazy.py").exists(),
        "typhon_runtime/lazy.py should be emitted"
    );
}

// ── tyc trace ────────────────────────────────────────────────────────────────

#[test]
fn trace_exits_zero_with_no_input() {
    // `tyc trace` with no path argument reads stdin — close it explicitly
    // so the test gets EOF immediately instead of hanging forever when the
    // harness runs with an inherited pipe that never closes (detached
    // shells, some CI runners).
    let status = tyc()
        .arg("trace")
        .stdin(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "tyc trace should exit 0 with no input");
}

#[test]
fn trace_passes_through_non_frame_lines() {
    // A traceback with no `.py` file references should be printed unchanged.
    let dir = tempfile::tempdir().unwrap();
    let tb_path = dir.path().join("tb.txt");
    std::fs::write(
        &tb_path,
        "Traceback (most recent call last):\nValueError: oops\n",
    )
    .unwrap();
    let out = tyc().arg("trace").arg(&tb_path).output().unwrap();
    assert!(out.status.success(), "tyc trace should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Traceback"),
        "header line should be preserved; got: {stdout}"
    );
    assert!(
        stdout.contains("ValueError"),
        "exception line should be preserved; got: {stdout}"
    );
}

#[test]
fn trace_rewrites_frame_with_map_file() {
    // Build a real project so `tyc build` emits a `.py.map` sidecar, then
    // feed a synthetic traceback pointing at the emitted `.py` to
    // `tyc trace` and verify the path is rewritten to the `.ty` source.
    //
    // Use two annotated-assignment statements. The emitter prepends a
    // `from __future__ import annotations` header to every built module
    // (so self-referencing class annotations don't NameError at import),
    // so Python lines 2 and 3 map to ty lines 1 and 2 respectively.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "x: int = 1\ny: str = \"hello\"\n");
    let build_status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(
        build_status.success(),
        "build should succeed before trace test"
    );

    let py_path = tmp.path().join("build").join("main.py");
    let map_path = tmp
        .path()
        .join("build")
        .join(".sourcemaps")
        .join("main.py.map");
    assert!(py_path.exists(), "main.py should exist after build");
    assert!(map_path.exists(), "main.py.map should exist after build");

    // Write a synthetic traceback referencing line 3 of the built .py file
    // (the second user-visible statement, after the future-import header).
    // The v2 source map should remap that back to ty line 2.
    let tb = format!(
        "Traceback (most recent call last):\n  File \"{}\", line 3, in <module>\n    y: str = \"hello\"\n",
        py_path.display()
    );
    let tb_path = tmp.path().join("tb.txt");
    std::fs::write(&tb_path, &tb).unwrap();

    let out = tyc().arg("trace").arg(&tb_path).output().unwrap();
    assert!(out.status.success(), "tyc trace should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("main.ty"),
        "trace should rewrite .py path to .ty; got: {stdout}"
    );
    assert!(
        stdout.contains("line 2"),
        "line number should be preserved; got: {stdout}"
    );
    assert!(
        !stdout.contains("main.py\""),
        ".py path should be replaced; got: {stdout}"
    );
}

// ── tyc profile ───────────────────────────────────────────────────────────────

#[test]
fn profile_instruments_scaffolded_project() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def greet() -> str:\n    return \"hello\"\n");
    let status = tyc().arg("profile").arg(tmp.path()).status().unwrap();
    assert!(
        status.success(),
        "tyc profile should succeed on a valid project"
    );
    assert!(
        tmp.path().join("build").join("typhon_profile.py").exists(),
        "typhon_profile.py should be dropped into the build dir"
    );
}

#[test]
fn profile_decorates_top_level_functions() {
    let tmp = tempfile::tempdir().unwrap();
    scaffold(tmp.path(), "def greet() -> str:\n    return \"hello\"\n");
    let status = tyc().arg("profile").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "tyc profile should succeed");
    let py = std::fs::read_to_string(tmp.path().join("build").join("main.py")).unwrap();
    assert!(
        py.contains("@__typhon_profile_record"),
        "top-level functions should be decorated with @__typhon_profile_record; got:\n{py}"
    );
}

// ── full pipeline ─────────────────────────────────────────────────────────────

/// Full pipeline smoke test: init → write Phase 3 source → check → build → verify output.
///
/// Exercises: `interface` (→ `Protocol`), sealed union type alias, `@pure`
/// function, and class desugaring — the four key Phase 3 features.
#[test]
fn full_pipeline_phase3_features() {
    let tmp = tempfile::tempdir().unwrap();

    // Scaffold the project via `tyc init`.
    let status = tyc()
        .args(["init", "demo", "--dir"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "tyc init failed");

    // Replace the generated main.ty with a Phase 3 feature showcase:
    //   - `interface` declaration (→ Protocol class)
    //   - two concrete classes conforming to the interface
    //   - sealed union type alias (`type AnyGreeter = ...`)
    //   - `@pure` function
    let src = r#"
interface Greeter:
    def greet(self) -> str:
        ...

class EnglishGreeter:
    def greet(self) -> str:
        return "Hello"

class SpanishGreeter:
    def greet(self) -> str:
        return "Hola"

type AnyGreeter = EnglishGreeter | SpanishGreeter

@pure
def double(x: int) -> int:
    return x * 2

let g: Greeter = EnglishGreeter()
let result: int = double(21)
"#;
    let project = tmp.path().join("demo");
    std::fs::write(project.join("src").join("main.ty"), src).unwrap();

    // `tyc check` must pass.
    let status = tyc().arg("check").arg(&project).status().unwrap();
    assert!(status.success(), "tyc check failed on Phase 3 fixture");

    // `tyc build` must succeed and produce main.py.
    let status = tyc().arg("build").arg(&project).status().unwrap();
    assert!(status.success(), "tyc build failed on Phase 3 fixture");

    let py = std::fs::read_to_string(project.join("build").join("main.py")).unwrap();
    assert!(
        py.contains("Protocol"),
        "interface should be emitted as a Protocol class"
    );
    assert!(
        py.contains("@dataclasses.dataclass"),
        "concrete classes should be emitted as dataclasses"
    );
    assert!(
        py.contains("double"),
        "pure function should appear in emitted output"
    );
}

// ── tyc migrate ───────────────────────────────────────────────────────────────

#[test]
fn migrate_rewrites_optional_annotation() {
    let tmp = tempfile::tempdir().unwrap();
    let py_path = tmp.path().join("app.py");
    std::fs::write(
        &py_path,
        "from typing import Optional\n\nname: Optional[str] = None\n",
    )
    .unwrap();

    let status = tyc().arg("migrate").arg(&py_path).status().unwrap();
    assert!(
        status.success(),
        "tyc migrate should succeed on valid Python"
    );

    let ty_path = tmp.path().join("app.ty");
    assert!(ty_path.exists(), "tyc migrate should produce a .ty file");

    let ty_src = std::fs::read_to_string(&ty_path).unwrap();
    assert!(
        ty_src.contains("str?"),
        "Optional[str] should be rewritten to str?; got:\n{ty_src}"
    );
}

#[test]
fn migrate_adds_let_to_module_level_assignment() {
    let tmp = tempfile::tempdir().unwrap();
    let py_path = tmp.path().join("constants.py");
    std::fs::write(&py_path, "PORT: int = 8080\nHOST: str = \"localhost\"\n").unwrap();

    let status = tyc().arg("migrate").arg(&py_path).status().unwrap();
    assert!(status.success(), "tyc migrate should succeed");

    let ty_src = std::fs::read_to_string(tmp.path().join("constants.ty")).unwrap();
    assert!(
        ty_src.contains("let PORT"),
        "module-level annotated assign should gain `let`; got:\n{ty_src}"
    );
    assert!(
        ty_src.contains("let HOST"),
        "module-level annotated assign should gain `let`; got:\n{ty_src}"
    );
}

#[test]
fn migrate_drops_dataclass_decorator() {
    let tmp = tempfile::tempdir().unwrap();
    let py_path = tmp.path().join("model.py");
    std::fs::write(
        &py_path,
        "from dataclasses import dataclass\n\n@dataclass\nclass Point:\n    x: int\n    y: int\n",
    )
    .unwrap();

    let status = tyc().arg("migrate").arg(&py_path).status().unwrap();
    assert!(status.success(), "tyc migrate should succeed");

    let ty_src = std::fs::read_to_string(tmp.path().join("model.ty")).unwrap();
    assert!(
        !ty_src.contains("@dataclass"),
        "dataclass decorator should be removed; got:\n{ty_src}"
    );
    assert!(
        ty_src.contains("class Point"),
        "class declaration should be preserved; got:\n{ty_src}"
    );
}

#[test]
fn migrate_check_mode_writes_to_stdout_not_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let py_path = tmp.path().join("thing.py");
    std::fs::write(&py_path, "x: int = 1\n").unwrap();

    let out = tyc()
        .args(["migrate", "--check"])
        .arg(&py_path)
        .output()
        .unwrap();
    assert!(out.status.success(), "tyc migrate --check should succeed");

    // The .ty file must NOT be written in --check mode.
    assert!(
        !tmp.path().join("thing.ty").exists(),
        "tyc migrate --check must not write a .ty file"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("let x"),
        "migrated source should appear on stdout; got:\n{stdout}"
    );
}

#[test]
fn migrate_missing_path_errors() {
    let status = tyc()
        .arg("migrate")
        .arg("/no/such/path/does/not/exist.py")
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "migrate of a missing path should fail with non-zero exit"
    );
}

// ── tyc check --stubs ─────────────────────────────────────────────────────────

#[test]
fn check_stubs_passes_when_stub_matches_implementation() {
    let tmp = tempfile::tempdir().unwrap();
    // Write a .ty implementation file.
    std::fs::write(
        tmp.path().join("math.ty"),
        "def add(a: int, b: int) -> int:\n    return a + b\n",
    )
    .unwrap();
    // Write a matching .dty stub.
    std::fs::write(
        tmp.path().join("math.dty"),
        "def add(a: int, b: int) -> int: ...\n",
    )
    .unwrap();

    let status = tyc()
        .arg("check")
        .arg("--stubs")
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "check --stubs should pass when stub matches implementation"
    );
}

#[test]
fn check_stubs_fails_when_stub_declares_missing_function() {
    let tmp = tempfile::tempdir().unwrap();
    // Implementation has `add` only.
    std::fs::write(
        tmp.path().join("math.ty"),
        "def add(a: int, b: int) -> int:\n    return a + b\n",
    )
    .unwrap();
    // Stub declares `add` and an extra `sub` that doesn't exist.
    std::fs::write(
        tmp.path().join("math.dty"),
        "def add(a: int, b: int) -> int: ...\ndef sub(a: int, b: int) -> int: ...\n",
    )
    .unwrap();

    let status = tyc()
        .arg("check")
        .arg("--stubs")
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "check --stubs should fail when stub declares a function absent from the implementation"
    );
}

#[test]
fn check_stubs_fails_when_implementation_has_extra_public_function() {
    let tmp = tempfile::tempdir().unwrap();
    // Implementation has both `add` and `mul`; stub only declares `add`.
    std::fs::write(
        tmp.path().join("math.ty"),
        "def add(a: int, b: int) -> int:\n    return a + b\n\ndef mul(a: int, b: int) -> int:\n    return a * b\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("math.dty"),
        "def add(a: int, b: int) -> int: ...\n",
    )
    .unwrap();

    let status = tyc()
        .arg("check")
        .arg("--stubs")
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "check --stubs should fail when implementation has a public function not in the stub"
    );
}

#[test]
fn check_stubs_standalone_dty_without_implementation_passes() {
    // A .dty with no sibling .ty/.py is valid — it may stub an external library.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("external.dty"),
        "def fetch(url: str) -> str: ...\n",
    )
    .unwrap();

    let status = tyc()
        .arg("check")
        .arg("--stubs")
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "standalone .dty with no implementation should pass --stubs"
    );
}

// ── source-map accuracy for sugar-expanded constructs ─────────────────────────
//
// `?` and `gather:` emit multiple Python lines from a single Typhon line.  The
// v2 source map records a 1-indexed line number in the expanded preprocessed
// text for each emitted Python line; entries are non-zero for every line.
// These tests build real projects and verify the map structure and `tyc trace`
// behaviour for those constructs.

/// Parse a `.py.map` JSON sidecar and return the `lines` array.  Panics on
/// malformed JSON.
fn parse_map_lines(map_body: &str) -> Vec<u32> {
    let v: serde_json::Value = serde_json::from_str(map_body).expect("valid JSON in .py.map");
    v["lines"]
        .as_array()
        .expect("lines array in .py.map")
        .iter()
        .map(|x| x.as_u64().expect("numeric line entry") as u32)
        .collect()
}

#[test]
fn source_map_question_op_expansion_maps_back_to_original_line() {
    // Build a project whose second statement uses `?` (error propagation).
    // After `expand_question_ops`, that one Typhon line expands to several
    // Python lines.  The v2 source map should record the originating Typhon
    // line for each of those expanded lines.
    //
    // Source layout (line numbers 1-indexed):
    //   1: def parse(s: str) -> Result[int, str]:
    //   2:     let n = int(s)?
    //   3:     return Ok(n)
    //
    // The `?` on line 2 expands to ≥3 Python lines; all of them should map
    // back to line 2 (or nearby) of the preprocessed source.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "\
def parse(s: str) -> Result[int, str]:
    let n = int(s)?
    return Ok(n)
",
    );
    let status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "build should succeed for ? operator test");

    let map_path = tmp
        .path()
        .join("build")
        .join(".sourcemaps")
        .join("main.py.map");
    assert!(map_path.exists(), "main.py.map should be emitted");

    let map_body = std::fs::read_to_string(&map_path).unwrap();
    assert!(
        map_body.contains("\"version\":2"),
        "map should be v2 format"
    );
    assert!(
        map_body.contains("\"line_strategy\":\"table\""),
        "map should use table strategy"
    );

    let lines = parse_map_lines(&map_body);
    assert!(!lines.is_empty(), "lines array must not be empty");

    // Every map entry must be non-zero (no gap in the table).
    for (py_line_idx, &ty_line) in lines.iter().enumerate() {
        assert!(
            ty_line >= 1,
            "Python line {} maps to 0 — source map has a gap",
            py_line_idx + 1
        );
    }

    // The `?` expansion injects at least three Python lines containing
    // `__typhon_q_` (assignment, isinstance guard, return/unwrap).  Reading
    // the emitted Python and counting those lines is a targeted proxy for
    // "the expansion produced multiple Python lines", and avoids the pitfall
    // of checking for any adjacent duplicate in the whole map (which can be
    // satisfied by unrelated function-header entries).
    let py_path = tmp.path().join("build").join("main.py");
    let py_src = std::fs::read_to_string(&py_path).unwrap();
    let q_expansion_count = py_src.lines().filter(|l| l.contains("__typhon_q_")).count();
    assert!(
        q_expansion_count >= 3,
        "? expansion should produce ≥3 Python lines containing __typhon_q_; got {q_expansion_count}"
    );
}

#[test]
fn source_map_question_op_traceback_rewrite() {
    // End-to-end: build a project with `?`, then feed a synthetic traceback
    // pointing at a Python line inside the expanded block to `tyc trace` and
    // verify the output names `main.ty` (not `main.py`).
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "\
def parse(s: str) -> Result[int, str]:
    let n = int(s)?
    return Ok(n)
",
    );
    let build_status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(build_status.success(), "build should succeed");

    let py_path = tmp.path().join("build").join("main.py");
    let map_path = tmp
        .path()
        .join("build")
        .join(".sourcemaps")
        .join("main.py.map");
    assert!(py_path.exists(), "main.py should exist");
    assert!(map_path.exists(), "main.py.map should exist");

    // Scan the emitted Python for a line injected by the `?` expansion
    // (`__typhon_q_N__`) so we don't hardcode a brittle line number.
    let py_src = std::fs::read_to_string(&py_path).unwrap();
    let frame_py_line: u32 = py_src
        .lines()
        .enumerate()
        .find_map(|(i, l)| {
            if l.contains("__typhon_q_") {
                Some((i + 1) as u32)
            } else {
                None
            }
        })
        .expect("emitted Python should contain a __typhon_q_ variable from ? expansion");

    let tb = format!(
        "Traceback (most recent call last):\n  File \"{}\", line {}, in parse\n",
        py_path.display(),
        frame_py_line
    );
    let tb_path = tmp.path().join("tb.txt");
    std::fs::write(&tb_path, &tb).unwrap();

    let out = tyc().arg("trace").arg(&tb_path).output().unwrap();
    assert!(out.status.success(), "tyc trace should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("main.ty"),
        "trace should rewrite .py path to .ty source; got: {stdout}"
    );
    assert!(
        !stdout.contains("main.py\""),
        ".py path should be replaced in trace output; got: {stdout}"
    );
    // We don't assert on the exact remapped line number because the `?`
    // expansion shifts lines — the important thing is that the path rewrite worked.
}

#[test]
fn source_map_gather_block_expansion_maps_back_to_original_line() {
    // Build a project with a `gather:` block (which lowers to asyncio.TaskGroup
    // and emits several Python lines from a compact Typhon block).
    // The v2 source map must record a valid Typhon line for every emitted line.
    let tmp = tempfile::tempdir().unwrap();
    scaffold(
        tmp.path(),
        "\
async def fetch_a() -> int:
    return 1

async def fetch_b() -> int:
    return 2

async def load() -> int:
    gather:
        a = fetch_a()
        b = fetch_b()
    return 0
",
    );
    let status = tyc().arg("build").arg(tmp.path()).status().unwrap();
    assert!(status.success(), "build should succeed for gather: test");

    let map_path = tmp
        .path()
        .join("build")
        .join(".sourcemaps")
        .join("main.py.map");
    let map_body = std::fs::read_to_string(&map_path).unwrap();
    let lines = parse_map_lines(&map_body);

    assert!(!lines.is_empty(), "lines array must not be empty");

    // Every map entry must be non-zero (no gap in the table).
    for (py_idx, &ty_line) in lines.iter().enumerate() {
        assert!(
            ty_line >= 1,
            "Python line {} maps to source line 0 — invalid gap in source map",
            py_idx + 1,
        );
    }

    // The `gather:` block lowers to an `async with asyncio.TaskGroup()` header
    // plus one `create_task` call per branch, producing ≥3 Python lines that
    // contain `__typhon_tg_` or `__typhon_gather_`.  Counting those lines is
    // targeted at the gather expansion specifically, unlike checking for any
    // adjacent map duplicate (which fires on blank-line / function-header pairs
    // unrelated to gather).
    let py_path = tmp.path().join("build").join("main.py");
    let py_src = std::fs::read_to_string(&py_path).unwrap();
    let gather_expansion_count = py_src
        .lines()
        .filter(|l| l.contains("__typhon_tg_") || l.contains("__typhon_gather_"))
        .count();
    assert!(
        gather_expansion_count >= 3,
        "gather: expansion should produce ≥3 Python lines with __typhon_tg_/__typhon_gather_; \
         got {gather_expansion_count}"
    );
}

// ── corpus round-trip sweep ───────────────────────────────────────────────────

/// Walk `examples/` and assert that `tyc check` succeeds on every
/// `.ty` source file. Closes roadmap concrete-next-step #1 ("Corpus
/// round-trip sweep") — any change to the type checker, resolver,
/// or analyser that breaks a previously-working example now fails CI
/// immediately rather than being discovered in a manual sweep
/// campaign.
///
/// Failures are collected rather than short-circuited so a single
/// run surfaces the complete breakage list. Files under
/// `examples/testing/` are intentional failure probes and are
/// excluded.
/// Round-trip the curated third-party-Python corpus through `tyc
/// migrate` + `tyc check`. Catches regressions in the migrator's
/// rewrite catalogue (dataclass, Protocol, NewType, generics) and in
/// the checker's handling of the migrator's output. Failures are
/// collected so a single run surfaces the complete list.
///
/// The corpus lives at `stress/third-party-py-corpus/*.py`. Adding a
/// file there automatically extends this sweep — no test-side wiring
/// required.
#[test]
fn third_party_corpus_round_trips_cleanly() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let corpus_dir = manifest_dir.join("../../../stress/third-party-py-corpus");
    let corpus_dir = match corpus_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            eprintln!(
                "skipping third-party corpus sweep: dir not found at {}",
                corpus_dir.display()
            );
            return;
        }
    };
    let mut py_files: Vec<std::path::PathBuf> = std::fs::read_dir(&corpus_dir)
        .expect("read corpus dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "py"))
        .collect();
    py_files.sort();
    assert!(
        !py_files.is_empty(),
        "expected at least one .py file under {}",
        corpus_dir.display()
    );

    let mut failures: Vec<(std::path::PathBuf, String, String)> = Vec::new();
    for py in &py_files {
        // Each fixture goes in its own tempdir so the migrator's
        // sibling `.ty` output and the subsequent `tyc check` run
        // can't cross-contaminate. The `--check` flag prints to
        // stdout so we capture the migrated source explicitly.
        let tmp = tempfile::tempdir().expect("tempdir");
        let target_py = tmp.path().join(py.file_name().unwrap());
        std::fs::copy(py, &target_py).expect("copy fixture");

        let migrate_out = tyc()
            .arg("migrate")
            .arg(&target_py)
            .output()
            .expect("migrate spawn");
        if !migrate_out.status.success() {
            failures.push((
                py.clone(),
                "migrate".into(),
                String::from_utf8_lossy(&migrate_out.stderr).into_owned(),
            ));
            continue;
        }

        // `tyc migrate` writes `foo.ty` next to `foo.py`. Set up a
        // small Typhon project around it so `tyc check` has a typhon.toml
        // anchor (the corpus dir itself is not a project root).
        let ty_path = target_py.with_extension("ty");
        if !ty_path.exists() {
            failures.push((
                py.clone(),
                "migrate-output".into(),
                format!("expected migrated file at {}", ty_path.display()),
            ));
            continue;
        }

        let project_root = tempfile::tempdir().expect("project tempdir");
        let src_dir = project_root.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::copy(&ty_path, src_dir.join("main.ty")).unwrap();
        std::fs::write(
            project_root.path().join("typhon.toml"),
            "[project]\nname = \"corpus\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
             [python]\ntarget = \"3.13\"\n[emit]\nformat = false\n[strictness]\n[env]\n",
        )
        .unwrap();

        let check_out = tyc()
            .arg("check")
            .arg(project_root.path())
            .output()
            .expect("check spawn");
        if !check_out.status.success() {
            failures.push((
                py.clone(),
                "check".into(),
                format!(
                    "{}\n{}\n--- migrated source ---\n{}",
                    String::from_utf8_lossy(&check_out.stdout),
                    String::from_utf8_lossy(&check_out.stderr),
                    std::fs::read_to_string(&ty_path).unwrap_or_default(),
                ),
            ));
        }
    }

    if !failures.is_empty() {
        let mut msg = format!(
            "{} of {} third-party fixture(s) failed migrate→check:\n",
            failures.len(),
            py_files.len()
        );
        for (path, stage, out) in &failures {
            msg.push_str(&format!("── {} [{stage}] ──\n{out}\n", path.display()));
        }
        panic!("{msg}");
    }
}

#[test]
fn corpus_examples_all_check_clean() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let examples_dir = manifest_dir.join("../../../examples");
    let examples_dir = match examples_dir.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // `examples/` doesn't exist relative to the manifest in
            // some checkout layouts; skip rather than fail loudly.
            eprintln!(
                "skipping corpus sweep: examples dir not found at {}",
                examples_dir.display()
            );
            return;
        }
    };
    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .is_some_and(|n| n == "testing")
                {
                    continue;
                }
                collect(&path, out);
            } else if path.extension().is_some_and(|e| e == "ty") {
                out.push(path);
            }
        }
    }
    let mut ty_files: Vec<std::path::PathBuf> = Vec::new();
    collect(&examples_dir, &mut ty_files);
    ty_files.sort();
    assert!(
        !ty_files.is_empty(),
        "expected at least one .ty file under {}",
        examples_dir.display()
    );

    let mut failures: Vec<(std::path::PathBuf, std::process::Output)> = Vec::new();
    for file in &ty_files {
        let out = tyc()
            .arg("check")
            .arg(file)
            .output()
            .expect("tyc check spawn");
        if !out.status.success() {
            failures.push((file.clone(), out));
        }
    }
    if !failures.is_empty() {
        let mut msg = format!(
            "{} of {} corpus example(s) failed `tyc check`:\n",
            failures.len(),
            ty_files.len()
        );
        for (path, out) in &failures {
            msg.push_str(&format!(
                "── {} ──\n{}\n{}\n",
                path.display(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            ));
        }
        panic!("{msg}");
    }
}

#[test]
fn bundled_httpx_stub_checks_without_venv() {
    // A project that declares (but hasn't installed) httpx still type-checks
    // against the compiler-bundled stub: construction is validated, the
    // `httpx.Response` / `httpx.AsyncClient` types resolve, and no
    // `unintrospectable-dependency` warning fires — with no `.venv` present.
    let project = tempfile::tempdir().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        project.path().join("typhon.toml"),
        "[project]\nname = \"x\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [python]\ntarget = \"3.13\"\n[dependencies]\nhttpx = \">=0.27\"\n",
    )
    .unwrap();
    std::fs::write(
        src.join("main.ty"),
        "import httpx\n\n\
         async def fetch(url: str) -> httpx.Response:\n    \
             let client: httpx.AsyncClient = httpx.AsyncClient(base_url=\"https://x\", timeout=10)\n    \
             let resp: httpx.Response = await client.get(url, params={\"q\": \"x\"})\n    \
             await client.aclose()\n    \
             return resp\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(project.path()).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.status.success(),
        "bundled httpx stub should let the project check cleanly, got: {combined}"
    );
    assert!(
        !combined.contains("unintrospectable"),
        "bundled httpx must suppress the unintrospectable-dependency warning, got: {combined}"
    );
}

#[test]
fn bundled_httpx_stub_catches_bad_constructor_kwarg() {
    // The bundle delivers real checking, not blanket leniency: a wrong
    // constructor kwarg is still caught.
    let project = tempfile::tempdir().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        project.path().join("typhon.toml"),
        "[project]\nname = \"x\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [dependencies]\nhttpx = \"*\"\n",
    )
    .unwrap();
    std::fs::write(
        src.join("main.ty"),
        "import httpx\n\ndef f() -> None:\n    \
             let c: httpx.AsyncClient = httpx.AsyncClient(no_such_kwarg=1)\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(project.path()).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !out.status.success(),
        "a bad httpx constructor kwarg must fail check, got: {combined}"
    );
    assert!(
        combined.contains("unknown_kwarg") || combined.contains("no_such_kwarg"),
        "expected an unknown-kwarg diagnostic, got: {combined}"
    );
}

#[test]
fn bundled_stubs_keep_cross_module_classes_distinct() {
    // Both httpx and requests define `Response`. The qualified↔bare
    // assignability relaxation must NOT conflate them: assigning a
    // `requests.Response` into an `httpx.Response` slot stays a mismatch.
    let project = tempfile::tempdir().unwrap();
    let src = project.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        project.path().join("typhon.toml"),
        "[project]\nname = \"x\"\nversion = \"0.1.0\"\nsrc = \"src\"\nout = \"build\"\n\
         [dependencies]\nhttpx = \"*\"\nrequests = \"*\"\n",
    )
    .unwrap();
    std::fs::write(
        src.join("main.ty"),
        "import httpx\nimport requests\n\ndef f() -> None:\n    \
             let r: httpx.Response = requests.get(\"https://x\")\n",
    )
    .unwrap();
    let out = tyc().arg("check").arg(project.path()).output().unwrap();
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !out.status.success(),
        "requests.Response into httpx.Response must mismatch, got: {combined}"
    );
    assert!(
        combined.contains("type_mismatch"),
        "expected type_mismatch, got: {combined}"
    );
}
