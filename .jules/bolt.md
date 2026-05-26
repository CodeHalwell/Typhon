## 2024-05-14 - String escaping optimization
**Learning:** `format!("\\x{:02x}", byte)` and `.to_string()` for characters are surprisingly slow in hot loops due to formatting overhead and heap allocations. Using manual byte indexing with a hex table and `char::encode_utf8` on a small stack buffer avoids allocations and is over 35x faster.
**Action:** When emitting code or serializing strings, always prefer pushing `char` directly, `encode_utf8`, or manual appending using `push_str` rather than using the `format!` macro or `.to_string()`.
## 2024-05-15 - Redundant String allocations in hot loops
**Learning:** `c.to_string()` for characters and `format!("{quote}{quote}{quote}")` in hot AST emission loops (like `tyc-emit`) cause significant overhead due to unnecessary heap allocations. Python string generation needs to be heavily optimized here since files can be large.
**Action:** Use `.push(c)` (or a helper like `write_char`) for individual characters and static slice conditionals (`if quote == '"' { "\"\"\"" } else { "'''" }`) instead of runtime formatting.
