
## 2024-06-04 - Optimize Source Map Generation string formatting
**Learning:** `format!` macros inside `build_source_map_v2` to join dynamically allocated strings (`n.to_string()`) causes excessive heap allocations and overhead. Given `lines.len()` could be huge for long files, it negatively impacts compilation speeds in hot paths. Using `itoa::Buffer` was an option but required adding a new dependency which violated the constraints.
**Action:** Use manual `String::with_capacity` and manual string building using `push_str`/`push` alongside `std::fmt::Write::write_fmt` via the `write!` macro to avoid multiple runtime allocations while retaining correct output format without adding new dependencies.

## $(date +%Y-%m-%d) - Optimize Source Map Generation line calculation
**Learning:** `build_source_map_v2` iterating over the preprocessed string to count newlines for every single offset caused O(N * M) performance degradation. A simple loop checking all bytes is slow when called repeatedly for thousands of offsets.
**Action:** Always precompute newline indices in a single O(M) pass and use a binary search (`partition_point`) to map offsets to line numbers. This brings the time complexity down to O(M + N log L), offering immense speedups for hot loops generating line mapping data.
