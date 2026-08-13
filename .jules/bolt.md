
## 2024-06-04 - Optimize Source Map Generation string formatting
**Learning:** `format!` macros inside `build_source_map_v2` to join dynamically allocated strings (`n.to_string()`) causes excessive heap allocations and overhead. Given `lines.len()` could be huge for long files, it negatively impacts compilation speeds in hot paths. Using `itoa::Buffer` was an option but required adding a new dependency which violated the constraints.
**Action:** Use manual `String::with_capacity` and manual string building using `push_str`/`push` alongside `std::fmt::Write::write_fmt` via the `write!` macro to avoid multiple runtime allocations while retaining correct output format without adding new dependencies.

## 2024-06-12 - Resolve O(N^2) Source Map Generation string traversal bottleneck
**Learning:** In `build_source_map_v2`, looking up the line number for a source file byte offset using `offset.min(source.len()); source.as_bytes()[..clamped].iter().filter(|&&b| b == b'\n').count()` requires scanning the string from the start every single time. Because this was called in an `O(N)` mapping operation over `line_offsets`, the algorithm degraded into `O(N * L)` complexity, creating severe bottlenecks on larger python source files.
**Action:** When converting multiple byte offsets to line numbers, precompute an index of newline offsets (which is `O(L)`) and then use binary search via `partition_point` (which is `O(log K)` where `K` is the number of newlines). This reduces the complexity to `O(L + N log K)`.

## 2024-06-05 - Avoid `.to_string()` inside match hot loops
**Learning:** Checking for string equality against static `.to_string()` allocations (e.g. `["Ok".to_string(), "Err".to_string()]`) inside frequently called functions (like `cases_cover_type`) leads to significant and avoidable heap allocations.
**Action:** Replace `["Ok".to_string(), "Err".to_string()]` with `["Ok", "Err"]` array of static references (`&'static str`) and dereference in closures properly (e.g. `|&v| covered.contains(v)`).
## 2024-08-01 - Avoid display().to_string() for Paths
**Learning:** Converting `std::path::Path` objects to strings using `path.display().to_string()` incurs heavy `std::fmt` allocation overhead.
**Action:** Prefer `path.to_string_lossy().into_owned()` when an owned `String` is strictly required to reduce formatting overhead.

## 2024-08-10 - Avoid HashSet<String> for AST Identifiers
**Learning:** Checking for subset matching or exhaustiveness against multiple AST node identifiers utilizing `HashSet<String>` creates constant temporary heap allocations within hot match evaluation logic.
**Action:** Always borrow AST node string slices into `HashSet<&str>` directly where practical (e.g. for short-lived tracking sets during type-checking pattern loops) to mitigate massive redundant allocations across deep compilation traces.
## 2024-05-19 - Use `&str` instead of `String` for HashSet duplicate tracking
**Learning:** Using `HashSet<String>` combined with `.to_owned()` during hot loops (like scanning class definitions in an AST) introduces unnecessary heap allocations that can be avoided by borrowing slices `HashSet<&str>` directly from the nodes.
**Action:** When tracking unique occurrences of names in AST nodes or strings in short-lived localized scopes, use `HashSet<&str>` to borrow without allocating, converting to owned strings only when they must be passed to long-lived containers.
## 2024-03-24 - Zero-Allocation AST Traversal
**Learning:** Using `HashSet<&str>` instead of `HashSet<String>` by capturing the lifetime of `Expr` nodes avoids significant allocation overhead in hot paths like `collect_names_in_expr`.
**Action:** When writing Rust traversal logic over the AST, borrow identifiers as `&str` instead of collecting owned `String` instances wherever possible to reduce heap allocations.
## 2024-11-25 - Zero-Allocation `module_level_bound_names`
**Learning:** `module_level_bound_names` in `tyc-desugar` originally created a new `HashSet<String>` by cloning string identifiers inside AST nodes. As this is used to verify `collections.abc` types inside hot paths, the repeated allocation of strings negatively impacted compilation times.
**Action:** By bounding the lifetime of the `HashSet<&str>` to the incoming `&[Stmt]` reference, we can avoid String allocations entirely when building the `bound_names` set and rely on dereferencing pointers for fast lookups.
## 2024-11-26 - Zero-Allocation shared mut AST traversal
**Learning:** Checking for shared mut races in `parallel_lints` required constructing temporary `HashSet<String>` objects from AST node slices and using `clone` on variable names, creating redundant heap allocations across parallel AST traversals.
**Action:** Always borrow string slices with `&'a str` into short-lived maps and vectors like `GoSpawn<'a>`, `HashSet<&'a str>`, and `HashMap<&'a str, bool>` where the lifetime `'a` is tied directly to the `ModModule` or `[Stmt]` to eliminate these temporary strings.
## 2024-11-26 - Zero-Allocation `module_all_names`
**Learning:** Checking for subset matching or exhaustiveness against multiple AST node identifiers utilizing `HashSet<String>` creates temporary heap allocations. `module_all_names` in `tyc-analyse` originally constructed a new `HashSet<String>` by cloning string identifiers inside AST nodes. As this is used to verify `__all__` exports inside hot paths, the repeated allocation of strings negatively impacted compilation times.
**Action:** By bounding the lifetime of the `HashSet<&str>` to the incoming `&[Stmt]` reference, we can avoid String allocations entirely when building the `all_names` set and rely on dereferencing pointers for fast lookups. Callers to `.contains()` just need to pass `&name`.
