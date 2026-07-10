## 2024-05-24 - Synchronized Secret Scanning Heuristics
**Vulnerability:** A minor discrepancy existed between the `is_secret_name` heuristic dictionary in `tyc-analyse` and `secret_suffix` in the `tyc` build tools, specifically missing the keyword `"PASS"`.
**Learning:** This discrepancy could have allowed a variable like `DB_PASS` to be flagged in one context but silently ignored in another, creating a confusing and potentially leaky security gap.
**Prevention:** Always ensure that heuristic logic, such as dictionaries or regexes used across multiple crates or commands in a workspace, are either shared as a common library constant or strictly synchronized to maintain consistent security enforcement.

## 2026-07-04 - Longest-first Matching for Secret Detection
**Vulnerability:** Hardcoded secret detection was missing `APIKEY` due to partial overlap with `KEY` because `KEY` was evaluated before `APIKEY`.
**Learning:** When scanning for secrets using keyword arrays, substrings (like `KEY`) must be placed after longer matches (like `APIKEY` or `API_KEY`) to prevent premature partial matches.
**Prevention:** Always maintain hardcoded secret matching keywords in longest-first order.

## 2024-05-27 - Expanding Hardcoded Secret Detection Keywords
**Vulnerability:** The hardcoded secret detection (`tyc::contains_secret_literal`) was missing checks for modern API tokens like `JWT`, `BEARER`, `CREDENTIAL`, and `PAT`.
**Learning:** Hardcoded credentials are a critical security risk and identifying them requires an up-to-date list of high-risk keywords used by developers. Also, matching logic based on substring bounding means we don't need to add permutations if the base word is caught, but we must ensure the base keywords are present and ordered longest-first.
**Prevention:** Periodically update the keyword lists (`is_secret_name` and `secret_suffix`) to include emerging credential patterns, ensuring they are ordered longest-first and synchronized across all parts of the scanning pipeline.
