## 2025-02-14 - Add allow-secret-comptime config to strictness
**Vulnerability:** A hardcoded secrets check (`contains_secret_literal`) printed a warning but could not be silenced because the configuration property `allow_secret_comptime` was missing.
**Learning:** Hardcoded secret checks that produce false positives can train users to ignore security warnings if there is no way to suppress them.
**Prevention:** Implement the suppression configuration knob to allow users to document and silence expected findings.

## 2025-02-14 - Improve Secret Detection Accuracy
**Vulnerability:** The secret detection scanner (`tyc::contains_secret_literal`) incorrectly flagged innocuous words ending in "KEY", "PASS", or "PWD" (like `MONKEY` or `COMPASS`), leading to false positives and potential alert fatigue.
**Learning:** Checking for substrings without validating boundaries (such as underscores, camelCase, or numeric separators) can flag perfectly safe variable names, leading developers to either ignore warnings or disable the check entirely. Safe boundary detection requires careful byte-index mapping when dealing with uppercased strings in Rust, as slicing the original string using lengths from the uppercased version can cause runtime panics if characters change size (though ASCII preserves length).
**Prevention:** When scanning for sensitive suffix patterns, always ensure that short, common endings are preceded by definitive word boundaries (e.g., `_`, camelCase case-shifts, or non-alphabetic characters), while adding explicit exceptions for known combined forms (like `APIKEY`).
