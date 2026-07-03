## 2024-05-24 - Synchronized Secret Scanning Heuristics
**Vulnerability:** A minor discrepancy existed between the `is_secret_name` heuristic dictionary in `tyc-analyse` and `secret_suffix` in the `tyc` build tools, specifically missing the keyword `"PASS"`.
**Learning:** This discrepancy could have allowed a variable like `DB_PASS` to be flagged in one context but silently ignored in another, creating a confusing and potentially leaky security gap.
**Prevention:** Always ensure that heuristic logic, such as dictionaries or regexes used across multiple crates or commands in a workspace, are either shared as a common library constant or strictly synchronized to maintain consistent security enforcement.

## 2024-07-03 - [Security Keyword Ordering]
**Vulnerability:** Weak hardcoded secret keyword detection.
**Learning:** Security keyword matchers must evaluate keywords ordered by length (longest-first). Without this, "KEY_APIKEY" matches "KEY" instead of the more specific "APIKEY", which could mask higher-risk secrets or trigger the wrong diagnostic categorization.
**Prevention:** Always maintain a strictly longest-first ordering when updating security keyword lists, and ensure this list is synchronized across all components (e.g., static analysis and build tools).
