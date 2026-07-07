## 2024-05-24 - Synchronized Secret Scanning Heuristics
**Vulnerability:** A minor discrepancy existed between the `is_secret_name` heuristic dictionary in `tyc-analyse` and `secret_suffix` in the `tyc` build tools, specifically missing the keyword `"PASS"`.
**Learning:** This discrepancy could have allowed a variable like `DB_PASS` to be flagged in one context but silently ignored in another, creating a confusing and potentially leaky security gap.
**Prevention:** Always ensure that heuristic logic, such as dictionaries or regexes used across multiple crates or commands in a workspace, are either shared as a common library constant or strictly synchronized to maintain consistent security enforcement.

## 2024-05-24 - Secret Scanner Longest-First Matching Requirement
**Vulnerability:** The secret scanner's keyword list (`is_secret_name` and `secret_suffix`) ordered `KEY` before `APIKEY`. This caused partial matches on strings like `APIKEY`, reporting a match on `KEY` and skipping a full exact match on the longer compound token.
**Learning:** Hardcoded dictionaries used for heuristic secret detection must be ordered longest-first so that specific compound permutations (e.g. `API_KEY`, `APIKEY`) are matched fully before generalized keywords (e.g. `KEY`) intercept them, preventing misleading outputs or incorrect categorisation of detected secrets.
**Prevention:** When updating or adding to the `tyc::contains_secret_literal` diagnostic (e.g., in `is_secret_name` or `secret_suffix`), explicitly register all common permutations of high-risk security keywords (like `APIKEY` and `API_KEY`) and ensure the entire array is sorted longest-first to prevent premature partial matches.
