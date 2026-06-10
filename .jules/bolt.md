
## 2024-06-04 - Optimize Source Map Generation string formatting
**Learning:** `format!` macros inside `build_source_map_v2` to join dynamically allocated strings (`n.to_string()`) causes excessive heap allocations and overhead. Given `lines.len()` could be huge for long files, it negatively impacts compilation speeds in hot paths. Using `itoa::Buffer` was an option but required adding a new dependency which violated the constraints.
**Action:** Use manual `String::with_capacity` and manual string building using `push_str`/`push` alongside `std::fmt::Write::write_fmt` via the `write!` macro to avoid multiple runtime allocations while retaining correct output format without adding new dependencies.

## 2024-06-10 - Optimize Source Map Line Offset Lookup
**Learning:** `offset_to_line` calculated line numbers iteratively from string start for each offset during source map generation. This produced $O(N \times M)$ complexity where N is the number of offsets and M is average length to offset.
**Action:** Always prefer caching iteration points in a collection and searching via `partition_point` (binary search). This approach dramatically reduced lookup overhead to $O(M + N \log K)$, where K is number of lines, dropping iteration time significantly.
