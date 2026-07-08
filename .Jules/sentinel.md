## 2024-05-24 - Synchronized Secret Scanning Heuristics
**Vulnerability:** A minor discrepancy existed between the `is_secret_name` heuristic dictionary in `tyc-analyse` and `secret_suffix` in the `tyc` build tools, specifically missing the keyword `"PASS"`.
**Learning:** This discrepancy could have allowed a variable like `DB_PASS` to be flagged in one context but silently ignored in another, creating a confusing and potentially leaky security gap.
**Prevention:** Always ensure that heuristic logic, such as dictionaries or regexes used across multiple crates or commands in a workspace, are either shared as a common library constant or strictly synchronized to maintain consistent security enforcement.
## 2024-05-24 - Partial Match Heuristic Ordering
**Vulnerability:** The secret scanning logic (`contains_secret_literal`) checked for `"KEY"` before `"APIKEY"`. Because the logic matches the first available substring, `"APIKEY"` would incorrectly match as `"KEY"`.
**Learning:** Hardcoded dictionaries used for substring matching must be ordered longest-first when there are overlapping values to prevent premature partial matches.
**Prevention:** When modifying the `WORDS` dictionary for secret scanning, always verify that the array is explicitly ordered by string length (longest first).
