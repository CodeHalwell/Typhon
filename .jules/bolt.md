
## 2024-06-04 - Optimize Source Map Generation string formatting
**Learning:** `format!` macros inside `build_source_map_v2` to join dynamically allocated strings (`n.to_string()`) causes excessive heap allocations and overhead. Given `lines.len()` could be huge for long files, it negatively impacts compilation speeds in hot paths. Using `itoa::Buffer` was an option but required adding a new dependency which violated the constraints.
**Action:** Use manual `String::with_capacity` and manual string building using `push_str`/`push` alongside `std::fmt::Write::write_fmt` via the `write!` macro to avoid multiple runtime allocations while retaining correct output format without adding new dependencies.

## 2024-06-11 - Source Map Bottleneck Resolved
**Learning:** `build_source_map_v2` had an O(N*K) bottleneck (N = number of offsets, K = length of source file) due to rescanning the entire string for newlines with every offset lookup via `offset_to_line()`.
**Action:** When finding line numbers from byte offsets in hot loops, always precompute newline offsets into a `Vec<usize>` and use binary search (`partition_point`) to map offsets to lines in O(N log K) time.
