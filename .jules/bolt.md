## 2025-01-01 - Avoid Double-Hashing/Allocation when resolving Python module scope classes

**Learning:** When collecting class names generated in Python macros or internal analysis (such as `tyc-resolve` gathering `plain_classes` and `raw_classes`, or `tyc-desugar` maintaining isolated identifiers), the initial Rust codebase often defaults to generating completely isolated `HashSet<String>`s. This forces string allocation within tight AST loops, causing allocations in hot paths.

**Action:** Refactored AST analyses in `tyc-resolve` and `tyc-desugar` to gather sets of `&str` reference string slices (`HashSet<&str>`). If the caller context mandates an owned `HashSet<String>`, the mapping/conversion is explicitly deferred and collected upstream using `into_iter().map(|s| s.to_string()).collect()`—rather than creating widespread string clones downstream.

## 2025-01-01 - Avoid Double-Hashing/Allocation when resolving Python module scope classes
**Learning:** When collecting class names generated in Python macros or internal analysis (such as `tyc-resolve` gathering `plain_classes` and `raw_classes`), the initial Rust codebase often defaults to generating completely isolated `HashSet<String>`s. This forces string allocation within tight AST loops, causing allocations in hot paths.
**Action:** Refactored AST analyses in `tyc-resolve` to gather sets of `&str` reference string slices (`HashSet<&str>`). If the caller context mandates an owned `HashSet<String>`, the mapping/conversion is explicitly deferred and collected upstream using `into_iter().map(|s| s.to_string()).collect()`—rather than creating widespread string clones downstream.
