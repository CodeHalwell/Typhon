## 2025-02-14 - Add allow-secret-comptime config to strictness
**Vulnerability:** A hardcoded secrets check (`contains_secret_literal`) printed a warning but could not be silenced because the configuration property `allow_secret_comptime` was missing.
**Learning:** Hardcoded secret checks that produce false positives can train users to ignore security warnings if there is no way to suppress them.
**Prevention:** Implement the suppression configuration knob to allow users to document and silence expected findings.

## $(date +%Y-%m-%d) - Improve secret detection bounding logic
**Vulnerability:** The secret detection logic in `tyc-analyse` and `tyc build` only matched variable names that *ended* with secret suffixes (e.g., `_KEY`), missing embedded secrets (`API_KEY_FOO`) and camelCase secrets (`myTokenValue`).
**Learning:** Simple `ends_with` string matching is insufficient for static analysis security checks; boundary conditions (string edges, underscores, and casing transitions) must be evaluated to prevent false negatives without introducing false positives like `MONKEY`.
**Prevention:** Always use proper token boundary evaluation (checking adjacent characters for delimiters or case transitions) when scanning for secret keywords within identifiers.

## 2024-06-26 - Squashed API KEY secrets miss the contains_secret_literal check
**Vulnerability:** A hardcoded secrets check (`contains_secret_literal`) missed `APIKEY` due to word-boundary and casing rules (it is fully capitalized and squashed, lacking an underscore), even though it effectively functions as an API_KEY.
**Learning:** Hardcoded literal checking relies heavily on heuristics matching typical casings (`API_KEY`, `myApiKey`). Fully capitalized squashed acronyms (`APIKEY`) evade checks looking for boundaries (`_` or case transitions).
**Prevention:** Register all permutations of high-risk security keywords in hardcoded secrets lists, even those that violate stylistic naming conventions.
