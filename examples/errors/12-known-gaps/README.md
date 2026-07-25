# Known gaps — programs that *should* fail to compile and don't

Every `.ty` file in this directory is broken: each one type-checks cleanly
today and then fails at runtime. They are checked in deliberately, as
executable documentation of where the type checker is currently silent.

Each file carries a `# KNOWN-GAP:` header describing the runtime failure it
produces, and is asserted by `tyc/crates/tyc/tests/error_examples.rs` to emit
**no diagnostics at all**. That assertion is a tripwire, not an endorsement:
the day `tyc` learns to catch one of these, the test fails, and whoever made
the fix moves the file into the matching `NN-*/` directory with a real
`# EXPECT-ERROR:` header.

Do not copy anything from this directory into real code.
