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
**Vulnerability:** The hardcoded secrets check (`tyc::contains_secret_literal`) failed to identify common credential-related identifiers like `CREDENTIALS`, `BEARER`, `PASSWD`, `PASSPHRASE`, and `AUTHTOKEN`, leaving potential security risks undetected during static analysis.
**Learning:** Expanding the keyword list significantly improves detection accuracy and broadens the security coverage against modern token conventions (e.g., JWT, OAuth Bearer, and generic credentials). Additionally, explicitly ordering keywords by length (longest-first, e.g., matching `APIKEY` before `KEY`) prevents shorter subsets from shadowing and misreporting the full context of the match.
**Prevention:** When expanding the `tyc::contains_secret_literal` detection in the future, always insert new terms in descending length order and ensure parity across both `is_secret_name` in `tyc-analyse` and `secret_suffix` in `tyc build`.
