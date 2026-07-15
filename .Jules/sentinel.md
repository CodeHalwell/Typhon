## 2024-05-24 - Synchronized Secret Scanning Heuristics
**Vulnerability:** A minor discrepancy existed between the `is_secret_name` heuristic dictionary in `tyc-analyse` and `secret_suffix` in the `tyc` build tools, specifically missing the keyword `"PASS"`.
**Learning:** This discrepancy could have allowed a variable like `DB_PASS` to be flagged in one context but silently ignored in another, creating a confusing and potentially leaky security gap.
**Prevention:** Always ensure that heuristic logic, such as dictionaries or regexes used across multiple crates or commands in a workspace, are either shared as a common library constant or strictly synchronized to maintain consistent security enforcement.

## 2026-07-04 - Longest-first Matching for Secret Detection
**Vulnerability:** Hardcoded secret detection was missing `APIKEY` due to partial overlap with `KEY` because `KEY` was evaluated before `APIKEY`.
**Learning:** When scanning for secrets using keyword arrays, substrings (like `KEY`) must be placed after longer matches (like `APIKEY` or `API_KEY`) to prevent premature partial matches.
**Prevention:** Always maintain hardcoded secret matching keywords in longest-first order.

## $(date +%Y-%m-%d) - Ensure all permutations of high-risk security keywords are explicitly registered
**Vulnerability:** A hardcoded secrets check (`contains_secret_literal`) failed to detect some authentication-related words such as `AUTHORIZATION`, `CREDENTIALS`, `PASSPHRASE`, `BEARER`, and `AUTH` as a secret, despite catching `API_KEY` and similar variations.
**Learning:** Hardcoded secret detection must explicitly include authentication-related words to properly capture a wide range of sensitive values.
**Prevention:** Regularly review and update the keyword list in `tyc::contains_secret_literal` diagnostic (`is_secret_name` and `secret_suffix`) to include all common permutations and variations of high-risk security keywords.
