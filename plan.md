1. **Add missing high-risk permutations to the `SECRET_NAME_KEYWORDS` array** in `tyc/crates/tyc-analyse/src/lib.rs`.
   - The test script identified 86 missing permutations across prefixes (`DB`, `API`, `APP`, `CLIENT`, `JWT`, `SECRET`, `ACCESS`, `AUTH`, `BEARER`, `CSRF`) and bases (`PASSWORD`, `PWD`, `PASS`, `SECRET`, `TOKEN`, `KEY`).
   - Many common permutations like `API_PWD`, `APP_TOKEN`, and `DB_KEY` are missing.
   - I will use `replace_with_git_merge_diff` to add these explicitly to the list. I will order them longest-first to preserve the `secret_keyword_table_is_longest_first` invariant.
   - For example, `API_KEY` exists, but `DB_KEY` does not.
2. **Add unit tests** in `tyc/crates/tyc-analyse/src/lib.rs` and `tyc/crates/tyc/src/commands/build.rs`.
   - Use `replace_with_git_merge_diff` to add the missing variations to `secret_literal_fires_on_embedded_words` or a new test.
   - Also add them to `secret_suffix_matches_credential_names` in `tyc/crates/tyc/src/commands/build.rs`.
3. **Run tests** to ensure the new keywords don't break the longest-first invariant and pass the tests.
   - Run `cargo test -p tyc-analyse` and `cargo test -p tyc`.
4. **Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.**
5. **Submit the PR** using the `submit` tool.
