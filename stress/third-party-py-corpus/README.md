# Third-party Python corpus

A curated set of typed Python source snippets that exercise common
third-party-library patterns Typhon's migrator + checker need to
survive. Each `*.py` is a self-contained module that compiles cleanly
under `mypy --strict` (or close to it) on its own. The companion
integration test `third_party_corpus_round_trips_cleanly` in
`tyc/crates/tyc/tests/pipeline.rs` runs each through:

1. `tyc migrate` — produces `.ty` source
2. `tyc check` — type-checks the migrated source

and fails CI if any file regresses.

This sweep complements the existing `corpus_examples_all_check_clean`
test, which exercises hand-written `.ty` files. The third-party
corpus probes the *migration* direction: real Python idioms entering
Typhon land for the first time.

## Files

| File | Patterns exercised |
|---|---|
| `dataclass_basic.py` | `@dataclass`, `Optional`, `from typing import`, module-level assigns |
| `dataclass_frozen.py` | `@dataclass(frozen=True)`, slots interplay |
| `protocol_basic.py` | `Protocol`, method-only interface |
| `newtype_basic.py` | `NewType("X", base)`, alias usage |
| `pep695_generic.py` | PEP 695 `def f[T](...)`, bounded type params |
| `legacy_generic.py` | `TypeVar` + `Generic[T]` base — pre-PEP-695 form |

Each file is intentionally short so the failure attribution is
unambiguous when a regression breaks the sweep.
