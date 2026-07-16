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

## $(date +%Y-%m-%d) - Expand hardcoded secret detection keywords
**Vulnerability:** The secret detection lists in `tyc-analyse` and `tyc build` lacked common keywords like `CREDENTIALS`, `AUTH_TOKEN`, and `PRIVATE_KEY`, resulting in false negatives for hardcoded credentials.
**Learning:** Security heuristics for detecting hardcoded secrets must be regularly updated to cover common and emerging naming conventions for sensitive data beyond standard 'KEY' or 'PASSWORD' terminology.
**Prevention:** Keep heuristic arrays synced across all toolchain paths (analysis and build) and ensure comprehensive permutations (like `AUTH`, `AUTH_TOKEN`, `CREDENTIALS`, `PRIVATE_KEY`) are added, preserving longest-first matching order to avoid premature token-boundary false negatives.
