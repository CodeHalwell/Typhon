## 2025-02-14 - Add allow-secret-comptime config to strictness
**Vulnerability:** A hardcoded secrets check (`contains_secret_literal`) printed a warning but could not be silenced because the configuration property `allow_secret_comptime` was missing.
**Learning:** Hardcoded secret checks that produce false positives can train users to ignore security warnings if there is no way to suppress them.
**Prevention:** Implement the suppression configuration knob to allow users to document and silence expected findings.

## $(date +%Y-%m-%d) - Improve secret detection bounding logic
**Vulnerability:** The secret detection logic in `tyc-analyse` and `tyc build` only matched variable names that *ended* with secret suffixes (e.g., `_KEY`), missing embedded secrets (`API_KEY_FOO`) and camelCase secrets (`myTokenValue`).
**Learning:** Simple `ends_with` string matching is insufficient for static analysis security checks; boundary conditions (string edges, underscores, and casing transitions) must be evaluated to prevent false negatives without introducing false positives like `MONKEY`.
**Prevention:** Always use proper token boundary evaluation (checking adjacent characters for delimiters or case transitions) when scanning for secret keywords within identifiers.
## 2026-06-24 - Enhance secret-suffix detection with APIKEY
**Vulnerability:** The secret detection scanner (`contains_secret_literal`) missed "APIKEY" suffixes due to the token bounds check logic only explicitly looking for "API_KEY" and "KEY". Because "I" is uppercase, the scanner would falsely reject "APIKEY" when attempting to match "KEY" inside it.
**Learning:** Static analysis checks that rely on case-boundary transition heuristics for tokenization can inadvertently create blind spots for fully capitalized acronyms (like APIKEY) if not explicitly registered.
**Prevention:** Register all permutations of high-risk security keywords, including common squashed acronyms like "APIKEY" and "API_KEY", directly into the scanner vocabulary.
