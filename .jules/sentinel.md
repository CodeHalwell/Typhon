## 2025-02-14 - Add allow-secret-comptime config to strictness
**Vulnerability:** A hardcoded secrets check (`contains_secret_literal`) printed a warning but could not be silenced because the configuration property `allow_secret_comptime` was missing.
**Learning:** Hardcoded secret checks that produce false positives can train users to ignore security warnings if there is no way to suppress them.
**Prevention:** Implement the suppression configuration knob to allow users to document and silence expected findings.

## $(date +%Y-%m-%d) - Improve secret detection bounding logic
**Vulnerability:** The secret detection logic in `tyc-analyse` and `tyc build` only matched variable names that *ended* with secret suffixes (e.g., `_KEY`), missing embedded secrets (`API_KEY_FOO`) and camelCase secrets (`myTokenValue`).
**Learning:** Simple `ends_with` string matching is insufficient for static analysis security checks; boundary conditions (string edges, underscores, and casing transitions) must be evaluated to prevent false negatives without introducing false positives like `MONKEY`.
**Prevention:** Always use proper token boundary evaluation (checking adjacent characters for delimiters or case transitions) when scanning for secret keywords within identifiers.

## $(date +%Y-%m-%d) - Ensure all permutations of high-risk security keywords are explicitly registered
**Vulnerability:** A hardcoded secrets check (`contains_secret_literal`) failed to detect `APIKEY` as a secret, despite catching `API_KEY` and similar variations.
**Learning:** Case-boundary heuristics and substring matching logic may miss fully capitalized squashed acronyms like `APIKEY` if they are not explicitly registered as standalone keywords, leading to potential false negatives for common secret variable names.
**Prevention:** When updating or adding to the `tyc::contains_secret_literal` diagnostic (e.g., in `is_secret_name` or `secret_suffix`), explicitly register all common permutations of high-risk security keywords (like `APIKEY` and `API_KEY`), as boundary heuristics cannot be fully relied upon to catch all variations.

## 2025-02-14 - Ensure all permutations of high-risk security keywords are explicitly registered
**Vulnerability:** A hardcoded secrets check (`contains_secret_literal`) failed to detect `APITOKEN` and `APISECRET` as secrets, despite catching `API_KEY` and similar variations.
**Learning:** Case-boundary heuristics and substring matching logic may miss fully capitalized squashed acronyms like `APITOKEN` if they are not explicitly registered as standalone keywords, leading to potential false negatives for common secret variable names.
**Prevention:** When updating or adding to the `tyc::contains_secret_literal` diagnostic (e.g., in `is_secret_name` or `secret_suffix`), explicitly register all common permutations of high-risk security keywords (like `API_TOKEN` and `APITOKEN`), as boundary heuristics cannot be fully relied upon to catch all variations.

## 2024-05-24 - Synchronized Secret Scanning Heuristics
**Vulnerability:** A minor discrepancy existed between the `is_secret_name` heuristic dictionary in `tyc-analyse` and `secret_suffix` in the `tyc` build tools, specifically missing the keyword `"PASS"`.
**Learning:** This discrepancy could have allowed a variable like `DB_PASS` to be flagged in one context but silently ignored in another, creating a confusing and potentially leaky security gap.
**Prevention:** Always ensure that heuristic logic, such as dictionaries or regexes used across multiple crates or commands in a workspace, are either shared as a common library constant or strictly synchronized to maintain consistent security enforcement.

## 2026-07-04 - Longest-first Matching for Secret Detection
**Vulnerability:** Hardcoded secret detection was missing `APIKEY` due to partial overlap with `KEY` because `KEY` was evaluated before `APIKEY`.
**Learning:** When scanning for secrets using keyword arrays, substrings (like `KEY`) must be placed after longer matches (like `APIKEY` or `API_KEY`) to prevent premature partial matches.
**Prevention:** Always maintain hardcoded secret matching keywords in longest-first order.
## 2024-10-27 - [Fix Secret Scanning Boundary Checks]
**Vulnerability:** The hardcoded secret scanning logic (`is_secret_name` and `secret_suffix`) failed to detect secrets adjacent to numbers (e.g., `PASSWORD123`, `foo123PASSWORD`) or TitleCase word junctions (e.g., `dbPASSWORDString`).
**Learning:** The boundary logic assumed valid separators were strictly underscores, start/end of strings, or lowercase-to-uppercase casing transitions. It didn't account for digits acting as separators or the transition at the end of an uppercase block back to a lowercase word (which signifies the start of the next camelCase/TitleCase word, like `S` in `String`).
**Prevention:** When writing custom tokenization or substring boundary heuristics, explicitly test for alphanumeric transitions and ensure lookahead/lookbehind correctly models all valid case junction permutations, including digits.
