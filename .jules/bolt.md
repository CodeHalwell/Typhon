
## 2024-06-04 - Optimize Source Map Generation string formatting
**Learning:** `format!` macros inside `build_source_map_v2` to join dynamically allocated strings (`n.to_string()`) causes excessive heap allocations and overhead. Given `lines.len()` could be huge for long files, it negatively impacts compilation speeds in hot paths. Using `itoa::Buffer` was an option but required adding a new dependency which violated the constraints.
**Action:** Use manual `String::with_capacity` and manual string building using `push_str`/`push` alongside `std::fmt::Write::write_fmt` via the `write!` macro to avoid multiple runtime allocations while retaining correct output format without adding new dependencies.

## 2024-06-12 - Resolve O(N^2) Source Map Generation string traversal bottleneck
**Learning:** In `build_source_map_v2`, looking up the line number for a source file byte offset using `offset.min(source.len()); source.as_bytes()[..clamped].iter().filter(|&&b| b == b'\n').count()` requires scanning the string from the start every single time. Because this was called in an `O(N)` mapping operation over `line_offsets`, the algorithm degraded into `O(N * L)` complexity, creating severe bottlenecks on larger python source files.
**Action:** When converting multiple byte offsets to line numbers, precompute an index of newline offsets (which is `O(L)`) and then use binary search via `partition_point` (which is `O(log K)` where `K` is the number of newlines). This reduces the complexity to `O(L + N log K)`.

## 2024-06-05 - Avoid `.to_string()` inside match hot loops
**Learning:** Checking for string equality against static `.to_string()` allocations (e.g. `["Ok".to_string(), "Err".to_string()]`) inside frequently called functions (like `cases_cover_type`) leads to significant and avoidable heap allocations.
**Action:** Replace `["Ok".to_string(), "Err".to_string()]` with `["Ok", "Err"]` array of static references (`&'static str`) and dereference in closures properly (e.g. `|&v| covered.contains(v)`).
## 2024-06-20 - Path Display Allocation in Hot Paths
**Learning:** In the Typhon compiler, using `path.display().to_string()` incurs significant overhead from heap allocation and `std::fmt` formatting machinery. This is especially problematic in hot loops handling module resolution (`extract_shapes_for_path`). Replacing this with `path.to_string_lossy()` leverages `std::borrow::Cow`, avoiding allocations entirely when passing references (`as_ref()`) to functions requiring borrowed strings, while securely allowing owned strings when needed (`into_owned()`).
**Action:** Always prefer `path.to_string_lossy()` over `path.display().to_string()` for path-to-string conversions, ensuring allocations are minimized.
