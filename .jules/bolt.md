
## 2024-06-04 - Optimize Source Map Generation string formatting
**Learning:** `format!` macros inside `build_source_map_v2` to join dynamically allocated strings (`n.to_string()`) causes excessive heap allocations and overhead. Given `lines.len()` could be huge for long files, it negatively impacts compilation speeds in hot paths. Using `itoa::Buffer` was an option but required adding a new dependency which violated the constraints.
**Action:** Use manual `String::with_capacity` and manual string building using `push_str`/`push` alongside `std::fmt::Write::write_fmt` via the `write!` macro to avoid multiple runtime allocations while retaining correct output format without adding new dependencies.

## 2025-02-12 - Resolve O(N^2) behavior in source map generation using binary search
**Learning:** The previous naive line resolution for source maps repeatedly iterated over a progressively growing slice of `preprocessed.as_bytes()` to find `b'\n'` values, counting them every single time. This resulted in an implicit O(N*M) runtime where N is the length of the string and M is the number of offsets, which is severely slow for very large source codes mapping.
**Action:** When working with offsets into lines translation, especially in batch structures like AST traversal and source map generation, always consider precomputing newline indices `Vec<usize>` in an O(N) pass, and use a standard binary search like `slice::partition_point` to map an arbitrary byte offset to a line number in O(log L) time (L being line count), bringing the total mapping to O(N + M*log L).
