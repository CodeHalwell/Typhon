## 2024-05-24 - Synchronized Secret Scanning Heuristics
**Vulnerability:** A minor discrepancy existed between the `is_secret_name` heuristic dictionary in `tyc-analyse` and `secret_suffix` in the `tyc` build tools, specifically missing the keyword `"PASS"`.
**Learning:** This discrepancy could have allowed a variable like `DB_PASS` to be flagged in one context but silently ignored in another, creating a confusing and potentially leaky security gap.
**Prevention:** Always ensure that heuristic logic, such as dictionaries or regexes used across multiple crates or commands in a workspace, are either shared as a common library constant or strictly synchronized to maintain consistent security enforcement.

## 2024-07-06 - Secret Pattern Matching Ordering Vulnerability Fix
**Vulnerability:** The secret detection algorithms (`is_secret_name` and `secret_suffix`) ordered matching words without strict length sorting (e.g. `"KEY"` came before `"APIKEY"`). A variable named `MY_APIKEY` would falsely match only the `"KEY"` substring instead of `"APIKEY"`, leading to incorrect mapping and potential bypasses depending on specific boundary-check constraints in other areas of the code.
**Learning:** Hardcoded substring matching heuristics that contain subsets of one another must always be explicitly sorted by length (longest-first) to ensure accurate parsing and avoid subset overlap shadow bugs.
**Prevention:** Always write robust tests for keyword heuristics containing subsets (like `"APIKEY"` and `"KEY"` in the same payload string) to enforce correct matching order.
