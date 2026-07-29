# `tests/knobs/` — opt-in codegen coverage fixtures

Every directory here is a complete miniature Typhon project that exercises one
**opt-in** configuration knob end-to-end. They exist because `examples/` and
`stress/` run entirely on default configuration, so every knob that changes the
emitted Python had zero executed coverage.

Run them with:

```bash
cd tyc && cargo build --release && cd ..
scripts/knob-matrix.sh                    # all fixtures
scripts/knob-matrix.sh --filter auto-     # a subset
```

Full documentation — fixture file format, what each check proves, and how to add
a knob — is in [`docs/differential-testing.md`](../../docs/differential-testing.md).

These are **not** part of the `examples/` or `stress/` regression corpus and are
not covered by `scripts/vm-differential.sh`; the knob matrix runs its own VM
comparison per fixture.

## Adding a knob

Create a directory with `typhon.toml` (knob on), `control.toml` (knob off),
`src/main.ty`, `emit-contains.txt` (what the rewrite emits) and `expect.txt`
(the program's stdout). Nothing in `scripts/knob-matrix.sh` needs to change.

Keep each fixture small and make the program's output *depend on the rewrite
being correct* — a fixture that would print the same thing whether or not the
knob worked is not coverage.
