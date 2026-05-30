## 2024-05-14 - String escaping optimization
**Learning:** `format!("\\x{:02x}", byte)` and `.to_string()` for characters are surprisingly slow in hot loops due to formatting overhead and heap allocations. Using manual byte indexing with a hex table and `char::encode_utf8` on a small stack buffer avoids allocations and is over 35x faster.
**Action:** When emitting code or serializing strings, always prefer pushing `char` directly, `encode_utf8`, or manual appending using `push_str` rather than using the `format!` macro or `.to_string()`.
## 2024-05-15 - Number literal formatting optimization
**Learning:** `i.to_string()` and `format!("{:?}", f)` cause unnecessary heap allocations when emitting number literals in the AST emitter, which is a hot path.
**Action:** Use stack-allocated buffers like `itoa::Buffer` and `ryu::Buffer` for formatting numbers to eliminate heap allocations, taking care to handle `ryu`'s lack of support for `NaN` and `inf` explicitly.
