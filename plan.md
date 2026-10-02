1. **Optimize `local_classes` Set Creation in `tyc-desugar`**: Change `let local_classes: HashSet<String> = ...` to `let local_classes: HashSet<&str> = ...` in `tyc/crates/tyc-desugar/src/lib.rs` (around line 3749).
2. **Use String slice matching**: Modify the filter_map closure to borrow the string slice from the `Stmt::ClassDef` node (`return Some(n);`) rather than allocating an owned `String` (`return Some(n.to_owned());`).
3. **Update Call Sites**: Change `local_classes.contains(t)` to `local_classes.contains(t.as_str())` around line 3788.
4. **Run Checks**: Run tests and lint checks in the `tyc` directory to ensure correctness.
5. **Pre-commit**: Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.
6. **Submit**: Use the `submit` tool to create a PR.
