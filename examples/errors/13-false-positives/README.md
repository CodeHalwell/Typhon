# False positives — valid programs the checker wrongly rejects

The inverse of [`12-known-gaps/`](../12-known-gaps/). Every `.ty` file here is
**correct Typhon that runs correctly once compiled**, and `tyc check` rejects
it anyway.

Each file carries a `# FALSE-POSITIVE:` header explaining why the program is
valid, followed by the `# EXPECT-ERROR:` line recording the diagnostic that
currently (wrongly) fires. The harness in
`tyc/crates/tyc/tests/error_examples.rs` asserts that diagnostic still appears
— so the day the false positive is fixed, the test fails and whoever fixed it
deletes the file. That is the intent: these entries are meant to disappear.

Do not use anything here as a style example. The code is idiomatic; the
compiler is wrong.
