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
## 2024-05-27 - Enhance hardcoded secret detection boundary logic
**Vulnerability:** The secret detection scanner missed hardcoded secrets when they were flanked by digits (e.g., `TOKEN123`) or functioning as acronym prefixes in PascalCase/CamelCase names (e.g., `TOKENString`). This could allow users to inadvertently embed secrets.
**Learning:** The previous boundary logic only checked for `_` or transitions between lowercase and uppercase letters. It did not recognize numbers as valid boundary delineators for secret substrings, nor did it realize that a sequence like "TOKENString" acts as a boundary because the lower-case 't' acts as a pivot after the 'S'.
**Prevention:** Ensured boundary logic correctly handles digits (`.is_ascii_digit()`) and correctly identifies the start of new camel/Pascalcase segments by checking `!name.as_bytes()[actual_end - 1].is_ascii_uppercase()` OR `name.as_bytes()[actual_end + 1].is_ascii_lowercase()` across both `is_secret_name` and `secret_suffix` functions.
## 2026-08-19 - Expanded secret literal keyword scanning
**Vulnerability:** The static analysis secret detector (`is_secret_name`) and CLI tool (`secret_suffix`) missed common high-risk variable names like `AUTHORIZATION`, `CREDENTIALS`, `WEBHOOK`, `SIGNING`, `COOKIE`, and `DSN`, allowing hardcoded secrets using these prefixes/suffixes to pass undetected.
**Learning:** Hardcoded secrets often hide in variables outside of traditional 'PASSWORD' and 'KEY' names; a static scanner's keyword heuristics must cover a broad range of token and credential naming patterns to be effective. The doc comment incorrectly described the function as a "suffix" check when it actually did a bounded substring scan anywhere in the string.
**Prevention:** Regularly audit and update the `SECRET_NAME_KEYWORDS` list against modern application credential patterns, maintaining the longest-first ordering invariant to prevent premature matching.
