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
## $(date +%Y-%m-%d) - Sync secret detection keywords across tyc-analyse and tyc build
**Vulnerability:** Hardcoded secret keywords were added to `tyc-analyse` but not synchronized with the `secret_suffix` function in the `tyc` build commands.
**Learning:** The `tyc::contains_secret_literal` heuristic is duplicated across different crates (`tyc-analyse` for static analysis and `tyc` for build-time evaluation of `comptime let`). Updating one without the other leads to inconsistent diagnostic behavior depending on how the code is evaluated.
**Prevention:** When adding new keywords to the `tyc::contains_secret_literal` hardcoded secret detection logic, ensure the changes are synchronized across both `is_secret_name` in `tyc-analyse` and `secret_suffix` in the `tyc` build commands to maintain consistent diagnostic behavior.
