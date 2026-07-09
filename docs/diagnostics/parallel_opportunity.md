# tyc::parallel_opportunity

Advice-level diagnostic, **on by default** — but only fires when the project
targets free-threaded Python (`[python] free-threaded = true`). Surfaces a
comprehension or integer accumulator loop that *could* be parallelised across a
thread pool (or PEP 734 sub-interpreters) on a free-threaded build, but whose
rewrite is currently disabled — or a `float` accumulator loop that matches every
reduction condition except the required `int` annotation.

Gated by `[strictness] suggest-parallel` (default `true`). Because the example /
stress corpus never sets `free-threaded`, this lint is silent across it by
construction.

## When it fires

1. **A parallelisable comprehension while `auto-parallel` is off.**

   ```ty
   # [python] free-threaded = true, [strictness] auto-parallel not set
   ys: list[int] = [transform(x) for x in xs]   # advice: could be parallelised
   ```

   `transform` is a `@pure` function, so `[transform(x) for x in xs]` qualifies
   for the auto-parallel rewrite. Enabling `[strictness] auto-parallel = true`
   lowers it to `typhon_runtime.parallel.map_pure(lambda x: transform(x), xs)`.
   The advice fires for the same widened shapes the rewrite handles — filtered
   (`… if COND`), multi-argument (`transform(x, k)` with a `let`-bound `k`), and
   nested (`g(f(x))`) pure calls.

2. **An integer accumulator loop while `auto-parallel-reductions` is off.**

   ```ty
   mut total: int = 0
   for x in xs:
       total += square(x)                       # advice: could be a parallel reduction
   ```

   Integer addition is associative and commutative and Python ints are exact,
   so `total += sum(map_pure(lambda x: square(x), xs))` is a semantics-preserving
   rewrite. Enable **both** `[strictness] auto-parallel = true` and
   `auto-parallel-reductions = true`.

   The advice mirrors the rewrite's full eligibility, including the iterable:
   `xs` must be provably bounded and effect-free to materialise — a
   `list`/`tuple`/`set` literal, a bare name annotated `list[...]` /
   `tuple[...]` / `set[...]` / `frozenset[...]` in the loop's scope, or a
   direct builtin `range(...)` call — because the rewrite's `map_pure` runs
   `list(ITER)` before evaluating a single element (an unbounded or stateful
   iterator would diverge from the sequential loop, so such loops neither
   rewrite nor produce this advice). Parallelising a `range` loop materialises
   the range — an inherent cost of the map-based design.

3. **A float accumulator loop that is *not* eligible.**

   ```ty
   mut total: float = 0.0
   for x in xs:
       total += x                               # advice: float reordering changes results
   ```

   A `float` accumulator matches every reduction condition except the `int`
   annotation. Floats are **never** auto-parallelised: reordering IEEE-754
   addition changes the result (`(a + b) + c != a + (b + c)`), so the compiler
   refuses to reorder it silently.

## Why it's advice, never a rewrite here

The lint never rewrites anything — it just names the knob to flip (cases 1 and
2) or explains why the shape is ineligible (case 3). Parallel execution is a
behaviour change the author opts into.

## Fix

* Cases 1 & 2 — flip the named `[strictness]` knob(s) on. The rewrite is
  semantics-preserving for pure comprehensions and integer reductions.
* Case 3 — refactor to an `int` accumulation if you can, or, if the precision
  tolerance is acceptable, write the parallel reduction explicitly with
  `sum(typhon_runtime.parallel.map_pure(lambda x: EXPR, ITER))` so the
  reordering is a deliberate, visible choice.
* Silence the whole family project-wide with `[strictness] suggest-parallel =
  false`.

See https://github.com/CodeHalwell/Typhon/blob/main/docs/diagnostics/parallel_opportunity.md
