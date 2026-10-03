# GPT-6 Codex W2 fixes — 2026-10-03

The 20 items in gpt-6-codex-2026-10-03.fixes.md are implemented in isolated worktree /tmp/typhon-w2-codex on local branch fix/W2-codex, based on main bcddead2. No push, merge or PR was performed. Shared checkout changes from the initial review-fix pass remain preserved; see the separate other-workstreams handoff.

The branch includes builtin and member contracts, contextual expression facts, unchecked-site reporting, checked-cast and freeze runtimes, Callable/ParamSpec provenance, coroutine and go checks, Result/task/gather binding types, arithmetic/AugAssign results, enum/comparison checks, rigid nested generic returns, newtype methods, mutation contracts, writable interface conformance, property/ClassVar/slot writes, builtin false-positive fixes, class lint corrections and iterative attribute-chain inference. Follow-ups cover ParamSpec keyword calls, class-factory aliases, synchronous go aliases and comparison help.

## Validation

- Rust toolchain: 1.94.1.
- Checker: 696 unit tests passed. Resolver: 93 passed. Diagnostics: 62 passed.
- fmt, touched-crate clippy with all targets/features and -D warnings, and CLI build passed.
- Portable full checker corpus: 1481 units; 1216 accepted / 265 rejected. Seven additional rejections against main are runtime-proven failures, detailed in W2-06/08/14/15 reports. No other corpus rejection was added.
- CPython 3.13 runtime checks cover casts, freezes, interface writes, negative powers, container mutation and builtin false-positive controls. Generated-runtime and CLI strictness probes are recorded in per-item reports.
- Attribute chains: 2k/4k/8k links check in 0.125/0.210/0.559 seconds, replacing 1.18/4.84/34.44 seconds and the 8k stack abort. Defaulted class lint checks at 500/1000/2000 classes take 0.528/1.084/1.810 seconds.
- Earlier CLI suite run: 311 passed, one existing macOS build-output-directory assertion failed. Full workspace/Linux differential, knob and machine-specific performance gates were not run; those remain integration gates in the master plan.

## Integration dependencies

- This isolated branch includes the one-line slice resolver whitelist addition needed by W2-17 and shared diagnostic wording needed by W2-09/18. W1/W6 should omit duplicate changes when integrating those items. These small dependencies were validated with the owning crates' tests.
- W3 owns imported function async/sync metadata. W2 consumes coroutine signatures and keeps imported calls permissive where the base branch lacks authoritative metadata; its local sync aliases and callable contracts are checked. The W3 request remains in the companion fix file.
- W5 owns the seven additions to the checker-rejection baseline; requests include runtime proofs and item hashes.
- W6 received the go decision and already corrected the shared checkout's ThreadPoolExecutor promise. Its docs commits are integrated separately.
- W2-20 exposes check_module_with_imports_and_types / CheckedModule.type_at for W3 extension lowering.

Per-item reports and runtime proof scripts are adjacent. The full corpus result JSON remains at /tmp/w2-corpus-11-followup.json; compact summary is saved here.

## Local commits

```text
5a273cac fix(W2-01): type builtin results and iterator elements
2eb42e1c feat(W2-20): expose contextual checker expression types
b9b5c556 perf(W2-19): infer attribute chains iteratively
0a204729 fix(W2-03): preserve union member contracts
45be59e2 feat(W2-02): report unchecked sites in debug builds
d0113932 fix(W2-04): enforce supported checked cast targets
6e91c94b fix(W2-05): type recursively frozen bindings honestly
ed8fa1a8 W2-06 enforce and preserve callable parameter contracts
2229e421 W2-07 check concrete await operands and preserve coroutine contracts
d7c150ae W2-08 type powers and augmented assignment by their result
2ffa5e17 W2-09 check dataclass enum ordering and string membership
d4cdc89c W2-10 type task gather and Result expression bindings
696e61e2 W2-11 reject go results from known synchronous callees
c7a02fb7 W2-12 keep generic body parameters rigid in nested types
4faff4d7 W2-13 preserve base method contracts on newtypes
6d7f8eec W2-14 check container write keys indices and update payloads
877509a1 W2-15 enforce writable invariant interface field contracts
457b9b57 W2-16 check property and ClassVar writes with inherited slot semantics
11e55109 W2-17 fix builtin constructor keyword and nullable call contracts
192bf823 W2-18 restrict slot lint to class accesses and correct Result error wording
e12b768d W2-06 preserve ParamSpec keyword names and class factory aliases
a06871be W2-11 reject synchronous go aliases and callable contracts
7faac2f6 W2-09 make comparison help applicable to class operators
```
