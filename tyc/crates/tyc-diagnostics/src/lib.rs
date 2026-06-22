//! Diagnostic infrastructure for the Typhon compiler.
//!
//! Every compiler error or warning is represented as a [`TycError`] that
//! implements [`miette::Diagnostic`], giving rich source-span rendering in
//! the terminal.

use miette::{Diagnostic, NamedSource, SourceOffset, SourceSpan};
use thiserror::Error;

/// A Typhon compiler error with source-location information.
#[derive(Debug, Clone, Error, Diagnostic)]
pub enum TycError {
    /// The source file could not be read.
    #[error("could not read file '{path}': {cause}")]
    #[diagnostic(
        code(tyc::io),
        url("https://typhon.dev/lang/diagnostics/io"),
        help("check that the file exists and is readable")
    )]
    Io { path: String, cause: String },

    /// The source file could not be parsed as a valid Typhon/Python program.
    ///
    /// The optional `suggestion` field is populated by [`TycError::parse`]
    /// when the underlying Python parser message matches a known
    /// Typhon-specific construct (multi-line `|>` chain without parens,
    /// `freeze let` inside a function, …). Those constructs go through
    /// the preprocess pipeline before the parser sees them, so the raw
    /// parser message ("Unexpected indentation", "Simple statements must
    /// be separated by newlines or semicolons") is meaningless to a
    /// Typhon user. The hint redirects them to the correct form.
    /// FINDINGS #34, #35.
    #[error("parse error in '{path}'")]
    #[diagnostic(code(tyc::parse), url("https://typhon.dev/lang/diagnostics/parse"))]
    Parse {
        path: String,
        message: String,
        #[help]
        suggestion: Option<String>,
        #[source_code]
        src: NamedSource<String>,
        #[label("{message}")]
        span: SourceSpan,
    },

    /// A `let` binding was re-assigned after its declaration.
    #[error("cannot assign to immutable binding '{name}'")]
    #[diagnostic(
        code(tyc::immutable_assign),
        url("https://typhon.dev/lang/diagnostics/immutable_assign"),
        help("change `let` to `mut` if you need a mutable binding")
    )]
    ImmutableAssign {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("first declared here")]
        declaration: SourceSpan,
        #[label("illegal re-assignment here")]
        assignment: SourceSpan,
    },

    /// A `case Foo(name):` pattern binds a name that already exists as
    /// an immutable `let` in an enclosing scope. Python semantics make
    /// the pattern binding a real rebinding (visible after the `match`),
    /// so under Rule 2 it would trip `tyc::immutable_assign` — but
    /// Rust/OCaml/Scala programmers reach for pattern captures
    /// reflexively and `change \`let\` to \`mut\`` is the wrong advice
    /// (the user wants a fresh binding, not a mutation site). This
    /// diagnostic surfaces the same shape with rename-the-capture as
    /// the actionable hint. FINDINGS O10.
    #[error("pattern capture `{name}` shadows an outer immutable `let {name}`")]
    #[diagnostic(
        code(tyc::pattern_shadows_outer),
        url("https://typhon.dev/lang/diagnostics/pattern_shadows_outer"),
        help("rename the pattern capture (e.g. `case Wrap({name}_inner):`) — pattern bindings are real rebindings under Python's `match` semantics, so reusing an outer immutable name is rejected. If you genuinely want to overwrite the outer binding, change its declaration to `mut`.")
    )]
    PatternShadowsOuter {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("outer `let` declared here")]
        declaration: SourceSpan,
        #[label("pattern capture here")]
        capture: SourceSpan,
    },

    /// A field on a `frozen` class was assigned outside the constructor.
    #[error("cannot assign to field '{field}' on frozen class `{class}`")]
    #[diagnostic(
        code(tyc::frozen_assign),
        url("https://typhon.dev/lang/diagnostics/frozen_assign"),
        help("`frozen` classes are immutable. Construct a new `{class}` with the desired values instead of mutating in place.")
    )]
    FrozenAssign {
        class: String,
        field: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("assignment to frozen field")]
        span: SourceSpan,
    },

    /// An instance constructed via `X.__new__(X)` or
    /// `object.__new__(X)` (bypassing the auto-generated `__init__`)
    /// escapes — returned, passed as an argument — without every
    /// required field having been assigned. Without this check, the
    /// emitted Python would crash with `AttributeError` the first
    /// time the missing field is read.
    #[error("instance of `{class}` escapes without all required fields set; missing: {missing}")]
    #[diagnostic(
        code(tyc::missing_field_init),
        url("https://typhon.dev/lang/diagnostics/missing_field_init"),
        help("either assign every required field (`{first_missing} = …`) before this point, or use the normal `{class}(...)` constructor which enforces field initialisation at compile time.")
    )]
    MissingFieldInit {
        class: String,
        missing: String,
        first_missing: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("instance escapes here with uninitialised field(s)")]
        span: SourceSpan,
    },

    /// A name is used but never declared in any enclosing scope.
    #[error("cannot find '{name}' in scope")]
    #[diagnostic(
        code(tyc::unknown_name),
        url("https://typhon.dev/lang/diagnostics/unknown_name"),
        help("declare '{name}' with `let` or `mut`, or import it from a module")
    )]
    UnknownName {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("not found in scope")]
        span: SourceSpan,
    },

    /// `self` was referenced outside an `impl ClassName:` method body.
    /// Distinct from the generic `tyc::unknown_name` so the help text can
    /// point users at the right shape (per Typhon Rule 4: methods take
    /// explicit `self`, free functions cannot).
    #[error("cannot find 'self' in scope")]
    #[diagnostic(
        code(tyc::self_outside_impl),
        url("https://typhon.dev/lang/diagnostics/self_outside_impl"),
        help("`self` is only available inside `impl ClassName:` method bodies. Move this function under an `impl` block, or replace `self` with an explicit parameter.")
    )]
    SelfOutsideImpl {
        #[source_code]
        src: NamedSource<String>,
        #[label("`self` is only valid inside an `impl` method body")]
        span: SourceSpan,
    },

    /// `from typing import TypeVar` is rejected — Typhon uses PEP 695
    /// type-parameter syntax (`def f[T](x: T) -> T:`) and the
    /// `TypeVar(...)` constructor is not a supported value.
    #[error("`from typing import TypeVar` is not supported in Typhon")]
    #[diagnostic(
        code(tyc::typevar_import_rejected),
        url("https://typhon.dev/lang/diagnostics/typevar_import_rejected"),
        help("Use PEP 695 syntax instead: `def f[T](x: T) -> T:` and `class Box[T]:` declare type parameters directly, no `TypeVar(...)` call needed.")
    )]
    TypeVarImportRejected {
        #[source_code]
        src: NamedSource<String>,
        #[label("remove this import and use `[T]` parameter syntax instead")]
        span: SourceSpan,
    },

    /// Importing a deprecated capitalised collection alias from `typing`
    /// (`List`, `Dict`, `Tuple`, `Set`, `FrozenSet`, `Type`) — Typhon prefers
    /// the built-in lowercase forms (PEP 585) for consistency with the rest
    /// of the language.
    #[error("`from typing import {name}` is deprecated in Typhon")]
    #[diagnostic(
        code(tyc::typing_alias_deprecated),
        url("https://typhon.dev/lang/diagnostics/typing_alias_deprecated"),
        help("Use the built-in lowercase `{lower}` instead — `{lower}[T]` works directly without importing anything.")
    )]
    TypingAliasDeprecated {
        name: String,
        lower: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("prefer `{lower}` over `typing.{name}`")]
        span: SourceSpan,
    },

    /// A call site passed a keyword argument whose name doesn't match
    /// any of the callee's parameters (positional or keyword-only) and
    /// the callee has no `**kwargs`. Distinct from `tyc::arg_count`
    /// because the user's mistake is a typo, not a count error.
    #[error("unknown keyword argument '{kwarg}' to `{fn_name}`")]
    #[diagnostic(
        code(tyc::unknown_kwarg),
        url("https://typhon.dev/lang/diagnostics/unknown_kwarg")
    )]
    UnknownKwarg {
        fn_name: String,
        kwarg: String,
        /// Pre-formatted help string. When a similar parameter name was
        /// found this reads "did you mean `<candidate>`?"; otherwise it
        /// lists every accepted parameter name.
        #[help]
        suggestion: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("not a parameter of `{fn_name}`")]
        span: SourceSpan,
    },

    /// A function declared with a non-`None` return type has at least
    /// one execution path that reaches the end of the body without
    /// `return` / `raise`. Equivalent to mypy's `missing-return`.
    #[error("function `{fn_name}` is missing a return on some paths (declared `-> {ret_type}`)")]
    #[diagnostic(
        code(tyc::missing_return),
        url("https://typhon.dev/lang/diagnostics/missing_return"),
        help("Add an explicit `return <{ret_type}>` (or `raise`) on every path, or widen the return type to `{ret_type} | None` / `None` if the function intentionally returns nothing.")
    )]
    MissingReturn {
        fn_name: String,
        ret_type: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("this function may fall off the end without a return value")]
        span: SourceSpan,
    },

    /// `let NAME: T` (or `mut NAME: T`) was written without an
    /// initialiser. Typhon requires every binding to have a value at
    /// declaration — the Rust-style "declare-then-assign-later" shape
    /// produces a confusing `tyc::immutable_assign` on the next
    /// assignment, so this dedicated diagnostic fires earlier.
    #[error("`{keyword} {name}: {annotation}` is missing an initialiser")]
    #[diagnostic(
        code(tyc::missing_initialiser),
        url("https://typhon.dev/lang/diagnostics/missing_initialiser"),
        help("Typhon bindings must be initialised at the point of declaration. Write `{keyword} {name}: {annotation} = <expr>` instead.")
    )]
    MissingInitialiser {
        keyword: String,
        name: String,
        annotation: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("missing `= <expr>` here")]
        span: SourceSpan,
    },

    /// A bare collection annotation (`list`, `dict`, `tuple`, `set`,
    /// `frozenset`) appears without element-type parameters. Per
    /// Typhon Rule 1 / `[strictness] no-implicit-any = true` (the
    /// default), every container annotation should spell out its
    /// element types.
    #[error("bare `{kind}` annotation has implicit `Any` element type")]
    #[diagnostic(
        code(tyc::implicit_any),
        url("https://typhon.dev/lang/diagnostics/implicit_any"),
        help("Spell out the element type so readers can see what the collection holds: `{kind}[<element-type>]`. For dicts use `dict[K, V]`; for tuples use `tuple[A, B, ...]` or `tuple[T, ...]` for a homogeneous tuple.")
    )]
    ImplicitAny {
        kind: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("missing `[<element type>]`")]
        span: SourceSpan,
    },

    /// A second `let`/`mut` binding tries to shadow an outer binding
    /// of the same name in the same function. Python's name scoping
    /// is function-level, so what looks like block-scoped shadowing
    /// actually rebinds the outer name — Typhon rejects this rather
    /// than silently accepting a confusing capture.
    #[error("cannot shadow `{name}` — Typhon names are function-scoped")]
    #[diagnostic(
        code(tyc::no_block_shadow),
        url("https://typhon.dev/lang/diagnostics/no_block_shadow"),
        help("Python doesn't have block scope, so a `let {name}: ...` inside a nested block would still rebind the outer `{name}`. Pick a different name, or remove the keyword to reuse the outer binding (if it's `mut`).")
    )]
    NoBlockShadow {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("first declared here")]
        decl_span: SourceSpan,
        #[label("re-declaration would shadow the outer binding")]
        span: SourceSpan,
    },

    /// A value of one type was used where another type was expected.
    ///
    /// The `#[help]` slot carries the suggestion, computed at construction
    /// time so it can adapt to the concrete shape of `expected` /
    /// `actual`. The default phrasing ("change the value or widen the
    /// annotation to `T | U`") is sound but unhelpful for the common
    /// invariant-collection case — `list[Dog]` flowing into a
    /// `list[Animal]` parameter shouldn't suggest `list[Animal] |
    /// list[Dog]`, it should suggest `Sequence[Animal]` (the covariant
    /// read-only view) or rebinding the source as `list[Animal]`.
    /// FINDINGS #37.
    #[error("type mismatch: expected `{expected}`, found `{actual}`")]
    #[diagnostic(
        code(tyc::type_mismatch),
        url("https://typhon.dev/lang/diagnostics/type_mismatch")
    )]
    TypeMismatch {
        expected: String,
        actual: String,
        #[help]
        suggestion: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("expected `{expected}`")]
        span: SourceSpan,
    },

    /// A reassignment supplied a value whose type doesn't match the
    /// binding's declared type. Distinct from [`TycError::TypeMismatch`]
    /// so the diagnostic can point at the original declaration site and
    /// explain `mut`'s semantics — `mut` allows new *values* of the same
    /// type, not a re-declaration with a new type. Without this variant
    /// users routinely interpret the bare "type mismatch" as a bug,
    /// since they wrote a literal `mut name = …` and (reasonably)
    /// expected that to behave like a fresh declaration.
    ///
    /// The labels are kept terse on purpose: miette renders one
    /// connector per labelled span, and verbose label text quickly
    /// turns a two-site diagnostic into a wall of branches on narrow
    /// terminals. The redundant information (binding name, declared
    /// type) lives in the headline message; the labels only carry
    /// what's unique to each anchor.
    #[error("cannot assign `{actual}` to `{name}: {expected}`")]
    #[diagnostic(
        code(tyc::type_mismatch),
        url("https://typhon.dev/lang/diagnostics/type_mismatch"),
        help("`mut` only permits new values of the declared type. Use a different name to bind type `{actual}`.")
    )]
    TypeReassignMismatch {
        name: String,
        expected: String,
        actual: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("`{actual}`")]
        span: SourceSpan,
        #[label("declared here")]
        decl_span: SourceSpan,
    },

    /// A binary operator was applied to operands whose types are
    /// incompatible per Python's runtime semantics — e.g. `str + int`
    /// or `list + dict`. The check is conservative: it fires only on
    /// clearly-wrong pairs whose operand types are both fully known and
    /// neither side is a user-defined `class` (which might define a
    /// custom `__add__` / `__mul__` / etc.).
    #[error("unsupported operand types for `{op}`: `{lhs}` and `{rhs}`")]
    #[diagnostic(
        code(tyc::operator_type_mismatch),
        url("https://typhon.dev/lang/diagnostics/operator_type_mismatch"),
        help("convert one operand so the types match (e.g. `str(n)` / `int(s)`)")
    )]
    OperatorTypeMismatch {
        op: String,
        lhs: String,
        rhs: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("operator `{op}` does not apply to `{lhs}` and `{rhs}`")]
        span: SourceSpan,
    },

    /// A constant integer index into a fixed-arity tuple is out of
    /// range — the tuple type carries its element count statically so
    /// the lookup can be flagged at type-check time. FINDINGS R3.10.
    #[error("tuple has {arity} element(s); index {index} is out of range")]
    #[diagnostic(
        code(tyc::tuple_index_out_of_range),
        url("https://typhon.dev/lang/diagnostics/tuple_index_out_of_range"),
        help("use an index in `0..{arity}` (or `-{arity}..0` for negative indexing)")
    )]
    TupleIndexOutOfRange {
        arity: usize,
        index: i64,
        #[source_code]
        src: NamedSource<String>,
        #[label("index `{index}` is out of range for `tuple` of arity {arity}")]
        span: SourceSpan,
    },

    /// A nullable value (`T | None`) was used in a position requiring `T`.
    #[error("possibly-None value used where `{expected}` is required")]
    #[diagnostic(
        code(tyc::nullable_use),
        url("https://typhon.dev/lang/diagnostics/nullable_use"),
        help("guard the value with `if {name} is not None:` to narrow it to `{expected}`")
    )]
    NullableUse {
        name: String,
        expected: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("value is `{expected} | None` here")]
        span: SourceSpan,
    },

    /// The error type propagated by `?` from a callee does not match the
    /// caller's `Result[T, E]` declaration. Distinct from the generic
    /// `tyc::type_mismatch` so users see immediately that the failure is
    /// at a `?`-propagation boundary and can act accordingly (convert at
    /// the boundary, or change one of the function signatures).
    #[error("`?` propagates `Err[{actual_err}]` into `Result[_, {expected_err}]`")]
    #[diagnostic(
        code(tyc::result_error_mismatch),
        url("https://typhon.dev/lang/diagnostics/result_error_mismatch"),
        help("the `?` operator forwards the callee's `Err` value as-is; convert it with a `match` or change one signature so the error types match")
    )]
    ResultErrorMismatch {
        expected_err: String,
        actual_err: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("error type does not match the enclosing function's `Result`")]
        span: SourceSpan,
    },

    /// A function was called with the wrong number of positional arguments.
    ///
    /// Special-case: when `expected == actual` the literal "expected N,
    /// got N" reads as a self-contradiction. This shape appears when the
    /// upstream type-checker bumps `expected` to include keyword-only
    /// parameters (so the count comparison passes if you sum
    /// positional + kw-only) but the user passed every argument
    /// positionally. The diagnostic rephrases for that case via the
    /// `#[help]` slot so the user sees "pass them by name" instead of
    /// "expected 2, got 2". FINDINGS #36.
    #[error("wrong number of arguments to `{name}`: expected {expected}, got {actual}")]
    #[diagnostic(
        code(tyc::arg_count),
        url("https://typhon.dev/lang/diagnostics/arg_count")
    )]
    WrongArgCount {
        name: String,
        expected: usize,
        actual: usize,
        /// Optional help string populated by the constructor when the
        /// count-equal case fires. `Some("function `f` declares some
        /// parameters as keyword-only; pass them by name (e.g.
        /// `f(a=1, b=2)`)")` for kw-only mismatch; `None` otherwise so
        /// miette skips the help block.
        #[help]
        suggestion: Option<String>,
        #[source_code]
        src: NamedSource<String>,
        #[label("called with {actual} argument(s) here")]
        span: SourceSpan,
    },

    /// A function or constructor was called without filling a required
    /// parameter — the caller can name *which* argument is missing,
    /// unlike [`WrongArgCount`] which only reports counts. Surfaces
    /// the field name in the error message so the fix is immediate
    /// ("add `client=...`" rather than "expected 1, got 4 — but the
    /// 4 you gave are all fine, you just missed the one required
    /// one").
    #[error("missing required argument{plural} to `{name}`: {missing_list}")]
    #[diagnostic(
        code(tyc::missing_argument),
        url("https://typhon.dev/lang/diagnostics/missing_argument"),
        help("supply {missing_list} when calling `{name}`")
    )]
    MissingArgument {
        name: String,
        /// Names of the required parameters that weren't supplied,
        /// in declaration order. Always non-empty when this variant
        /// fires.
        missing: Vec<String>,
        /// Pre-rendered comma-separated list (with backticks) for
        /// inclusion in `#[error]` / `#[help]`. Built once at
        /// construction so the `Display` impl stays straightforward.
        missing_list: String,
        /// `""` for one missing name, `"s"` for many — drops the
        /// plural conditional out of the format string.
        plural: &'static str,
        #[source_code]
        src: NamedSource<String>,
        #[label("missing here")]
        span: SourceSpan,
    },

    /// Something that is not callable was called.
    #[error("`{typ}` is not callable")]
    #[diagnostic(
        code(tyc::not_callable),
        url("https://typhon.dev/lang/diagnostics/not_callable")
    )]
    NotCallable {
        typ: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("this value is not a function")]
        span: SourceSpan,
    },

    /// A function uses `yield` (or `yield from`) but its declared
    /// return type isn't iterator-shaped. Calling it returns a
    /// generator object at runtime, not the declared type. FINDINGS #51.
    #[error("`{fn_name}` contains `yield` so it returns a generator, not `{returned}`")]
    #[diagnostic(
        code(tyc::generator_return_type),
        url("https://typhon.dev/lang/diagnostics/generator_return_type"),
        help(
            "annotate the return type as `Iterator[T]` / `Generator[T, S, R]` (or `AsyncIterator[T]` / `AsyncGenerator[T, S]` for `async def`)"
        )
    )]
    GeneratorReturnType {
        fn_name: String,
        returned: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("declared return type here")]
        span: SourceSpan,
    },

    /// A `class` body declared `__init__` directly. Typhon generates
    /// the constructor from the field annotations, so writing one
    /// manually conflicts with the emitted dataclass / model. Use
    /// field defaults or a free factory function instead. FINDINGS #50.
    #[error("`{class_name}.__init__` cannot be defined — the constructor is generated from the class fields")]
    #[diagnostic(
        code(tyc::manual_init),
        url("https://typhon.dev/lang/diagnostics/manual_init"),
        help(
            "remove `__init__`; set per-field defaults on the class or write a free factory function"
        )
    )]
    ManualInit {
        class_name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("declared __init__ here")]
        span: SourceSpan,
    },

    /// A `class` body declared a field without a default value after
    /// one that has a default. The emitted `@dataclass(slots=True)`
    /// generates `__init__` in declaration order, and Python's
    /// signature rules forbid a positional parameter without a
    /// default following one with a default — without this check, the
    /// class definition would blow up at *import* time with a
    /// `TypeError: non-default argument 'X' follows default argument
    /// 'Y'`. Mirrors how the parser already rejects the same shape on
    /// free functions, so the rule is consistent across both surface
    /// forms (R3-11).
    #[error("class `{class_name}` field `{non_default}` (no default) is declared after `{prior_default}` (has a default)")]
    #[diagnostic(
        code(tyc::field_default_ordering),
        url("https://typhon.dev/lang/diagnostics/field_default_ordering"),
        help("move every field without a default above every field with one — the generated `__init__` follows Python's rule that non-default parameters precede default ones, otherwise the class fails to construct at import time")
    )]
    FieldDefaultOrdering {
        class_name: String,
        non_default: String,
        prior_default: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("non-default field declared after a defaulted field")]
        span: SourceSpan,
    },

    /// A sync function called an `async def` without awaiting it. The
    /// expression's value is a coroutine, not the function's declared
    /// return type — and Python's runtime emits "coroutine was never
    /// awaited" warnings for these. FINDINGS #49.
    #[error("missing `await` on async call to `{callee}`")]
    #[diagnostic(
        code(tyc::missing_await),
        url("https://typhon.dev/lang/diagnostics/missing_await"),
        help(
            "wrap the call in `await` (and make the caller `async`), or call `asyncio.run(...)` if you are at the top level"
        )
    )]
    MissingAwait {
        callee: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("this call returns a coroutine — `await` it")]
        span: SourceSpan,
    },

    /// A `match` on a sealed union does not cover all variants and has no wildcard arm.
    #[error("non-exhaustive `match` on sealed union `{union_name}`: missing variant(s) {missing}")]
    #[diagnostic(
        code(tyc::non_exhaustive_match),
        url("https://typhon.dev/lang/diagnostics/non_exhaustive_match"),
        help("add a `case <Variant>():` arm for each missing variant, or add a `case _:` wildcard arm")
    )]
    NonExhaustiveMatch {
        union_name: String,
        missing: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("match is not exhaustive")]
        span: SourceSpan,
    },

    /// A `comptime` binding could not be evaluated at build time.
    #[error("comptime evaluation failed for '{name}': {message}")]
    #[diagnostic(
        code(tyc::comptime),
        url("https://typhon.dev/lang/diagnostics/comptime"),
        help("comptime expressions support: int/float/str/bool literals, list/tuple/dict literals, arithmetic, comparisons, boolean ops (and/or/not), ternaries (`x if c else y`), env(\"NAME\"[, \"default\"]), int()/str()/float()/len(), pure str methods (upper, lower, strip, lstrip, rstrip, replace, startswith, endswith, split), and calls to user-defined `comptime def` functions")
    )]
    Comptime { name: String, message: String },

    /// Generic error with a human-readable message (used during early phases).
    #[error("{message}")]
    #[diagnostic(code(tyc::generic), url("https://typhon.dev/lang/diagnostics/generic"))]
    Generic { message: String },

    /// The `?` error-propagation operator was used in a position where
    /// it cannot lower correctly. Two common reasons: (a) the enclosing
    /// function doesn't return `Result[T, E]`, or (b) the `?` sits
    /// inside a comprehension (the lowering can't hoist a short-circuit
    /// out of a comprehension's local scope). The error `message` is
    /// case-specific; the static help text below covers both.
    #[error("{message}")]
    #[diagnostic(
        code(tyc::invalid_question_op),
        url("https://typhon.dev/lang/diagnostics/invalid_question_op"),
        help("the `?` operator must appear inside a function whose return type is `Result[T, E]`, AND it must not appear inside a comprehension. Rewrite a comprehension as an explicit `for`-loop, or move the `?` call out into a `let` binding before the comprehension.")
    )]
    InvalidQuestionOp {
        message: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("invalid use of `?` here")]
        span: SourceSpan,
    },

    /// An imported name is never used in the module.
    ///
    /// Default severity is **warn** — almost every linter treats unused
    /// imports as a stylistic nit rather than a hard error, and bare
    /// `tyc check` should match that expectation. Codebases that want to
    /// keep their imports rigorously pruned can flip `[strictness]
    /// unused-import = "error"` in `typhon.toml` to promote the
    /// diagnostic back into an error via
    /// [`crate::commands::util::apply_strictness`]. FINDINGS #41.
    #[error("imported name '{name}' is never used")]
    #[diagnostic(
        severity(Warning),
        code(tyc::unused_import),
        url("https://typhon.dev/lang/diagnostics/unused_import"),
        help("remove the import, or prefix it with `_` if it is intentionally unused")
    )]
    UnusedImport {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("imported here but never used")]
        span: SourceSpan,
    },

    /// A `lazy` construct was used in an unsupported form.
    #[error("{message}")]
    #[diagnostic(
        code(tyc::lazy_usage),
        url("https://typhon.dev/lang/diagnostics/lazy_usage"),
        help("`lazy` supports `lazy import name = module` and `lazy val NAME: T = expr` only")
    )]
    LazyUsage {
        message: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("unsupported lazy form here")]
        span: SourceSpan,
    },

    /// An `extend` declaration named a Python built-in type.
    #[error("{message}")]
    #[diagnostic(
        code(tyc::extend_builtin),
        url("https://typhon.dev/lang/diagnostics/extend_builtin"),
        help("Python's built-in types cannot be modified at runtime; wrap the value in a user-defined class or expose a free function")
    )]
    ExtendBuiltin {
        message: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("cannot extend a built-in type")]
        span: SourceSpan,
    },

    /// A project `.ty` module's filename matches a Python stdlib module name
    /// (`types`, `ast`, `string`, `io`, `json`, …). When the build output
    /// directory is on `sys.path` (as it is for the default `python
    /// build/main.py` entry point), a transitive stdlib import will resolve
    /// to the project module instead of the standard library and the user
    /// will see a baffling `ImportError` or `AttributeError` blamed on an
    /// innocent stdlib package. R2-4 in apps-feedback.
    ///
    /// Default severity is **warn** — the file still compiles; the user
    /// just needs to know that running the emitted Python may break. Rename
    /// the file (e.g. `lang_types.ty`, `lang_ast.ty`) to silence.
    #[error("module name `{name}` shadows the Python stdlib module of the same name")]
    #[diagnostic(
        severity(Warning),
        code(tyc::stdlib_module_shadow),
        url("https://typhon.dev/lang/diagnostics/stdlib_module_shadow"),
        help(
            "the emitted `build/{name}.py` will be on `sys.path` and \
             intercept transitive `import {name}` from other stdlib modules, \
             producing surprising `ImportError`s — rename the file to \
             something stdlib-disjoint (e.g. `lang_{name}.ty`)"
        )
    )]
    StdlibModuleShadow {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("module name shadows the stdlib")]
        span: SourceSpan,
    },

    /// A binding declared inside an `unsafe:` block flows into a
    /// concretely-typed `return` site without being re-asserted (cast
    /// or re-annotated). Rule 5 in the Typhon language spec: an unsafe
    /// value carries `Unknown` and must cross the safety boundary
    /// via a deliberate re-typing.  O14 / FINDINGS #107.
    #[error(
        "`{name}` was introduced inside `unsafe:` and escapes into a concrete `{return_ty}` return"
    )]
    #[diagnostic(
        code(tyc::unsafe_value_leak),
        url("https://typhon.dev/lang/diagnostics/unsafe_value_leak"),
        help("re-assert the type before returning, e.g. `let typed: {return_ty} = {name}` outside the unsafe block, or annotate the assignment inside `unsafe:` with `let {name}: {return_ty} = …` so the compiler can verify the cross")
    )]
    UnsafeValueLeak {
        name: String,
        return_ty: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("unsafe value crosses into safe-typed return")]
        span: SourceSpan,
    },

    /// `tyc check --stubs` found a mismatch between a `.dty` stub and its
    /// implementation module.
    #[error("{message}")]
    #[diagnostic(
        code(tyc::stub_mismatch),
        url("https://typhon.dev/lang/diagnostics/stub_mismatch"),
        help("synchronise the stub with the implementation, or hide private names with a leading underscore")
    )]
    StubMismatch {
        message: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("stub mismatch reported here")]
        span: SourceSpan,
    },

    /// A function decorated `@pure` violates one of the six purity conditions.
    #[error("`@pure` function '{name}' is not pure: {reason}")]
    #[diagnostic(
        code(tyc::impure_pure_fn),
        url("https://typhon.dev/lang/diagnostics/impure_pure_fn"),
        help("pure functions must be sync, take hashable args, perform no I/O, no entropy/clocks, no mutable module state, and not raise — return Result[T, E] for failure")
    )]
    ImpurePureFn {
        name: String,
        reason: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("declared `@pure` here")]
        span: SourceSpan,
    },

    /// `isinstance(x, Interface)` was used without an opt-in.
    #[error("`isinstance(x, {interface})` is rejected: structural interfaces only validate attribute presence at runtime")]
    #[diagnostic(
        code(tyc::interface_isinstance),
        url("https://typhon.dev/lang/diagnostics/interface_isinstance"),
        help("opt in by decorating the interface with `@runtime_checkable` (attribute-only check) or rely on static structural typing instead")
    )]
    InterfaceIsinstance {
        interface: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("runtime check against interface")]
        span: SourceSpan,
    },

    /// A value of type `T` doesn't structurally conform to an `interface Iface`.
    #[error("`{actual}` does not structurally conform to interface `{interface}`: missing or incompatible member(s) {missing}")]
    #[diagnostic(
        code(tyc::interface_not_conforming),
        url("https://typhon.dev/lang/diagnostics/interface_not_conforming"),
        help(
            "add the missing member(s) to `{actual}` with matching parameter types and return type"
        )
    )]
    InterfaceNotConforming {
        interface: String,
        actual: String,
        missing: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("not a `{interface}`")]
        span: SourceSpan,
    },

    /// A type argument at a call site doesn't satisfy the TypeVar's declared bound.
    #[error("type argument `{actual}` for `{typevar}` does not satisfy bound `{bound}`")]
    #[diagnostic(
        code(tyc::typevar_bound),
        url("https://typhon.dev/lang/diagnostics/typevar_bound"),
        help("pass a value whose type is a subtype of `{bound}`")
    )]
    TypeVarBoundViolation {
        typevar: String,
        actual: String,
        bound: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("inferred `{actual}` for `{typevar}`, which must satisfy `{bound}`")]
        span: SourceSpan,
    },

    /// A higher-kinded type constructor was applied ill-kindedly at a call
    /// site — either to the wrong number of type arguments, or bound to two
    /// different concrete constructors within the same call. The human-
    /// readable `message` and `help` are composed by the type checker
    /// (which knows the constructor name and arities); this variant just
    /// carries them through with a stable diagnostic code so the unifier
    /// reports the kind error instead of silently producing `Unknown`.
    #[error("{message}")]
    #[diagnostic(
        code(tyc::kind_mismatch),
        url("https://typhon.dev/lang/diagnostics/kind_mismatch"),
        help("{help}")
    )]
    KindMismatch {
        message: String,
        help: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("ill-kinded type-constructor application")]
        span: SourceSpan,
    },

    /// An attribute is accessed on a value whose type doesn't expose it.
    #[error("attribute `{attr}` is not defined on `{recv_type}`")]
    #[diagnostic(
        code(tyc::attribute_not_found),
        url("https://typhon.dev/lang/diagnostics/attribute_not_found"),
        help("check the definition of `{recv_type}` for its available attributes")
    )]
    AttributeNotFound {
        attr: String,
        recv_type: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("no attribute `{attr}` on `{recv_type}`")]
        span: SourceSpan,
    },

    /// A method is defined inside a `class NAME:` body instead of a
    /// matching `impl NAME:` block. Today the type checker still accepts
    /// the method (Python allows it), so this is surfaced as a warning to
    /// nudge users toward the recommended `impl` form without breaking
    /// existing code. Promotion to error is a separate v0.2 decision.
    #[error(
        "method `{method}` defined inside `class {class}:` body — methods live in `impl {class}:`"
    )]
    #[diagnostic(
        code(tyc::method_in_class_body),
        url("https://typhon.dev/lang/diagnostics/method_in_class_body"),
        help("move the method into an `impl {class}:` block at the same scope (multiple `impl` blocks for one class are merged at desugar)")
    )]
    MethodInClassBody {
        class: String,
        method: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("`def` here belongs in `impl {class}:`")]
        span: SourceSpan,
    },

    /// A `class NAME:` body contains only annotated assignments with
    /// defaults (`NAME: T = literal`) and no methods or per-instance
    /// fields, so it reads like a namespace of constants. The class will
    /// emit as `@dataclass(slots=True)`, which turns each name into a
    /// slot descriptor: `Klass.NAME` at runtime returns the descriptor
    /// rather than the literal. Surfaced as a warning so the existing
    /// pattern keeps building; the recommended fix is annotating each
    /// field as `ClassVar[T]` (which `@dataclass` excludes from slots).
    /// Promotion to error is a v0.2 follow-up.
    #[error(
        "class `{class}` has only defaulted fields — `{class}.{field}` will be a slot descriptor at runtime, not `{field_value_hint}`"
    )]
    #[diagnostic(
        code(tyc::class_attr_shadows_slot),
        url("https://typhon.dev/lang/diagnostics/class_attr_shadows_slot"),
        help(
            "annotate each field as `ClassVar[T]` (from `typing`) so \
             `@dataclass(slots=True)` excludes them from `__slots__`, OR \
             — if this class is a nullary variant of a sealed union — drop \
             the placeholder field entirely and use `class {class} frozen: \
             pass` (R2-2)"
        )
    )]
    ClassAttrShadowsSlot {
        class: String,
        field: String,
        field_value_hint: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("class `{class}` reads like a namespace of constants")]
        span: SourceSpan,
    },

    /// A function parameter or return type is missing its annotation. Rule 1
    /// of the Typhon language: every parameter and return type is annotated.
    /// Defaults on (`[strictness] no-implicit-any = true`) — turning it off
    /// is supported but almost never what you want.
    #[error("{what} on `{function}` is missing a type annotation")]
    #[diagnostic(
        code(tyc::missing_annotation),
        url("https://typhon.dev/lang/diagnostics/missing_annotation"),
        help("Typhon's Rule 1: annotate every parameter and return type. For a function that returns nothing, write `-> None`.")
    )]
    MissingAnnotation {
        function: String,
        what: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("annotation required here")]
        span: SourceSpan,
    },

    /// A local assignment inside a function body did not declare `let`
    /// or `mut`. Rule 2 of the Typhon language: local bindings always
    /// carry a binding-kind keyword so readers can tell at a glance
    /// whether a name is rebound later. Module-level bindings still
    /// default to `let`, so this only fires at function/method scope.
    #[error("local binding '{name}' is missing `let` or `mut`")]
    #[diagnostic(
        code(tyc::missing_binding_kind),
        url("https://typhon.dev/lang/diagnostics/missing_binding_kind"),
        help("write `let {name} = …` for an immutable binding, or `mut {name} = …` if you intend to rebind it later")
    )]
    MissingBindingKind {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("declare with `let` or `mut`")]
        span: SourceSpan,
    },

    /// Two or more adjacent `await CALLEE(...)` statements look
    /// independent enough to fold into an `asyncio.TaskGroup` block,
    /// but at least one callee is a same-module `async def` that
    /// lacks the `@gatherable` decorator. The auto-gather pass only
    /// rewrites runs where every callee carries `@gatherable` (the
    /// decorator is the user's attestation that the function is safe
    /// to run concurrently with peers); without it, the awaits stay
    /// sequential silently. This advice-level diagnostic surfaces the
    /// missed opportunity so users can decide whether to decorate.
    #[error(
        "two or more adjacent awaits look gather-able but `{missing}` is not decorated `@gatherable`"
    )]
    #[diagnostic(
        severity(Advice),
        code(tyc::auto_gather_missed),
        url("https://typhon.dev/lang/diagnostics/auto_gather_missed"),
        help("decorate `{missing}` (and any other same-module async callees in the run) with `@gatherable` to fold the awaits into an `asyncio.TaskGroup` automatically")
    )]
    AutoGatherMissed {
        missing: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("auto-gather skipped this run")]
        span: SourceSpan,
    },

    /// Two or more adjacent `NAME = await CALL(...)` statements inside an
    /// `async def` are independent by data flow (no later await consumes
    /// an earlier binding), so they run sequentially when they could run
    /// concurrently. Unlike [`TycError::AutoGatherMissed`], this is
    /// callee-agnostic — it fires for awaited method calls on imported
    /// clients (`await client.get_user(id)` then
    /// `await client.get_posts(id)`), the most common real
    /// missed-concurrency shape — and suggests the explicit `gather:`
    /// block, which works for any awaitable without a `@gatherable`
    /// decorator. Advice-level: concurrency is a behaviour change the
    /// author opts into (data-flow independence doesn't rule out ordering
    /// side effects), so this never rewrites and never blocks a build.
    #[error("{count} adjacent awaits run sequentially but look independent")]
    #[diagnostic(
        severity(Advice),
        code(tyc::gather_opportunity),
        url("https://typhon.dev/lang/diagnostics/gather_opportunity"),
        help("if these awaits have no ordering dependency, wrap them in a `gather:` block so they run concurrently in an `asyncio.TaskGroup` instead of one after another")
    )]
    GatherOpportunity {
        count: usize,
        #[source_code]
        src: NamedSource<String>,
        #[label("these {count} awaits could run concurrently")]
        span: SourceSpan,
    },

    /// A top-level `def main()` is defined but is never called from
    /// the module. Common newcomer mistake — the script's `main`
    /// function never runs, leaving the build apparently successful
    /// but producing no output. Surfaced as advice (not an error) so
    /// existing library-style modules with a `main` symbol that's
    /// imported elsewhere aren't broken.
    #[error("`main` is defined but never called in this module")]
    #[diagnostic(
        severity(Advice),
        code(tyc::main_not_called),
        url("https://typhon.dev/lang/diagnostics/main_not_called"),
        help("Add `if __name__ == \"__main__\":\\n    main()` at the end of the module (the standard Python script-entry pattern) so the script runs when invoked directly.")
    )]
    MainNotCalled {
        #[source_code]
        src: NamedSource<String>,
        #[label("`main()` is never invoked")]
        span: SourceSpan,
    },

    /// An import references a module that isn't in the Python stdlib,
    /// not the project tree, not the bundled `typhon_runtime`, and not
    /// listed in `typhon.toml`'s dependencies. The build would later
    /// fail at import time with `ModuleNotFoundError`; surface the
    /// typo / missing dep at check time instead. FINDINGS #79.
    #[error("module `{module}` is not in the stdlib, the project, or `typhon.toml` dependencies")]
    #[diagnostic(
        code(tyc::unknown_module),
        url("https://typhon.dev/lang/diagnostics/unknown_module"),
        help("Either fix the import name, add `{module}` to the `[dependencies]` table in `typhon.toml` (then run `tyc sync`), or create a sibling `.ty` file with the right name.")
    )]
    UnknownModule {
        module: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("not resolvable at check time")]
        span: SourceSpan,
    },

    /// A `class NAME:` statement re-uses a name that has already been
    /// declared at the same scope. Python silently lets the second
    /// definition shadow the first; Typhon flags it so the user can
    /// either rename one of them or merge the body into `impl NAME:`.
    /// FINDINGS #77.
    #[error("class `{name}` is declared more than once in this module")]
    #[diagnostic(
        code(tyc::duplicate_class),
        url("https://typhon.dev/lang/diagnostics/duplicate_class"),
        help("rename one of the declarations, or merge the second body into `impl {name}:` / `extend {name}:`")
    )]
    DuplicateClass {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("redeclaration here")]
        span: SourceSpan,
    },

    /// Two `impl` / `extend` blocks (or one of each) on the same class
    /// both define a method with the same name. The desugar pass would
    /// silently emit both `def`s in the merged class body, Python takes
    /// the last one, and one definition is lost without any warning.
    /// N8 (2026-05-22).
    #[error("method `{method}` is defined more than once on `{class_name}`")]
    #[diagnostic(
        code(tyc::duplicate_method),
        url("https://typhon.dev/lang/diagnostics/duplicate_method"),
        help(
            "rename one of the methods, or merge the body of the second \
              `impl {class_name}:` / `extend {class_name}:` block into the first"
        )
    )]
    DuplicateMethod {
        class_name: String,
        method: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("redefined here")]
        span: SourceSpan,
    },

    /// An `impl NAME:` block targets a class that does not exist in the
    /// current module. The methods otherwise lower into a free-floating
    /// `__typhon_impl_NAME` pseudo-class that the merge pass silently
    /// drops, producing dead code. FINDINGS #78.
    #[error("`impl {name}:` targets an unknown class")]
    #[diagnostic(
        code(tyc::impl_unknown_class),
        url("https://typhon.dev/lang/diagnostics/impl_unknown_class"),
        help("declare `class {name}:` first, or fix the name to match an existing class in this module")
    )]
    ImplUnknownClass {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("no such class in scope")]
        span: SourceSpan,
    },

    /// A `type` alias chain forms a cycle (`type A = B; type B = A`).
    /// The runtime never crashes because Python evaluates aliases
    /// lazily, but no caller can ever resolve the type. FINDINGS #81,
    /// O4.
    ///
    /// Self-referential aliases through generic containers — the
    /// canonical recursive-JSON / AST / tree shape
    /// `type JSON = None | bool | int | str | list[JSON] | dict[str, JSON]`
    /// — are not yet supported and trigger this diagnostic for the
    /// same reason: `unwrap_alias` is bounded to eight hops and a
    /// chain that re-enters itself never resolves to a concrete type.
    /// Today's workaround is to use `dict[str, object]` /
    /// `list[object]` at the recursion boundary; full recursive-type
    /// support is tracked in `docs/findings.md`.
    #[error("type alias `{name}` is part of a cycle")]
    #[diagnostic(
        code(tyc::cyclic_type_alias),
        url("https://typhon.dev/lang/diagnostics/cyclic_type_alias"),
        help("recursive type aliases are not yet supported (including the canonical `list[Self]` / `dict[str, Self]` shape). Break the cycle by pointing at a concrete type, splitting the alias into named classes, or falling back to `object` at the recursion boundary.")
    )]
    CyclicTypeAlias {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("alias here is in a cycle")]
        span: SourceSpan,
    },

    /// A direct call to a known-blocking stdlib function (`time.sleep`,
    /// `requests.get`, `socket.recv`, `subprocess.run`, …) appears
    /// inside an `async def` body. The call halts the entire event
    /// loop until it returns, defeating the point of `async`. Wrap
    /// the call in `await asyncio.to_thread(...)` (or
    /// `loop.run_in_executor(...)`) to run it on a worker thread
    /// without blocking the loop. Default severity is **warn**;
    /// promote to error via `[strictness] blocking-in-async =
    /// "error"` in `typhon.toml`.
    #[error("`{name}(...)` blocks the event loop when called from an `async def`")]
    #[diagnostic(
        severity(Warning),
        code(tyc::blocking_in_async),
        url("https://typhon.dev/lang/diagnostics/blocking_in_async"),
        help("wrap the call: `await asyncio.to_thread({name}, ...)`")
    )]
    BlockingInAsync {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("blocking call inside `async def`")]
        span: SourceSpan,
    },

    /// A call to a known resource-returning function (`open`,
    /// `socket.socket`, `sqlite3.connect`, …) was bound to a
    /// variable without a surrounding `with` statement. Without
    /// `with`, the handle is only released when the GC collects
    /// the binding — at best non-deterministically, at worst not
    /// at all if the function raises mid-way. Default severity is
    /// **warn**; promote to error via `[strictness] require-with
    /// = "error"` in `typhon.toml` for CI enforcement.
    #[error("`{name}(...)` returns a resource that should be managed by `with`")]
    #[diagnostic(
        severity(Warning),
        code(tyc::resource_not_managed),
        url("https://typhon.dev/lang/diagnostics/resource_not_managed"),
        help("wrap the call in a `with` block: `with {name}(...) as handle: ...`")
    )]
    ResourceNotManaged {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("not consumed by a `with` statement")]
        span: SourceSpan,
    },

    /// A division-style operator (`/`, `//`, `%`) has a literal zero
    /// on the right-hand side. The runtime will raise
    /// `ZeroDivisionError` unconditionally; catching it at compile
    /// time is a free win because the expression has no other
    /// behaviour. The check is constant-fold only — runtime values
    /// that *could* be zero are not flagged.
    #[error("division by literal zero — `x {op} 0` always raises `ZeroDivisionError`")]
    #[diagnostic(
        code(tyc::div_by_zero_literal),
        url("https://typhon.dev/lang/diagnostics/div_by_zero_literal"),
        help("change the divisor to a non-zero value, or guard the expression behind an `if d != 0:` check")
    )]
    DivByZeroLiteral {
        op: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("literal zero divisor")]
        span: SourceSpan,
    },

    /// A bare value of the base type was passed where a `newtype` is
    /// expected, without going through the explicit constructor.
    /// `newtype UserId = int` makes `UserId` nominally distinct: a
    /// `UserId` flows into an `int` slot freely (the runtime values
    /// are identical), but the reverse requires `UserId(x)` so the
    /// boundary is explicit at the call site.
    ///
    /// Two emit sites:
    ///
    /// - Constructor-arg mismatch: `UserId("seven")` against
    ///   `newtype UserId = int` — argument fails to satisfy the base.
    ///   `arg_type` is the wrong-typed argument; the label reads
    ///   "type `str` is not a `int`".
    /// - Boundary-passing mismatch: `fetch_user(42)` against
    ///   `def fetch_user(uid: UserId): …` — a bare base-typed value
    ///   flowing into a newtype slot. The label reads "wrap with
    ///   `UserId(42)`".
    #[error("expected `{name}`, found bare `{arg_type}`")]
    #[diagnostic(
        code(tyc::newtype_violation),
        url("https://typhon.dev/lang/diagnostics/newtype_violation"),
        help(
            "wrap with `{name}({arg_type_short})` to satisfy the nominal newtype, \
              or change the annotation to `{base}` if the nominal type isn't needed here"
        )
    )]
    NewtypeViolation {
        name: String,
        base: String,
        arg_type: String,
        arg_type_short: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("wrap with `{name}(...)`")]
        span: SourceSpan,
    },

    /// A `freeze let X = <expr>` whose RHS would fail
    /// `typhon_runtime.freeze.deep_freeze` at startup. Deep-freezing
    /// recursively converts list → tuple, dict → MappingProxy, set →
    /// frozenset; any reachable type without an immutable equivalent
    /// (open file handles, generators, non-`frozen` dataclasses) makes
    /// the call raise `TypeError` at import time. We catch the common
    /// shape (constructing a non-frozen class) at check time so the
    /// failure surfaces in the user's editor instead of as a crash
    /// during the first `python build/main.py` invocation.
    #[error("`freeze let {name}` cannot be deep-frozen: `{kind}` is not freezable")]
    #[diagnostic(
        code(tyc::freeze_not_freezable),
        url("https://typhon.dev/lang/diagnostics/freeze_not_freezable"),
        help(
            "deep_freeze can only recurse into immutable shapes (frozen \
             classes, tuples, frozensets, mapping proxies) and primitive \
             values (int/float/str/bool/bytes/None). Mark `{kind}` as \
             `class {kind} frozen:` to make it freezable, or move this \
             binding off `freeze let` so the value stays a plain mutable \
             reference."
        )
    )]
    FreezeNotFreezable {
        name: String,
        kind: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("non-freezable value here")]
        span: SourceSpan,
    },

    /// A `newtype Foo = <expr>` whose RHS isn't a recognised type
    /// expression (it's a string literal, number, function call, …).
    /// `newtype` is a nominal alias of an existing type, so the RHS
    /// must name a type — not a value. FINDINGS v0.7.1 #14.
    #[error("`newtype {name}` base must be a type, not `{base_kind}`")]
    #[diagnostic(
        code(tyc::newtype_invalid_base),
        url("https://typhon.dev/lang/diagnostics/newtype_invalid_base"),
        help(
            "the right-hand side of `newtype {name} = …` must be a type \
             (e.g. `int`, `str`, `list[int]`, `MyClass`); replace the \
             literal/expression with an actual type name"
        )
    )]
    NewtypeInvalidBase {
        name: String,
        base_kind: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("not a valid type")]
        span: SourceSpan,
    },

    /// A `let NAME: T` binding declared without an initialiser is
    /// read on a control-flow path where no preceding statement
    /// assigned it. R3-8's definite-assignment pass surfaces the
    /// use-before-init bug before runtime — without the check, a
    /// declare-then-assign-in-arms idiom that forgets the `Err` arm
    /// would silently return whatever Python's `NameError` produces.
    #[error("`{name}` may be used before it is initialised")]
    #[diagnostic(
        code(tyc::use_of_uninitialised),
        url("https://typhon.dev/lang/diagnostics/use_of_uninitialised"),
        help(
            "ensure every branch that reaches this point assigns to `{name}` \
             first, or initialise it at the declaration site (`let {name}: T = …`)"
        )
    )]
    UseOfUninitialised {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("`{name}` is not assigned on every path that reaches here")]
        span: SourceSpan,
        #[label("declared here without an initialiser")]
        decl_span: SourceSpan,
    },

    /// Two sibling modules aggregated by a `pub *` re-export in
    /// `__init__.ty` both expose the same name. The synthesised
    /// `from .a import X` + `from .b import X` would silently shadow
    /// one with the other depending on import order — almost certainly
    /// a refactoring slip, not the intended behaviour. The diagnostic
    /// names both sibling files so the user can decide which one to
    /// rename (or drop from the re-export).
    #[error("`pub *` collision: `{name}` is exported by both `{first}` and `{second}`")]
    #[diagnostic(
        code(tyc::pub_name_collision),
        url("https://typhon.dev/lang/diagnostics/pub_name_collision"),
        help(
            "rename one of the conflicting `pub` declarations, or replace `pub *` \
             with an explicit `from .module import name` list that resolves the \
             ambiguity"
        )
    )]
    PubNameCollision {
        name: String,
        first: String,
        second: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("`pub *` re-export here")]
        span: SourceSpan,
    },

    /// A `pub *` statement appears in a regular `.ty` module rather
    /// than the package's `__init__.ty`. The wildcard re-export only
    /// has meaning at a package boundary — anywhere else it's a no-op
    /// with confusing intent. Surfaced as advice so the user can move
    /// it (or drop it) without the build failing.
    #[error("`pub *` is only meaningful inside `__init__.ty`")]
    #[diagnostic(
        severity(Advice),
        code(tyc::pub_star_outside_init),
        url("https://typhon.dev/lang/diagnostics/pub_star_outside_init"),
        help(
            "move the `pub *` statement to the package's `__init__.ty`, or remove \
             it if the module isn't acting as a package facade"
        )
    )]
    PubStarOutsideInit {
        #[source_code]
        src: NamedSource<String>,
        #[label("`pub *` outside `__init__.ty`")]
        span: SourceSpan,
    },

    /// An `async def` function body never `await`s. The function still
    /// returns a coroutine, but the `async` keyword is functionally a
    /// no-op — usually a sign of a half-finished refactor or a missing
    /// `await` on an internal call. FINDINGS #83.
    #[error("`async def {name}` has no `await` expression")]
    #[diagnostic(
        severity(Warning),
        code(tyc::async_without_await),
        url("https://typhon.dev/lang/diagnostics/async_without_await"),
        help("drop `async` if the function is synchronous, or `await` the call(s) that should run concurrently")
    )]
    AsyncWithoutAwait {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("no `await` inside this body")]
        span: SourceSpan,
    },

    /// `typhon.toml` declared a value that isn't in the allowed enumeration for
    /// that key (e.g. `[emit] class-default = "plain"`). Rejected at config-
    /// load time so the build never silently does the wrong thing.
    #[error("invalid value `{value}` for `{key}` in `{path}`: expected one of {allowed}")]
    #[diagnostic(
        code(tyc::invalid_config_value),
        url("https://typhon.dev/lang/diagnostics/invalid_config_value"),
        help("Pick one of the listed values in your typhon.toml.")
    )]
    InvalidConfigValue {
        key: String,
        value: String,
        allowed: String,
        path: String,
    },

    /// A `from .NAME import …` references a sibling `.py` file that lives
    /// outside the project's `src/` tree, so `tyc build` won't copy it into
    /// the output. The emitted Python would crash at import time.
    #[error("relative import `{import_path}` resolves outside `src/` and will not be copied to the build output")]
    #[diagnostic(
        severity(Warning),
        code(tyc::orphan_py_import),
        url("https://typhon.dev/lang/diagnostics/orphan_py_import"),
        help("Move the `.py` file under `src/`, or rewrite the import as a project-relative absolute import.")
    )]
    OrphanPyImport {
        import_path: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("relative import resolves outside src/")]
        span: SourceSpan,
    },

    /// Typhon's type checker rejects an expression that CPython would
    /// happily evaluate. Used as a regression signal during the Phase 5
    /// Python-semantic-alignment audit. Surfaces as a warning so existing
    /// builds keep working while the underlying rule is corrected.
    #[error("Typhon rejected `{expression}` but CPython accepts it: {detail}")]
    #[diagnostic(
        severity(Warning),
        code(tyc::python_semantic_drift),
        url("https://typhon.dev/lang/diagnostics/python_semantic_drift"),
        help("Open an issue with the offending snippet so the type-checker rule can be relaxed.")
    )]
    PythonSemanticDrift {
        expression: String,
        detail: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("Typhon and CPython disagree here")]
        span: SourceSpan,
    },

    /// A `comptime let` binding whose name matches a secret-suffix heuristic
    /// (`*KEY`, `*TOKEN`, `*PASSWORD`, `*SECRET`, `*PASS`, `*PWD`) inlines its
    /// env value at build time, so the emitted Python contains the raw secret
    /// as a string literal. Read the env var at runtime instead.
    #[error("comptime binding `{name}` inlines a secret-shaped value at build time")]
    #[diagnostic(
        severity(Warning),
        code(tyc::contains_secret_literal),
        url("https://typhon.dev/lang/diagnostics/contains_secret_literal"),
        help("Replace `comptime let {name} = env(...)` with a runtime lookup such as `os.environ[\"{env_key}\"]`.")
    )]
    ContainsSecretLiteral { name: String, env_key: String },

    /// A plain `let` / module-level binding whose name matches the
    /// secret-suffix heuristic (`*KEY`, `*TOKEN`, `*PASSWORD`, `*SECRET`,
    /// `*PWD`, `*API_KEY`) is initialised from a raw string literal
    /// instead of an environment lookup. Committing such a literal hard-
    /// codes a credential into the source tree.
    #[error("binding `{name}` looks like a credential but is initialised from a string literal")]
    #[diagnostic(
        severity(Warning),
        code(tyc::contains_secret_literal),
        url("https://typhon.dev/lang/diagnostics/contains_secret_literal"),
        help("Read the value at runtime via `os.environ[\"{name}\"]` or `os.getenv(\"{name}\")` instead of hard-coding a literal.")
    )]
    SecretLiteralInline {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("hard-coded secret-shaped value")]
        span: SourceSpan,
    },

    /// A `let` or `mut` binding whose RHS is an empty collection literal
    /// (`[]`, `{}`, `set()`) has no type annotation, so its element type
    /// defaults to `Unknown` (effectively `Any`) and silences any later
    /// element-type mismatch.
    #[error("empty {literal} without a type annotation defaults to `Unknown` and disables element-type checking")]
    #[diagnostic(
        severity(Warning),
        code(tyc::empty_collection_no_annotation),
        url("https://typhon.dev/lang/diagnostics/empty_collection_no_annotation"),
        help("Add an explicit annotation, e.g. `let {name}: list[int] = []`, so the element type is checked.")
    )]
    EmptyCollectionNoAnnotation {
        name: String,
        /// Human-readable name of the literal: "list literal `[]`",
        /// "dict literal `{}`", or "set literal `set()`". Used in the
        /// error message so users immediately recognise the offending form.
        literal: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("annotate this binding to fix")]
        span: SourceSpan,
    },

    /// A type annotation references a deprecated `typing.<Name>` alias by
    /// name (`List[int]`, `Dict[str, int]`, `Optional[int]`,
    /// `Union[int, str]`, …) even though the import is rejected. The
    /// reference is silently accepted as a forward-reference name; this
    /// warning surfaces the inconsistency so users migrate to the
    /// built-in lowercase forms (`list`, `dict`, `T?`, `A | B`).
    #[error("`{name}` in an annotation is the deprecated `typing.{name}` alias")]
    #[diagnostic(
        severity(Warning),
        code(tyc::typing_alias_in_annotation),
        url("https://typhon.dev/lang/diagnostics/typing_alias_in_annotation"),
        help("Use `{suggestion}` instead — the deprecated `typing.{name}` alias is rejected on import and should not be used in annotations either.")
    )]
    TypingAliasInAnnotation {
        name: String,
        suggestion: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("prefer `{suggestion}` here")]
        span: SourceSpan,
    },

    /// A function parameter's default value is a mutable literal
    /// (`def f(xs: list[int] = [])`). Python evaluates the default ONCE at
    /// definition time, so every call that omits the argument shares the
    /// same object — the classic shared-state footgun. Typhon already
    /// rewrites the identical pattern on class fields to a per-instance
    /// factory; function parameters get this warning instead (rewriting
    /// them would change the signature visible to runtime introspection).
    #[error("mutable default for parameter `{name}` is shared across calls")]
    #[diagnostic(
        severity(Warning),
        code(tyc::mutable_default_param),
        url("https://typhon.dev/lang/diagnostics/mutable_default_param"),
        help("Python evaluates the {literal} once, at `def` time — every call that omits `{name}` mutates the same object. Use `{name}: T? = None` and create the value inside the body (`if {name} is None: ...`), or pass the argument explicitly.")
    )]
    MutableDefaultParam {
        name: String,
        /// Human-readable description of the literal: "list literal `[]`",
        /// "dict literal `{{}}`", or "set constructor `set()`".
        literal: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("this default is created once and shared")]
        span: SourceSpan,
    },

    /// An `is` / `is not` comparison against a literal (`s is "hello"`,
    /// `n is 5`). `is` compares object *identity*, not value — whether two
    /// equal literals are the same object is an interpreter implementation
    /// detail (small-int caching, string interning), so the result is
    /// arbitrary. CPython itself emits a SyntaxWarning for this shape.
    #[error("`is` compares identity, not value — comparing against a literal is unreliable")]
    #[diagnostic(
        severity(Warning),
        code(tyc::is_literal_comparison),
        url("https://typhon.dev/lang/diagnostics/is_literal_comparison"),
        help(
            "Use `==` / `!=` for value comparison. Reserve `is` for `None` and sentinel objects."
        )
    )]
    IsLiteralComparison {
        #[source_code]
        src: NamedSource<String>,
        #[label("literal operand — use `==` instead")]
        span: SourceSpan,
    },

    /// A subclass method overrides a base-class method with an
    /// incompatible signature (different arity, a parameter type narrower
    /// than the base's, or a return type not assignable to the base's).
    /// Calls dispatched through the base type can then break at runtime —
    /// the Liskov substitution principle violation mypy / pyright flag.
    #[error("`{class_name}.{method}` overrides `{base}.{method}` incompatibly: {reason}")]
    #[diagnostic(
        severity(Warning),
        code(tyc::incompatible_override),
        url("https://typhon.dev/lang/diagnostics/incompatible_override"),
        help("Code holding a `{base}` may call `{method}` with the base signature and dispatch to this override at runtime. Match the base signature (parameters may widen, returns may narrow), or rename the method.")
    )]
    IncompatibleOverride {
        class_name: String,
        method: String,
        base: String,
        reason: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("overriding class declared here")]
        span: SourceSpan,
    },

    /// A closure (lambda or nested `def`) created inside a loop references
    /// the loop variable. Python closures capture *variables*, not values
    /// — every closure sees the final iteration's value once the loop
    /// ends (`[lambda: i for i in range(3)]` → all return 2).
    #[error("closure captures loop variable `{name}` by reference, not value")]
    #[diagnostic(
        severity(Warning),
        code(tyc::loop_closure_capture),
        url("https://typhon.dev/lang/diagnostics/loop_closure_capture"),
        help("Each closure shares the single `{name}` binding and will observe its value at *call* time — after the loop, that's the last iteration. Bind the current value per closure with a default (`lambda {name}={name}: ...`) or build values eagerly instead of deferring.")
    )]
    LoopClosureCapture {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("captured by reference here")]
        span: SourceSpan,
    },
}

impl TycError {
    /// Construct a [`TycError::Io`] from a [`std::io::Error`].
    pub fn io(path: impl Into<String>, cause: &dyn std::error::Error) -> Self {
        Self::Io {
            path: path.into(),
            cause: cause.to_string(),
        }
    }

    /// Construct a [`TycError::Generic`] from any string-like message.
    pub fn generic(message: impl Into<String>) -> Self {
        Self::Generic {
            message: message.into(),
        }
    }

    /// Construct a [`TycError::Parse`] from a parser error.
    ///
    /// The raw `message` from the Python parser is fed verbatim into
    /// the label, but [`parse_error_hint`] inspects it (plus the
    /// surrounding source) to recover a Typhon-specific suggestion
    /// for known shapes: multi-line `|>` chains without parens
    /// (FINDINGS #34) and `freeze let` inside a function
    /// (FINDINGS #35). Anything unrecognised falls through with
    /// `suggestion = None` so miette omits the help block.
    pub fn parse(
        path: impl Into<String>,
        source: impl Into<String>,
        message: impl Into<String>,
        offset: usize,
    ) -> Self {
        let path = path.into();
        let source = source.into();
        let message = message.into();
        let suggestion = parse_error_hint(&message, &source, offset);
        let span = SourceSpan::new(SourceOffset::from(offset), 0usize);
        Self::Parse {
            src: NamedSource::new(path.clone(), source),
            path,
            message,
            suggestion,
            span,
        }
    }

    /// Construct an [`TycError::UnknownName`] diagnostic.
    pub fn unknown_name(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::UnknownName {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct an [`TycError::SelfOutsideImpl`] diagnostic.
    pub fn self_outside_impl(
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::SelfOutsideImpl {
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::TypeVarImportRejected`] diagnostic.
    pub fn typevar_import_rejected(
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::TypeVarImportRejected {
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::TypingAliasDeprecated`] diagnostic for a
    /// capitalised `typing.<Name>` alias of a lowercase built-in.
    pub fn typing_alias_deprecated(
        name: impl Into<String>,
        lower: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::TypingAliasDeprecated {
            name: name.into(),
            lower: lower.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::UnknownKwarg`] diagnostic. `suggestion`
    /// should be a complete help message — typically either "did you
    /// mean `<candidate>`?" (when a close match exists) or a listing
    /// of every accepted parameter name.
    pub fn unknown_kwarg(
        fn_name: impl Into<String>,
        kwarg: impl Into<String>,
        suggestion: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::UnknownKwarg {
            fn_name: fn_name.into(),
            kwarg: kwarg.into(),
            suggestion: suggestion.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::MissingReturn`] diagnostic.
    pub fn missing_return(
        fn_name: impl Into<String>,
        ret_type: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::MissingReturn {
            fn_name: fn_name.into(),
            ret_type: ret_type.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::MissingInitialiser`] diagnostic.
    pub fn missing_initialiser(
        keyword: impl Into<String>,
        name: impl Into<String>,
        annotation: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::MissingInitialiser {
            keyword: keyword.into(),
            name: name.into(),
            annotation: annotation.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::ImplicitAny`] diagnostic.
    pub fn implicit_any(
        kind: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::ImplicitAny {
            kind: kind.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::NoBlockShadow`] diagnostic.
    #[allow(clippy::too_many_arguments)]
    pub fn no_block_shadow(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        decl_offset: usize,
        decl_length: usize,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::NoBlockShadow {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            decl_span: SourceSpan::new(SourceOffset::from(decl_offset), decl_length),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::TypeMismatch`] diagnostic.
    ///
    /// The help text is computed at construction time. For the common
    /// invariant-collection case (`list[Sub]` flowing into `list[Super]`,
    /// likewise `dict`/`set`) the suggestion points at the covariant
    /// read-only view (`Sequence[Super]` / `Mapping[K, V]`) instead of
    /// the unhelpful "widen to `list[Super] | list[Sub]`" union which is
    /// never what the user wants. FINDINGS #37.
    pub fn type_mismatch(
        expected: impl Into<String>,
        actual: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        let expected: String = expected.into();
        let actual: String = actual.into();
        let suggestion = type_mismatch_help(&expected, &actual);
        Self::TypeMismatch {
            expected,
            actual,
            suggestion,
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::TypeReassignMismatch`] diagnostic that
    /// points at both the offending assignment and the original
    /// declaration site, with help text explaining what `mut` permits.
    #[allow(clippy::too_many_arguments)]
    pub fn type_reassign_mismatch(
        name: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
        decl_offset: usize,
        decl_length: usize,
    ) -> Self {
        Self::TypeReassignMismatch {
            name: name.into(),
            expected: expected.into(),
            actual: actual.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
            decl_span: SourceSpan::new(SourceOffset::from(decl_offset), decl_length.max(1)),
        }
    }

    /// Construct a [`TycError::OperatorTypeMismatch`] diagnostic.
    pub fn operator_type_mismatch(
        op: impl Into<String>,
        lhs: impl Into<String>,
        rhs: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::OperatorTypeMismatch {
            op: op.into(),
            lhs: lhs.into(),
            rhs: rhs.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::TupleIndexOutOfRange`] diagnostic.
    pub fn tuple_index_out_of_range(
        arity: usize,
        index: i64,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::TupleIndexOutOfRange {
            arity,
            index,
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::NullableUse`] diagnostic.
    pub fn nullable_use(
        name: impl Into<String>,
        expected: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::NullableUse {
            name: name.into(),
            expected: expected.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::ResultErrorMismatch`] diagnostic.
    pub fn result_error_mismatch(
        expected_err: impl Into<String>,
        actual_err: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::ResultErrorMismatch {
            expected_err: expected_err.into(),
            actual_err: actual_err.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::WrongArgCount`] diagnostic.
    ///
    /// When `expected == actual` the rendered message reads "expected N,
    /// got N", which is self-contradictory at the headline level. The
    /// upstream checker produces this shape when the call site passed
    /// every argument positionally but some of the parameters are
    /// keyword-only — the count comparison sums positional + kw-only,
    /// so it passes if you ignore the calling-convention mismatch.
    /// In that case the constructor populates the `#[help]` field with
    /// "pass them by name" so the user gets actionable advice. FINDINGS #36.
    pub fn wrong_arg_count(
        name: impl Into<String>,
        expected: usize,
        actual: usize,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        let name_s: String = name.into();
        let suggestion = if expected == actual {
            Some(format!(
                "`{name_s}` declares some parameters as keyword-only; \
                 pass them by name (e.g. `{name_s}(a=1, b=2)`)"
            ))
        } else {
            None
        };
        Self::WrongArgCount {
            name: name_s,
            expected,
            actual,
            suggestion,
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::MissingArgument`] diagnostic. `missing`
    /// must be non-empty — callers that can't identify a specific
    /// missing name should use [`TycError::wrong_arg_count`] instead.
    pub fn missing_argument(
        name: impl Into<String>,
        missing: Vec<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        let plural = if missing.len() == 1 { "" } else { "s" };
        let missing_list = missing
            .iter()
            .map(|n| format!("`{n}`"))
            .collect::<Vec<_>>()
            .join(", ");
        Self::MissingArgument {
            name: name.into(),
            missing,
            missing_list,
            plural,
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::NotCallable`] diagnostic.
    pub fn not_callable(
        typ: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::NotCallable {
            typ: typ.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::GeneratorReturnType`] diagnostic.
    pub fn generator_return_type(
        fn_name: impl Into<String>,
        returned: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::GeneratorReturnType {
            fn_name: fn_name.into(),
            returned: returned.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::ManualInit`] diagnostic.
    pub fn manual_init(
        class_name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::ManualInit {
            class_name: class_name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::FieldDefaultOrdering`] diagnostic (R3-11).
    pub fn field_default_ordering(
        class_name: impl Into<String>,
        non_default: impl Into<String>,
        prior_default: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::FieldDefaultOrdering {
            class_name: class_name.into(),
            non_default: non_default.into(),
            prior_default: prior_default.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::MissingAwait`] diagnostic.
    pub fn missing_await(
        callee: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::MissingAwait {
            callee: callee.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::NonExhaustiveMatch`] diagnostic.
    pub fn non_exhaustive_match(
        union_name: impl Into<String>,
        missing: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::NonExhaustiveMatch {
            union_name: union_name.into(),
            missing: missing.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::Comptime`] diagnostic.
    pub fn comptime(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Comptime {
            name: name.into(),
            message: message.into(),
        }
    }

    /// Construct a [`TycError::InvalidQuestionOp`] diagnostic.
    pub fn invalid_question_op(
        message: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::InvalidQuestionOp {
            message: message.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::UnusedImport`] diagnostic.
    pub fn unused_import(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::UnusedImport {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::LazyUsage`] diagnostic.
    pub fn lazy_usage(
        message: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::LazyUsage {
            message: message.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::StubMismatch`] diagnostic.
    pub fn stub_mismatch(
        message: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::StubMismatch {
            message: message.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::StdlibModuleShadow`] warning. R2-4.
    pub fn stdlib_module_shadow(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::StdlibModuleShadow {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::ExtendBuiltin`] diagnostic.
    pub fn extend_builtin(
        message: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::ExtendBuiltin {
            message: message.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::UnsafeValueLeak`] diagnostic. O14 / FINDINGS #107.
    pub fn unsafe_value_leak(
        name: impl Into<String>,
        return_ty: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::UnsafeValueLeak {
            name: name.into(),
            return_ty: return_ty.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::ImpurePureFn`] diagnostic.
    pub fn impure_pure_fn(
        name: impl Into<String>,
        reason: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::ImpurePureFn {
            name: name.into(),
            reason: reason.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::InterfaceIsinstance`] diagnostic.
    pub fn interface_isinstance(
        interface: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::InterfaceIsinstance {
            interface: interface.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::InterfaceNotConforming`] diagnostic.
    pub fn interface_not_conforming(
        interface: impl Into<String>,
        actual: impl Into<String>,
        missing: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::InterfaceNotConforming {
            interface: interface.into(),
            actual: actual.into(),
            missing: missing.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::TypeVarBoundViolation`] diagnostic.
    pub fn typevar_bound_violation(
        typevar: impl Into<String>,
        actual: impl Into<String>,
        bound: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::TypeVarBoundViolation {
            typevar: typevar.into(),
            actual: actual.into(),
            bound: bound.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::KindMismatch`] diagnostic. `message` and
    /// `help` are fully composed by the caller (the type checker), which
    /// has the constructor name and arities in hand.
    pub fn kind_mismatch(
        message: impl Into<String>,
        help: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::KindMismatch {
            message: message.into(),
            help: help.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::AttributeNotFound`] diagnostic.
    pub fn attribute_not_found(
        attr: impl Into<String>,
        recv_type: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::AttributeNotFound {
            attr: attr.into(),
            recv_type: recv_type.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::MethodInClassBody`] diagnostic.
    pub fn method_in_class_body(
        class: impl Into<String>,
        method: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::MethodInClassBody {
            class: class.into(),
            method: method.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::ClassAttrShadowsSlot`] diagnostic.
    pub fn class_attr_shadows_slot(
        class: impl Into<String>,
        field: impl Into<String>,
        field_value_hint: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::ClassAttrShadowsSlot {
            class: class.into(),
            field: field.into(),
            field_value_hint: field_value_hint.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::MissingAnnotation`] diagnostic. `what` is the
    /// human-readable description of what's unannotated (e.g. `parameter
    /// `name`` or `return type`).
    pub fn missing_annotation(
        function: impl Into<String>,
        what: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::MissingAnnotation {
            function: function.into(),
            what: what.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::MissingBindingKind`] diagnostic.
    pub fn missing_binding_kind(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::MissingBindingKind {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::AutoGatherMissed`] advice diagnostic.
    pub fn auto_gather_missed(
        missing: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::AutoGatherMissed {
            missing: missing.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::GatherOpportunity`] advice diagnostic.
    pub fn gather_opportunity(
        count: usize,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::GatherOpportunity {
            count,
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::MainNotCalled`] advice diagnostic.
    pub fn main_not_called(
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::MainNotCalled {
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::UnknownModule`] diagnostic.
    pub fn unknown_module(
        module: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::UnknownModule {
            module: module.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::FrozenAssign`] diagnostic.
    pub fn frozen_assign(
        class: impl Into<String>,
        field: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::FrozenAssign {
            class: class.into(),
            field: field.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::MissingFieldInit`] diagnostic. `missing`
    /// is the comma-separated list of unassigned required field
    /// names; `first_missing` is the first one (also surfaced in the
    /// help text so the user sees an actionable fix).
    pub fn missing_field_init(
        class: impl Into<String>,
        missing: impl Into<String>,
        first_missing: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::MissingFieldInit {
            class: class.into(),
            missing: missing.into(),
            first_missing: first_missing.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::DuplicateClass`] diagnostic.
    pub fn duplicate_class(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::DuplicateClass {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::DuplicateMethod`] diagnostic.
    pub fn duplicate_method(
        class_name: impl Into<String>,
        method: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::DuplicateMethod {
            class_name: class_name.into(),
            method: method.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::ImplUnknownClass`] diagnostic.
    pub fn impl_unknown_class(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::ImplUnknownClass {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::CyclicTypeAlias`] diagnostic.
    pub fn cyclic_type_alias(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::CyclicTypeAlias {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::BlockingInAsync`] diagnostic.
    pub fn blocking_in_async(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::BlockingInAsync {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::ResourceNotManaged`] diagnostic.
    pub fn resource_not_managed(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::ResourceNotManaged {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::DivByZeroLiteral`] diagnostic.
    pub fn div_by_zero_literal(
        op: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::DivByZeroLiteral {
            op: op.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::NewtypeViolation`] diagnostic.
    pub fn newtype_violation(
        name: impl Into<String>,
        base: impl Into<String>,
        arg_type: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        let arg_type = arg_type.into();
        // Help text shows `{name}({arg_type_short})` — when the actual
        // is the same name as the base (e.g. `int` against an `int`-
        // newtype, the boundary case), drop the verbose type and just
        // suggest the bare wrap call so the help reads cleanly.
        let arg_type_short = if arg_type.contains(['[', '|', '<']) {
            // Compound type — keep as-is so the hint is at least
            // unambiguous, even if verbose.
            arg_type.clone()
        } else {
            // Primitive / single-identifier — use a placeholder so the
            // help doesn't read `UserId(int)` as if `int` were a value.
            "value".to_owned()
        };
        Self::NewtypeViolation {
            name: name.into(),
            base: base.into(),
            arg_type,
            arg_type_short,
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::FreezeNotFreezable`] diagnostic.
    pub fn freeze_not_freezable(
        name: impl Into<String>,
        kind: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::FreezeNotFreezable {
            name: name.into(),
            kind: kind.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::NewtypeInvalidBase`] diagnostic.
    /// FINDINGS v0.7.1 #14.
    pub fn newtype_invalid_base(
        name: impl Into<String>,
        base_kind: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::NewtypeInvalidBase {
            name: name.into(),
            base_kind: base_kind.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::AsyncWithoutAwait`] diagnostic.
    pub fn async_without_await(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::AsyncWithoutAwait {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::UseOfUninitialised`] diagnostic for a
    /// read of a declare-only `let NAME: T` binding on a path where
    /// no preceding statement assigned it.
    pub fn use_of_uninitialised(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        use_offset: usize,
        use_length: usize,
        decl_offset: usize,
        decl_length: usize,
    ) -> Self {
        Self::UseOfUninitialised {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(use_offset), use_length),
            decl_span: SourceSpan::new(SourceOffset::from(decl_offset), decl_length),
        }
    }

    /// Construct a [`TycError::PubNameCollision`] diagnostic for a
    /// name aggregated by `pub *` and exported by two distinct
    /// sibling modules.
    pub fn pub_name_collision(
        name: impl Into<String>,
        first: impl Into<String>,
        second: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::PubNameCollision {
            name: name.into(),
            first: first.into(),
            second: second.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct a [`TycError::PubStarOutsideInit`] advice for a
    /// `pub *` line in a non-`__init__.ty` module.
    pub fn pub_star_outside_init(
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::PubStarOutsideInit {
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length),
        }
    }

    /// Construct an [`TycError::ImmutableAssign`] diagnostic.
    pub fn immutable_assign(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        declaration_offset: usize,
        declaration_len: usize,
        assignment_offset: usize,
        assignment_len: usize,
    ) -> Self {
        let path = path.into();
        let source = source.into();
        Self::ImmutableAssign {
            name: name.into(),
            src: NamedSource::new(path, source),
            declaration: SourceSpan::new(SourceOffset::from(declaration_offset), declaration_len),
            assignment: SourceSpan::new(SourceOffset::from(assignment_offset), assignment_len),
        }
    }

    /// Construct a [`TycError::PatternShadowsOuter`] diagnostic.
    pub fn pattern_shadows_outer(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        declaration_offset: usize,
        declaration_len: usize,
        capture_offset: usize,
        capture_len: usize,
    ) -> Self {
        let path = path.into();
        let source = source.into();
        Self::PatternShadowsOuter {
            name: name.into(),
            src: NamedSource::new(path, source),
            declaration: SourceSpan::new(SourceOffset::from(declaration_offset), declaration_len),
            capture: SourceSpan::new(SourceOffset::from(capture_offset), capture_len),
        }
    }

    /// Construct a [`TycError::InvalidConfigValue`] diagnostic for a
    /// `typhon.toml` value that's not in the allowed set for its key.
    pub fn invalid_config_value(
        key: impl Into<String>,
        value: impl Into<String>,
        allowed: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self::InvalidConfigValue {
            key: key.into(),
            value: value.into(),
            allowed: allowed.into(),
            path: path.into(),
        }
    }

    /// Construct a [`TycError::OrphanPyImport`] warning for a relative `.py`
    /// import that resolves outside the project's `src/` tree.
    pub fn orphan_py_import(
        import_path: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::OrphanPyImport {
            import_path: import_path.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::PythonSemanticDrift`] warning surfaced by the
    /// Phase 5 Python-semantic-alignment regression sweep.
    pub fn python_semantic_drift(
        expression: impl Into<String>,
        detail: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::PythonSemanticDrift {
            expression: expression.into(),
            detail: detail.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::ContainsSecretLiteral`] warning for a
    /// `comptime` binding whose name matches the secret-suffix heuristic.
    pub fn contains_secret_literal(name: impl Into<String>, env_key: impl Into<String>) -> Self {
        Self::ContainsSecretLiteral {
            name: name.into(),
            env_key: env_key.into(),
        }
    }

    /// Construct a [`TycError::SecretLiteralInline`] warning for a plain
    /// `let` / `mut` / module-level binding whose name matches the secret
    /// suffix heuristic AND whose RHS is a raw string literal.
    pub fn secret_literal_inline(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::SecretLiteralInline {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::EmptyCollectionNoAnnotation`] warning for
    /// an empty collection literal bound without an explicit annotation.
    pub fn empty_collection_no_annotation(
        name: impl Into<String>,
        literal: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::EmptyCollectionNoAnnotation {
            name: name.into(),
            literal: literal.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::LoopClosureCapture`] warning.
    pub fn loop_closure_capture(
        name: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::LoopClosureCapture {
            name: name.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::IncompatibleOverride`] warning.
    #[allow(clippy::too_many_arguments)]
    pub fn incompatible_override(
        class_name: impl Into<String>,
        method: impl Into<String>,
        base: impl Into<String>,
        reason: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::IncompatibleOverride {
            class_name: class_name.into(),
            method: method.into(),
            base: base.into(),
            reason: reason.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::IsLiteralComparison`] warning.
    pub fn is_literal_comparison(
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::IsLiteralComparison {
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::MutableDefaultParam`] warning.
    pub fn mutable_default_param(
        name: impl Into<String>,
        literal: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::MutableDefaultParam {
            name: name.into(),
            literal: literal.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }

    /// Construct a [`TycError::TypingAliasInAnnotation`] warning for a
    /// deprecated `typing.<Name>` alias referenced from an annotation.
    pub fn typing_alias_in_annotation(
        name: impl Into<String>,
        suggestion: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::TypingAliasInAnnotation {
            name: name.into(),
            suggestion: suggestion.into(),
            src: NamedSource::new(path.into(), source.into()),
            span: SourceSpan::new(SourceOffset::from(offset), length.max(1)),
        }
    }
}

/// Compute a dedup key for `e`: `(code, rendered message)`. Stable
/// across copies of the same diagnostic produced by per-variant impl
/// distribution (B24).
fn diag_dedupe_key(e: &TycError) -> (String, String) {
    use miette::Diagnostic;
    let code = e
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "<no-code>".to_owned());
    let message = format!("{}", e);
    (code, message)
}

/// A list of diagnostics collected during a compiler phase.
#[derive(Debug, Default, Clone)]
pub struct Diagnostics {
    errors: Vec<TycError>,
    warnings: Vec<TycError>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_error(&mut self, e: TycError) {
        self.errors.push(e);
    }

    pub fn push_warning(&mut self, w: TycError) {
        self.warnings.push(w);
    }

    pub fn extend(&mut self, other: Diagnostics) {
        self.errors.extend(other.errors);
        self.warnings.extend(other.warnings);
    }

    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn errors(&self) -> &[TycError] {
        &self.errors
    }

    pub fn warnings(&self) -> &[TycError] {
        &self.warnings
    }

    /// Drop diagnostics that are exact duplicates of an earlier one in
    /// the same list. Used by callers that synthesise per-variant copies
    /// of a sealed-union impl block — without dedup, every variant
    /// produces the same `tyc::missing_return` / `tyc::non_exhaustive_match`
    /// once, so a 10-variant union prints 10 identical errors (B24).
    ///
    /// The dedup key is `(diagnostic code, rendered top-line message)`.
    /// Both are stable across copies of the same user-written method
    /// after preprocess expansion. Diagnostics with no code (the
    /// `Generic` variant) compare by message alone.
    pub fn dedupe(&mut self) {
        let mut seen: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        self.errors.retain(|e| {
            let key = diag_dedupe_key(e);
            seen.insert(key)
        });
        let mut seen_warn: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        self.warnings.retain(|e| {
            let key = diag_dedupe_key(e);
            seen_warn.insert(key)
        });
    }

    pub fn error_count(&self) -> usize {
        self.errors.len()
    }

    pub fn warning_count(&self) -> usize {
        self.warnings.len()
    }

    /// Consume the `Diagnostics` and return `(errors, warnings)` as owned vectors,
    /// allowing callers to move diagnostics without cloning.
    pub fn into_parts(self) -> (Vec<TycError>, Vec<TycError>) {
        (self.errors, self.warnings)
    }

    /// Remove duplicate diagnostics — entries with the same `Display` text
    /// and diagnostic code are considered identical.  Deduplication preserves
    /// the first occurrence and drops subsequent ones.
    pub fn dedup(&mut self) {
        dedup_vec(&mut self.errors);
        dedup_vec(&mut self.warnings);
    }
}

/// Compute the user-facing help suggestion for a [`TycError::TypeMismatch`]
/// diagnostic. The default phrasing is the "widen to `expected | actual`"
/// hint that was previously baked into the `#[diagnostic(help(...))]`
/// attribute; this helper now sits in front of it so collection-shape
/// mismatches get a targeted suggestion instead.
///
/// Detection is purely textual (the diagnostics layer only sees the
/// already-rendered type display strings, not a `Type` enum); it keys on
/// the `list[`, `dict[`, `set[` head and only suggests covariant
/// alternatives when both sides share the same head. Anything else
/// falls back to the original "widen the annotation to `T | U`" hint.
/// FINDINGS #37.
fn type_mismatch_help(expected: &str, actual: &str) -> String {
    if let Some(hint) = collection_variance_hint(expected, actual) {
        return hint;
    }
    if let Some(hint) = dict_literal_to_model_hint(expected, actual) {
        return hint;
    }
    format!(
        "change the value so it produces `{expected}`, or widen the annotation to \
         `{expected} | {actual}` if both are intended"
    )
}

/// When a dict literal flows into a `model` / `class` / `class!` binding,
/// the headline ("expected `UserCreate`, found `dict[str, str]`") gives
/// the user no clue that the fix is to use the constructor call form
/// (`UserCreate(name=…, age=…, email=…)`). FINDINGS #38.
///
/// TODO(#38): this textual hint cannot enumerate the *specific* fields
/// that are missing from the literal — the diagnostics layer only sees
/// the rendered display strings of `expected` / `actual`, not the
/// underlying `Type`s or `ModuleShapes`. To produce "missing fields:
/// age, email", the call site in `tyc-types` that builds the
/// dict-literal-vs-model mismatch needs to:
///
///   1. detect the shape (expected is a model/plain/class!, actual is a
///      `dict` literal whose key set is a strict subset of the model's
///      field names),
///   2. compute the missing-field list from the model's `InterfaceShape`,
///   3. emit a dedicated `TycError::ModelLiteralMissingFields` variant
///      (or thread the list into `type_mismatch` via a new constructor)
///      so this layer can render it verbatim.
///
/// Until that plumbing lands, the textual hint below at least redirects
/// the user to the constructor form, which is the right escape hatch.
fn dict_literal_to_model_hint(expected: &str, actual: &str) -> Option<String> {
    // Cheap recogniser: actual is a `dict[..]` generic, expected is a
    // bare identifier (no generic brackets, no union pipes). Anything
    // else is some other shape of mismatch.
    if !actual.starts_with("dict[") {
        return None;
    }
    if expected.contains('[') || expected.contains('|') {
        return None;
    }
    let first = expected.chars().next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    Some(format!(
        "dict literals don't auto-coerce to `{expected}`. Construct the value via \
         `{expected}(field1=…, field2=…, …)` so every required field is named and \
         type-checked. (Run `tyc explain type_mismatch` for the full list of fields \
         the model declares — naming the specific missing keys is tracked in \
         FINDINGS #38.)"
    ))
}

/// Inspect the rendered expected/actual type display strings and, when
/// both name the same invariant generic head (`list`, `dict`, `set`)
/// with different parameters, return a covariant-alternative
/// suggestion. Returns `None` for any other shape so the caller can
/// fall back to the default "widen to T | U" hint.
fn collection_variance_hint(expected: &str, actual: &str) -> Option<String> {
    // Helper: split `head[args]` into `(head, args)`. Returns `None`
    // when the input doesn't end in `]` so non-generic display strings
    // (`int`, `str`, …) take the default branch above.
    fn split_generic(s: &str) -> Option<(&str, &str)> {
        let stripped = s.strip_suffix(']')?;
        let open = stripped.find('[')?;
        Some((&stripped[..open], &stripped[open + 1..]))
    }
    let (exp_head, exp_args) = split_generic(expected)?;
    let (act_head, _act_args) = split_generic(actual)?;
    if exp_head != act_head || exp_args.is_empty() {
        return None;
    }
    match exp_head {
        "list" => Some(format!(
            "`list[T]` is invariant — `{actual}` is not assignable to `{expected}`. \
             Use `Sequence[{exp_args}]` (covariant, read-only) on the parameter, or rebind \
             the source as `{expected}` so the element type matches."
        )),
        "set" => Some(format!(
            "`set[T]` is invariant — `{actual}` is not assignable to `{expected}`. \
             Use `frozenset[{exp_args}]` (immutable) or `AbstractSet[{exp_args}]` \
             (covariant, read-only) on the parameter, or rebind the source as `{expected}`."
        )),
        "dict" => Some(format!(
            "`dict[K, V]` is invariant — `{actual}` is not assignable to `{expected}`. \
             Use `Mapping[{exp_args}]` (covariant in V, read-only) on the parameter, or \
             rebind the source as `{expected}`."
        )),
        _ => None,
    }
}

/// FINDINGS #32: rewrite known preprocess-synthesised lines in `source`
/// back to their original Typhon form so the rendered source listing
/// in a miette diagnostic doesn't leak compiler internals. The output
/// has the SAME byte length as the input — each substitution is
/// padded with trailing spaces inside the line — so every `SourceSpan`
/// constructed against the preprocessed source still indexes correctly.
///
/// Currently recognises:
///   - `class __typhon_impl_X(...)(object):` → `impl X(...):`
///   - `class __typhon_builtin_ext_X(...)(object):` → `extend X(...):`
///   - `if True:  # __typhon_unsafe__` → `unsafe:`
///   - `__typhon_freeze__(EXPR)` wrappers (RHS-only) → `EXPR`
///   - synthesised `if isinstance(__typhon_q_N__, __typhon_Err__):` etc.
///     `?`-operator scaffolding lines are replaced with all-spaces so
///     the listing skips past them without confusing the user.
///   - synthesised `from typhon_runtime import …` headers are blanked.
///
/// Anything that doesn't match is returned verbatim. The pass is
/// purely textual, line-oriented, and runs in O(source.len()).
///
/// **MVP scope**: this is the "hide synthetic line" minimum described
/// in FINDINGS #32. The longer-term fix is a full source map produced
/// by `preprocess` and consulted at diagnostic construction time —
/// see the `TODO(#32)` block in [`sanitize_synthetic_source`].
pub fn sanitize_synthetic_source(source: &str) -> String {
    if !source.contains("__typhon_") && !source.contains("typhon_runtime") {
        return source.to_owned();
    }
    // TODO(#32): the current pass is length-preserving but lossy — a
    // span that originally pointed at the `__typhon_q_0__` variable on
    // a synthesised `if isinstance(...)` line now lands on padding
    // spaces, which still surprises the user (the source listing
    // shows a blank line with the span underline at a column that
    // doesn't correspond to anything they wrote). The proper fix is:
    //
    //   1. extend [`tyc_syntax::preprocess::PreprocessResult`] with a
    //      `source_map: Vec<SpanMap>` field that records, for every
    //      synthesised byte range, the `(original_offset, original_length)`
    //      it should re-map to (or `None` if the synthesis is purely
    //      compiler-internal and should be hidden);
    //   2. plumb that map through `tyc_db::check_*` to the diagnostic
    //      construction sites in `tyc-types` / `tyc-resolve` /
    //      `tyc-analyse`. The map travels in `ExternalShapes` /
    //      a new analogue so no signature on tyc-types is widened
    //      gratuitously;
    //   3. at every `TycError::*` constructor call, look up the
    //      synthesised span and substitute the original span before
    //      `NamedSource::new(path, source).span()` builds the label.
    //
    // That plumbing is invasive enough to want its own dedicated PR;
    // until then the MVP below at least removes the worst of the
    // confusion (no `class __typhon_impl_Foo(object):` in user-facing
    // diagnostics, no `from typhon_runtime import Err as __typhon_Err__`).
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\r', '\n']);
        let terminator = &line[raw.len()..];
        if let Some(restored) = restore_synthetic_line(raw) {
            out.push_str(&restored);
        } else {
            out.push_str(raw);
        }
        out.push_str(terminator);
    }
    debug_assert_eq!(
        out.len(),
        source.len(),
        "sanitize_synthetic_source must preserve byte length so spans stay aligned"
    );
    out
}

/// Per-line worker for [`sanitize_synthetic_source`]. Returns `Some`
/// when the line matched a known synthetic pattern (the result has the
/// same byte length as the input, padded with trailing spaces inside
/// the line), `None` to leave the line untouched. Splitting this out
/// makes the pattern table easy to extend per future findings.
fn restore_synthetic_line(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let indent_len = line.len() - trimmed.len();
    let indent = &line[..indent_len];

    // Helper: build a result string of the same byte length as `line`,
    // with `replacement` substituted into the body. Pad with trailing
    // spaces (inside the line) so downstream byte offsets stay valid.
    let pad_to_length = |replacement: &str| -> String {
        let total = line.len();
        let body_len = total.saturating_sub(indent_len);
        let mut s = String::with_capacity(total);
        s.push_str(indent);
        let take = replacement.len().min(body_len);
        s.push_str(&replacement[..take]);
        // Pad with spaces to match original line length.
        for _ in s.len()..total {
            s.push(' ');
        }
        s
    };

    // 1. `class __typhon_impl_X(...)(object):` → `impl X(...):`
    if let Some(tail) = trimmed.strip_prefix("class __typhon_impl_") {
        // tail looks like `Name(object):` or `Name(Base)(object):`.
        let restored = tail.replacen("(object)", "", 1);
        return Some(pad_to_length(&format!("impl {}", restored)));
    }

    // 2. `class __typhon_builtin_ext_X(...)(object):` → `extend X(...):`
    if let Some(tail) = trimmed.strip_prefix("class __typhon_builtin_ext_") {
        let restored = tail.replacen("(object)", "", 1);
        return Some(pad_to_length(&format!("extend {}", restored)));
    }

    // 3. `if True:  # __typhon_unsafe__` → `unsafe:`
    if trimmed.starts_with("if True:") && trimmed.contains("__typhon_unsafe__") {
        return Some(pad_to_length("unsafe:"));
    }

    // 4. Synthesised `?`-operator scaffolding: blanks out the entire
    //    `if isinstance(__typhon_q_N__, __typhon_Err__):` line plus its
    //    paired `return __typhon_q_N__` / `let X = __typhon_q_N__.value`
    //    lines. The user wrote `let x = f()?`; the listing should not
    //    show the desugaring.
    if trimmed.contains("__typhon_q_") || trimmed.contains("__typhon_Err__") {
        return Some(pad_to_length(""));
    }

    // 5. `from typhon_runtime import …` and the bare `import
    //    typhon_runtime` are compiler-emitted and never present in the
    //    user's `.ty` source. Blank them out.
    if trimmed.starts_with("from typhon_runtime") || trimmed.starts_with("import typhon_runtime") {
        return Some(pad_to_length(""));
    }

    // 6. `__typhon_freeze__(EXPR)` wrappers — replace the wrapper with
    //    spaces, leaving the inner EXPR intact. Conservative scan: we
    //    only touch lines that contain the literal call form.
    if trimmed.contains("__typhon_freeze__(") {
        // Rewrite by replacing `__typhon_freeze__(` with the same
        // number of spaces; the matching `)` is harder to locate
        // unambiguously, so leave it (it renders as a stray paren but
        // is far less alarming than the synthetic call name).
        let needle = "__typhon_freeze__(";
        let space = " ".repeat(needle.len());
        let restored = line.replacen(needle, &space, 1);
        // The above replacement preserves byte length already.
        debug_assert_eq!(restored.len(), line.len());
        return Some(restored);
    }

    None
}

/// FINDINGS #34, #35: inspect a raw Python-parser error message and the
/// surrounding source to recover a Typhon-specific hint. Returns
/// `None` when the message doesn't match a known shape so callers can
/// pass through with no help block.
///
/// Cases handled:
///   - **#34** Multi-line `|>` chain without parens: the previous
///     non-blank line ends with `|>` and the parser bailed with an
///     indentation message. The fix is to wrap the entire chain in
///     parentheses so Python treats the continuation as part of the
///     same expression.
///   - **#35** `freeze let` inside a function body: `freeze let` is a
///     module-level-only declaration that the preprocessor leaves
///     unmodified at non-zero indent, so the Python parser sees two
///     statements on one line and complains about statement
///     separators. The hint says so explicitly.
pub(crate) fn parse_error_hint(message: &str, source: &str, offset: usize) -> Option<String> {
    let line_starts = line_start_offsets(source);
    let line_idx = line_for_offset(&line_starts, offset);

    // #35: `freeze let` at non-module scope. The preprocessor strips the
    // `freeze` keyword only at indent 0, so an indented `freeze let`
    // confuses the parser. Detect this by scanning the offending line
    // for a leading `freeze let` after some whitespace.
    if let Some(line) = source_line(source, &line_starts, line_idx) {
        let trimmed = line.trim_start();
        let indent_len = line.len() - trimmed.len();
        if indent_len > 0 && trimmed.starts_with("freeze let ") {
            return Some(
                "`freeze let` is a module-level-only declaration (it wraps the RHS in \
                 `__typhon_freeze__(...)` so the value is deep-immutable at runtime). \
                 Move the binding to the top level, or use a plain `let` if the binding \
                 only needs to be lexically immutable."
                    .to_owned(),
            );
        }
    }

    // `?` error-propagation inside an f-string expression part. The
    // preprocessor's `?` expansion can't reach into string literals, so
    // the raw `?` hits the Python parser and dies with an opaque parse
    // error. Detect `{...?...}` inside an f-string on the offending line
    // and point at the rebind idiom.
    if let Some(line) = source_line(source, &line_starts, line_idx) {
        let has_fstring = line.contains("f\"") || line.contains("f'");
        if has_fstring {
            let mut depth = 0usize;
            let mut question_in_braces = false;
            for ch in line.chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth = depth.saturating_sub(1),
                    '?' if depth > 0 => {
                        question_in_braces = true;
                        break;
                    }
                    _ => {}
                }
            }
            if question_in_braces {
                return Some(
                    "the `?` error-propagation operator cannot be used inside an \
                     f-string expression — the preprocessor cannot rewrite inside \
                     string literals. Bind the result first: \
                     `let v: T = fallible()?` then interpolate `{v}`."
                        .to_owned(),
                );
            }
        }
    }

    // #34: previous non-blank line ends with `|>` and current message
    // mentions indentation. Wrapping the chain in parens flips the
    // continuation from a statement boundary into a sub-expression.
    let lower = message.to_ascii_lowercase();
    let mentions_indent = lower.contains("indentation")
        || lower.contains("indent")
        || lower.contains("unexpected indent");
    if mentions_indent {
        // Walk back from the current line looking for the closest
        // non-blank/non-comment line — that's where the chain
        // started.
        let mut probe = line_idx;
        while probe > 0 {
            probe -= 1;
            let prev = source_line(source, &line_starts, probe);
            let Some(prev) = prev else { break };
            let trimmed = prev.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // Strip a trailing inline comment then check for the `|>`
            // suffix.
            let code = trimmed.split('#').next().unwrap_or(trimmed).trim_end();
            if code.ends_with("|>") {
                return Some(
                    "multi-line pipe chains (`|>`) must be wrapped in parentheses, \
                     otherwise Python's indentation rules treat the continuation as a \
                     new statement. Wrap the whole chain: `let result = (value |> f \
                     |> g |> h)`."
                        .to_owned(),
                );
            }
            break;
        }
    }

    None
}

/// Cached newline offsets for [`source_line`] / [`line_for_offset`].
/// Walks the source once; downstream lookups are O(log n) binary
/// search.
fn line_start_offsets(source: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

fn line_for_offset(starts: &[usize], offset: usize) -> usize {
    match starts.binary_search(&offset) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    }
}

fn source_line<'a>(source: &'a str, starts: &[usize], line_idx: usize) -> Option<&'a str> {
    let begin = *starts.get(line_idx)?;
    let end = starts.get(line_idx + 1).copied().unwrap_or(source.len());
    let slice = source.get(begin..end)?;
    Some(slice.trim_end_matches(['\r', '\n']))
}

/// FINDINGS #32: an owning wrapper around a [`TycError`] that overrides
/// `Diagnostic::source_code` to return a sanitised view (synthetic
/// `__typhon_*` lines blanked out / restored). Every other trait
/// method delegates straight through, so codes, labels, help, severity
/// and so on read identically to the inner diagnostic.
///
/// Construct via [`SanitisedDiagnostic::wrap`]; render with miette as
/// usual:
/// ```ignore
/// eprintln!(
///     "{:?}",
///     miette::Report::new_boxed(Box::new(SanitisedDiagnostic::wrap(err.clone())))
/// );
/// ```
#[derive(Clone)]
pub struct SanitisedDiagnostic {
    inner: TycError,
    sanitised: Option<NamedSource<String>>,
    /// B15 remap table for `impl Alias:`-over-sealed-union distribution.
    /// `preprocess` duplicates the impl body once per union variant, so
    /// the 2nd…Nth synthesised blocks occupy line numbers *past the end
    /// of the user's real source*. A label landing in one of those
    /// duplicated blocks is redirected back to the byte-equivalent
    /// position in the first (real-source-aligned) block, so the
    /// rendered line number never exceeds the file's real line count.
    block_remap: Option<BlockRemap>,
}

impl SanitisedDiagnostic {
    /// Build a wrapper that masks synthetic preprocess output from the
    /// rendered source listing. When the inner diagnostic doesn't carry
    /// a `NamedSource` (e.g. `TycError::Io`) the wrapper is a no-op
    /// pass-through.
    pub fn wrap(inner: TycError) -> Self {
        let sanitised = inner.embedded_source().map(|src| {
            let cleaned = sanitize_synthetic_source(src.inner());
            NamedSource::new(src.name(), cleaned)
        });
        let block_remap = sanitised.as_ref().and_then(|s| {
            let text = named_source_text(s);
            let distributed = distributed_impl_lines(&text);
            BlockRemap::from_sanitised(&text, &distributed)
        });
        Self {
            inner,
            sanitised,
            block_remap,
        }
    }

    /// Build a wrapper that reuses a pre-sanitised `NamedSource`. Callers
    /// rendering many diagnostics for the same file should sanitise once
    /// and pass the result here to avoid the O(n_diags × file_size)
    /// rework that [`wrap`](Self::wrap) does on a hot loop.
    pub fn wrap_with_source(inner: TycError, sanitised: NamedSource<String>) -> Self {
        let text = named_source_text(&sanitised);
        let distributed = distributed_impl_lines(&text);
        let block_remap = BlockRemap::from_sanitised(&text, &distributed);
        Self {
            inner,
            sanitised: Some(sanitised),
            block_remap,
        }
    }
}

/// Read the full text out of a `NamedSource<String>`. miette doesn't
/// expose the inner string directly, so we round-trip through the
/// `SourceCode` interface (a 1-byte span at offset 0 with an unbounded
/// trailing-context request returns the whole document — see
/// [`TycError::embedded_source`] for the same trick).
fn named_source_text(src: &NamedSource<String>) -> String {
    use miette::SourceCode;
    let span = miette::SourceSpan::new(miette::SourceOffset::from(0), 1);
    match src.read_span(&span, 0, usize::MAX) {
        Ok(contents) => std::str::from_utf8(contents.data())
            .map(|s| s.to_owned())
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// B15 byte-offset remap for `impl Alias:`-over-sealed-union distribution.
///
/// The preprocessor expands one `impl Alias:` block (where `Alias` is a
/// sealed union `A | B | …`) into one `impl A:` / `impl B:` / … block
/// per variant, byte-duplicating the user's method bodies. The first
/// block keeps the original source's line numbers; every later block is
/// *synthetic*, sitting past the end of the real `.ty` file. A
/// diagnostic that fires inside a later block therefore reports a line
/// number greater than the file's real line count (B15).
///
/// This table records, for each synthetic (2nd…Nth) block, the byte
/// range it occupies in the rendered source and the offset `delta` that
/// maps a position inside it back to the byte-equivalent position in the
/// first block (which is real-source-aligned). Because the blocks are
/// byte-for-byte duplicates of one another, subtracting `delta` lands on
/// the matching column of the matching real line — the diagnostic points
/// at honest source, never past EOF.
#[derive(Clone, Debug)]
struct BlockRemap {
    /// `(start, end, delta)` — a label offset in `start..end` is remapped
    /// to `offset - delta`. Sorted by `start`, non-overlapping.
    ranges: Vec<(usize, usize, usize)>,
}

impl BlockRemap {
    /// Build a remap table from the *sanitised* source text (the form
    /// `sanitize_synthetic_source` produces, where each distributed
    /// `class __typhon_impl_<Variant>(object):` header has already been
    /// restored to `impl <Variant>:`). Returns `None` when the source
    /// contains no distributed impl group (the overwhelmingly common
    /// case), so normal diagnostics pay only a cheap scan.
    ///
    /// Detection: a *group* is a maximal run of two or more `impl <Name>:`
    /// header lines at the same indent whose bodies (every more-indented
    /// line up to the next dedent) are byte-for-byte identical AND whose
    /// header lines are all in `distributed_lines` — the 0-based line
    /// indices the preprocessor recorded for blocks it synthesised by
    /// distributing one `impl Alias:` over a sealed union.
    ///
    /// B15 robustness: a byte-identical body is NOT sufficient to treat a
    /// run as synthetic — two genuinely-real, adjacent `impl A:` /
    /// `impl B:` blocks the user authored can share an identical body and
    /// are indistinguishable from a distribution by text alone. Gating on
    /// `distributed_lines` (the only place the real/synthetic distinction
    /// is recorded) ensures a real block is never grouped or remapped.
    /// When `distributed_lines` is empty the function returns `None`
    /// (no remap) — safer than wrongly collapsing a real block.
    fn from_sanitised(source: &str, distributed_lines: &[usize]) -> Option<Self> {
        // Cheap reject: the restoration step only emits `impl ` headers
        // for files that carried an `impl`/`extend` block. Bail early
        // when there isn't one — or when nothing was distributed, in
        // which case there is nothing to remap.
        if distributed_lines.is_empty() || !source.contains("impl ") {
            return None;
        }
        let is_distributed = |line: usize| distributed_lines.contains(&line);
        let lines: Vec<&str> = source.split_inclusive('\n').collect();
        // Byte offset at the start of each line.
        let mut starts = Vec::with_capacity(lines.len() + 1);
        let mut acc = 0usize;
        for l in &lines {
            starts.push(acc);
            acc += l.len();
        }
        starts.push(acc);

        let impl_header_indent = |raw: &str| -> Option<usize> {
            let indent = raw.len() - raw.trim_start().len();
            let trimmed = raw.trim_start();
            let after = trimmed.strip_prefix("impl ")?;
            // Must look like `impl <Name…>:` — a header, not a method
            // body line that merely starts with the word.
            if after.trim_end().ends_with(':') {
                Some(indent)
            } else {
                None
            }
        };

        // The (header_line, body_start_line, body_end_line) of each impl
        // block, in source order. `body_end_line` is exclusive.
        let mut blocks: Vec<(usize, usize, usize)> = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let raw = lines[i].trim_end_matches(['\r', '\n']);
            if let Some(indent) = impl_header_indent(raw) {
                let header = i;
                let mut j = i + 1;
                let mut last_body = header;
                while j < lines.len() {
                    let cand = lines[j].trim_end_matches(['\r', '\n']);
                    if cand.trim().is_empty() {
                        j += 1;
                        continue;
                    }
                    let cand_indent = cand.len() - cand.trim_start().len();
                    if cand_indent <= indent {
                        break;
                    }
                    last_body = j;
                    j += 1;
                }
                blocks.push((header, header + 1, last_body + 1));
                i = last_body + 1;
            } else {
                i += 1;
            }
        }

        if blocks.len() < 2 {
            return None;
        }

        // Concatenated body text of a block, for the duplicate check.
        let body_text = |b: &(usize, usize, usize)| -> String { lines[b.1..b.2].concat() };

        let mut ranges: Vec<(usize, usize, usize)> = Vec::new();
        // Walk consecutive blocks; group runs with identical bodies whose
        // header lines are all flagged as distributed.
        let mut g = 0;
        while g < blocks.len() {
            let first = blocks[g];
            // A real (non-distributed) block anchors no group: skip it so
            // a genuine `impl A:` immediately before a distribution can't
            // absorb the synthetic blocks that follow.
            if !is_distributed(first.0) {
                g += 1;
                continue;
            }
            let first_body = body_text(&first);
            let mut k = g + 1;
            // Only group blocks with a non-empty body, a byte-identical
            // body, AND a distributed header. Genuinely empty (or
            // whitespace/comment-only) `impl` blocks all share the empty
            // body string and would otherwise be mis-grouped as a synthetic
            // distribution and remapped to a wrong location.
            if !first_body.trim().is_empty() {
                while k < blocks.len()
                    && is_distributed(blocks[k].0)
                    && body_text(&blocks[k]) == first_body
                {
                    k += 1;
                }
            }
            // blocks[g..k] form a distributed group of size (k - g).
            if k - g >= 2 {
                for dup in &blocks[g + 1..k] {
                    // Map the duplicate block back onto the first block
                    // *per line*, not per block: the synthesised headers
                    // differ in length (`impl Circle:` vs `impl Triangle:`),
                    // so a single block-level byte delta would skew the
                    // column on body lines. Each body line is byte-identical
                    // to the first block's matching body line, so we emit
                    // one range per line with its own delta — preserving
                    // the exact column.
                    //
                    // The header line itself is redirected to the first
                    // block's header (the real `impl Alias:` source line);
                    // a span there can't preserve a meaningful column
                    // across differing variant names, so it clamps to the
                    // header line start. Body lines keep their column.
                    let header_delta = starts[dup.0] - starts[first.0];
                    ranges.push((starts[dup.0], starts[dup.1], header_delta));
                    // Body lines: dup body line i ↔ first body line i.
                    let dup_body_lines = dup.2 - dup.1;
                    for off in 0..dup_body_lines {
                        let dup_line = dup.1 + off;
                        let first_line = first.1 + off;
                        let dline_start = starts[dup_line];
                        let dline_end = starts[dup_line + 1];
                        let fline_start = starts[first_line];
                        ranges.push((dline_start, dline_end, dline_start - fline_start));
                    }
                }
            }
            g = k;
        }
        ranges.sort_by_key(|r| r.0);

        if ranges.is_empty() {
            None
        } else {
            Some(Self { ranges })
        }
    }

    /// Remap a single byte offset out of a synthetic duplicate block back
    /// onto the first (real-source-aligned) block. Offsets outside every
    /// synthetic range are returned unchanged.
    fn remap_offset(&self, offset: usize) -> usize {
        for &(start, end, delta) in &self.ranges {
            if offset >= start && offset < end {
                return offset - delta;
            }
        }
        offset
    }
}

/// Recover the set of distributed `impl <Variant>:` header line indices
/// (0-based) from a *sanitised* source buffer.
///
/// The driver that renders a diagnostic only holds the diagnostic's
/// embedded source — not the [`tyc_syntax::PreprocessResult`] that
/// recorded `impl_distributed_lines` at preprocess time. Re-deriving the
/// set here, from structure that survives into the sanitised buffer,
/// keeps the B15 remap correct without threading the metadata through
/// every analysis crate.
///
/// The signal is real, not a textual-prefix heuristic: the preprocessor
/// only distributes an `impl Alias:` block when `Alias` is a sealed-union
/// `type Alias = V1 | V2 | …` declared in the same module. So a run of
/// consecutive `impl <Name>:` blocks is synthetic iff the run's header
/// names equal, *in order*, the full variant list of such an alias and
/// the bodies are byte-identical. Two genuinely-real adjacent
/// `impl A:` / `impl B:` blocks with no matching alias never match and
/// are therefore never flagged — exactly the case B15 must protect.
///
/// This mirrors the `expand_impl_sealed_unions` rule in `tyc-syntax`, so
/// the recovered set equals the recorded `impl_distributed_lines` for any
/// buffer the preprocessor produced.
fn distributed_impl_lines(source: &str) -> Vec<usize> {
    if !source.contains("impl ") {
        return Vec::new();
    }
    let aliases = sealed_union_aliases(source);
    if aliases.is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = source.split_inclusive('\n').collect();

    // Header line + name + (body_start, body_end exclusive) for each
    // `impl <Name>:` block, in source order.
    struct ImplBlock {
        header: usize,
        name: String,
        body_start: usize,
        body_end: usize,
    }
    let mut blocks: Vec<ImplBlock> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i].trim_end_matches(['\r', '\n']);
        if let Some(name) = impl_header_name(raw) {
            let indent = raw.len() - raw.trim_start().len();
            let header = i;
            let mut j = i + 1;
            let mut last_body = header;
            while j < lines.len() {
                let cand = lines[j].trim_end_matches(['\r', '\n']);
                if cand.trim().is_empty() {
                    j += 1;
                    continue;
                }
                let cand_indent = cand.len() - cand.trim_start().len();
                if cand_indent <= indent {
                    break;
                }
                last_body = j;
                j += 1;
            }
            blocks.push(ImplBlock {
                header,
                name,
                body_start: header + 1,
                body_end: last_body + 1,
            });
            i = last_body + 1;
        } else {
            i += 1;
        }
    }

    let body_text = |b: &ImplBlock| -> String { lines[b.body_start..b.body_end].concat() };

    let mut out: Vec<usize> = Vec::new();
    let mut g = 0;
    while g < blocks.len() {
        // A distribution group is `run` consecutive blocks whose names
        // equal some alias's variant list in order, with identical
        // non-empty bodies. Match greedily against each alias.
        let first_body = body_text(&blocks[g]);
        let mut matched = false;
        if !first_body.trim().is_empty() {
            for variants in aliases.values() {
                let run = variants.len();
                if run < 2 || g + run > blocks.len() {
                    continue;
                }
                let names_match = blocks[g..g + run]
                    .iter()
                    .zip(variants.iter())
                    .all(|(b, v)| &b.name == v);
                if !names_match {
                    continue;
                }
                let bodies_match = blocks[g + 1..g + run]
                    .iter()
                    .all(|b| body_text(b) == first_body);
                if !bodies_match {
                    continue;
                }
                for b in &blocks[g..g + run] {
                    out.push(b.header);
                }
                g += run;
                matched = true;
                break;
            }
        }
        if !matched {
            g += 1;
        }
    }
    out.sort_unstable();
    out
}

/// Extract the bare class/alias name from an `impl <Name…>:` header line
/// in a sanitised buffer. Returns `None` when the line isn't an impl
/// header. Strips any `impl[T,…]` type-param prefix and any `[args]` /
/// `(bases)` suffix on the name so generic and based forms collapse to
/// their head name — matching `tyc-syntax`'s distribution rule.
fn impl_header_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim_start();
    let after = if let Some(s) = trimmed.strip_prefix("impl ") {
        s
    } else if let Some(s) = trimmed.strip_prefix("impl[") {
        // Skip the `[T, …]` impl type-param list.
        let mut depth = 1i32;
        let mut end = None;
        for (i, c) in s.char_indices() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        s[end?..].trim_start()
    } else {
        return None;
    };
    let header = after.trim_end();
    let body = header.strip_suffix(':')?;
    let head_end = body
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(body.len());
    let name = &body[..head_end];
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

/// Collect sealed-union aliases (`type NAME[…]? = V1 | V2 | …`, two or
/// more variants) from a source buffer at indent zero, mapping each alias
/// name to its ordered list of variant head-names. Mirrors
/// `tyc_syntax::preprocess::collect_sealed_union_aliases_from_text`,
/// reimplemented here so `tyc-diagnostics` needn't depend on the
/// preprocessor internals.
fn sealed_union_aliases(source: &str) -> std::collections::HashMap<String, Vec<String>> {
    let mut out = std::collections::HashMap::new();
    for line in source.lines() {
        // Indent zero only — nested `type` aliases aren't legal targets.
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let after = match line.trim_end().strip_prefix("type ") {
            Some(s) => s,
            None => continue,
        };
        let name_end = after
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        if name_end == 0 {
            continue;
        }
        let name = &after[..name_end];
        let rest = &after[name_end..];
        // Skip an optional `[T, …]` type-param list on the alias name.
        let after_tps = if rest.starts_with('[') {
            let mut depth = 0i32;
            let mut close = None;
            for (i, c) in rest.char_indices() {
                match c {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            match close {
                Some(p) => &rest[p..],
                None => continue,
            }
        } else {
            rest
        };
        let rhs = match after_tps.trim_start().strip_prefix('=') {
            Some(s) => s.trim(),
            None => continue,
        };
        // Strip a trailing `# comment`.
        let rhs = rhs.split('#').next().unwrap_or(rhs).trim();
        if rhs.is_empty() {
            continue;
        }
        let variants: Vec<String> = split_top_level_pipes(rhs)
            .into_iter()
            .map(|p| {
                let p = p.trim();
                let head_end = p
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(p.len());
                p[..head_end].to_owned()
            })
            .filter(|p| !p.is_empty())
            .collect();
        if variants.len() >= 2 {
            out.insert(name.to_owned(), variants);
        }
    }
    out
}

/// Split `s` on top-level `|` characters, respecting `[]` / `()` / `{}`
/// nesting and `'`/`"` string literals. Operands are returned in source
/// order (untrimmed; callers trim).
fn split_top_level_pipes(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0usize;
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_str = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'|' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

/// Compute the sanitised `NamedSource` for a single diagnostic. Returns
/// `None` for variants that don't carry source text (`TycError::Io`).
/// Renderers grouping diagnostics by file should call this once per
/// file and reuse the result via [`SanitisedDiagnostic::wrap_with_source`].
pub fn sanitised_named_source_for(err: &TycError) -> Option<NamedSource<String>> {
    let src = err.embedded_source()?;
    let cleaned = sanitize_synthetic_source(src.inner());
    Some(NamedSource::new(src.name(), cleaned))
}

impl std::fmt::Debug for SanitisedDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.inner, f)
    }
}

impl std::fmt::Display for SanitisedDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl std::error::Error for SanitisedDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

impl miette::Diagnostic for SanitisedDiagnostic {
    fn code<'b>(&'b self) -> Option<Box<dyn std::fmt::Display + 'b>> {
        self.inner.code()
    }
    fn severity(&self) -> Option<miette::Severity> {
        self.inner.severity()
    }
    fn help<'b>(&'b self) -> Option<Box<dyn std::fmt::Display + 'b>> {
        self.inner.help()
    }
    fn url<'b>(&'b self) -> Option<Box<dyn std::fmt::Display + 'b>> {
        self.inner.url()
    }
    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        let labels = self.inner.labels()?;
        // B15: redirect any label that landed in a synthetic duplicated
        // `impl Variant:` block (past the real source's EOF) back onto the
        // first, real-source-aligned block. No-op when the file carries
        // no distributed impl group.
        let Some(remap) = self.block_remap.clone() else {
            return Some(labels);
        };
        let remapped = labels.map(move |label| {
            let span = label.inner();
            let new_offset = remap.remap_offset(span.offset());
            if new_offset == span.offset() {
                return label;
            }
            let new_span = miette::SourceSpan::new(new_offset.into(), span.len());
            miette::LabeledSpan::new_with_span(label.label().map(|s| s.to_owned()), new_span)
        });
        Some(Box::new(remapped))
    }
    fn related<'b>(&'b self) -> Option<Box<dyn Iterator<Item = &'b dyn miette::Diagnostic> + 'b>> {
        self.inner.related()
    }
    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        self.inner.diagnostic_source()
    }
    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        if let Some(src) = &self.sanitised {
            return Some(src);
        }
        self.inner.source_code()
    }
}

impl TycError {
    /// FINDINGS #32: best-effort accessor that returns the `NamedSource`
    /// embedded in the variant, if any, so the renderer wrapper can
    /// substitute a sanitised copy. Only the labels read from the same
    /// source; the diagnostic message itself doesn't include the
    /// preprocessed text.
    ///
    /// Implemented via Debug + miette's `SourceCode` indirection so we
    /// don't have to enumerate every variant by hand — `source_code()`
    /// is part of the `Diagnostic` trait and downcasts cleanly to
    /// `NamedSource<String>` for every variant that uses one.
    fn embedded_source(&self) -> Option<NamedSourceView<'_>> {
        use miette::Diagnostic;
        let any = self.source_code()?;
        // miette's `SourceCode` trait doesn't expose a `len()`, but
        // [`SourceCode::read_span`] returns the document slice that
        // covers `span` plus `context_lines_before` / `_after` of
        // context. Asking for a 1-byte span at offset 0 with a
        // ridiculously large `context_lines_after` returns the full
        // document because miette walks the source until it has that
        // many trailing newlines (which exhausts the iterator long
        // before it reaches the cap).
        let span = miette::SourceSpan::new(miette::SourceOffset::from(0), 1);
        let full = any.read_span(&span, 0, usize::MAX).ok()?;
        let bytes = full.data();
        let text = std::str::from_utf8(bytes).ok()?;
        let name = full.name().map(|s| s.to_owned()).unwrap_or_default();
        Some(NamedSourceView {
            name,
            text: text.to_owned(),
            _marker: std::marker::PhantomData,
        })
    }
}

/// Owned snapshot of a [`miette::NamedSource`] view extracted from a
/// [`TycError`]'s `source_code()`. Holds owned strings so the caller
/// can rewrite the text without aliasing the diagnostic's interior.
struct NamedSourceView<'a> {
    name: String,
    text: String,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> NamedSourceView<'a> {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn inner(&self) -> &str {
        &self.text
    }
}

fn dedup_vec(v: &mut Vec<TycError>) {
    use miette::Diagnostic;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    v.retain(|e| {
        let code = e.code().map(|c| c.to_string()).unwrap_or_default();
        let span = e
            .labels()
            .and_then(|mut it| it.next())
            .map(|l| format!("{}:{}", l.inner().offset(), l.inner().len()))
            .unwrap_or_default();
        seen.insert(format!("{}\x00{}\x00{}", e, code, span))
    });
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── TycError constructor correctness ─────────────────────────────────────

    #[test]
    fn io_error_contains_path_and_cause() {
        let e = TycError::io("foo.ty", &std::io::Error::other("disk full"));
        let msg = e.to_string();
        assert!(msg.contains("foo.ty"), "path should appear in message");
        assert!(msg.contains("disk full"), "cause should appear in message");
        assert!(matches!(e, TycError::Io { .. }));
    }

    #[test]
    fn generic_error_round_trips_message() {
        let e = TycError::generic("something went wrong");
        assert_eq!(e.to_string(), "something went wrong");
        assert!(matches!(e, TycError::Generic { .. }));
    }

    #[test]
    fn parse_error_is_correct_variant() {
        let e = TycError::parse("a.ty", "val x = 1", "unexpected token", 4);
        assert!(matches!(e, TycError::Parse { .. }));
        let msg = e.to_string();
        assert!(msg.contains("a.ty"));
    }

    #[test]
    fn unknown_name_contains_name() {
        let e = TycError::unknown_name("foo", "a.ty", "foo", 0, 3);
        assert!(matches!(e, TycError::UnknownName { .. }));
        assert!(e.to_string().contains("foo"));
    }

    #[test]
    fn type_mismatch_contains_expected_and_actual() {
        let e = TycError::type_mismatch("int", "str", "a.ty", "val x: int = \"hi\"", 0, 5);
        assert!(matches!(e, TycError::TypeMismatch { .. }));
        let msg = e.to_string();
        assert!(msg.contains("int"), "expected type should appear");
        assert!(msg.contains("str"), "actual type should appear");
    }

    #[test]
    fn type_mismatch_list_variance_suggests_sequence() {
        // FINDINGS #37: invariant collection mismatch should point at
        // the covariant read-only view, not "widen to T | U".
        let e = TycError::type_mismatch(
            "list[Animal]",
            "list[Dog]",
            "a.ty",
            "let xs: list[Animal] = dogs",
            0,
            3,
        );
        let TycError::TypeMismatch { suggestion, .. } = &e else {
            panic!("expected TypeMismatch variant");
        };
        assert!(
            suggestion.contains("Sequence[Animal]"),
            "list-variance hint must mention Sequence, got: {suggestion}"
        );
        assert!(
            !suggestion.contains("list[Animal] | list[Dog]"),
            "should not suggest a union widening: {suggestion}"
        );
    }

    #[test]
    fn type_mismatch_dict_variance_suggests_mapping() {
        let e = TycError::type_mismatch(
            "dict[str, Animal]",
            "dict[str, Dog]",
            "a.ty",
            "let xs: dict[str, Animal] = dogs",
            0,
            3,
        );
        let TycError::TypeMismatch { suggestion, .. } = &e else {
            panic!("expected TypeMismatch variant");
        };
        assert!(
            suggestion.contains("Mapping[str, Animal]"),
            "dict-variance hint must mention Mapping, got: {suggestion}"
        );
    }

    #[test]
    fn parse_error_hint_pipe_chain_without_parens() {
        // FINDINGS #34: multi-line `|>` without parens should redirect
        // the user at parenthesising the chain rather than the raw
        // "Unexpected indentation" message from the Python parser.
        let source = "let result = value |>\n    f()\n";
        let hint = parse_error_hint(
            "Unexpected indentation",
            source,
            source.find("    f").unwrap(),
        );
        let hint = hint.expect("pipe-chain hint must fire when prev line ends with |>");
        assert!(
            hint.contains("pipe chain") && hint.contains("parentheses"),
            "hint should mention pipe and parens, got: {hint}"
        );
    }

    #[test]
    fn parse_error_hint_freeze_let_inside_function() {
        // FINDINGS #35: `freeze let` at non-zero indent is module-level
        // only — fire a dedicated hint instead of the generic
        // "Simple statements must be separated …".
        let source = "def f() -> None:\n    freeze let x: int = 1\n";
        let offset = source.find("    freeze").unwrap();
        let hint = parse_error_hint(
            "Simple statements must be separated by newlines or semicolons",
            source,
            offset,
        );
        let hint = hint.expect("freeze-let hint must fire when the offending line is indented");
        assert!(
            hint.contains("module-level"),
            "hint should mention module-level only, got: {hint}"
        );
    }

    #[test]
    fn sanitize_synthetic_source_strips_impl_class_wrapper() {
        // FINDINGS #32 MVP: the rendered source listing for a span
        // pointing at `class __typhon_impl_Foo(object):` should show
        // `impl Foo:` instead, with the rest of the line padded to
        // preserve byte offsets for the label.
        let raw = "class __typhon_impl_Foo(object):\n    def m(self) -> int:\n";
        let out = sanitize_synthetic_source(raw);
        assert_eq!(out.len(), raw.len(), "must preserve byte length");
        assert!(
            out.starts_with("impl Foo:"),
            "first line should be restored to `impl Foo:`, got: {out}"
        );
        assert!(
            !out.contains("__typhon_impl_"),
            "synthetic wrapper must be hidden, got: {out}"
        );
    }

    #[test]
    fn block_remap_redirects_duplicate_impl_blocks() {
        // B15: the sanitised buffer for an `impl Alias:` distributed over
        // a two-variant union has two consecutive `impl <Variant>:` blocks
        // with byte-identical bodies. A label offset inside the *second*
        // (synthetic, past-EOF) block must remap onto the byte-equivalent
        // position in the first (real-source-aligned) block.
        let sanitised = "impl Leaf:\n\
                         \x20\x20\x20\x20def total(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.x\n\
                         \n\
                         impl Node:\n\
                         \x20\x20\x20\x20def total(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.x\n";
        // Headers `impl Leaf:` (line 0) and `impl Node:` (line 4) are the
        // synthetic distribution; both are flagged as distributed.
        let remap = BlockRemap::from_sanitised(sanitised, &[0, 4])
            .expect("two duplicate impl blocks should produce a remap");

        // Offset of `self.x` inside the SECOND block.
        let second_block = sanitised.find("impl Node:").unwrap();
        let dup_self = second_block + sanitised[second_block..].find("self.x").unwrap();
        // Offset of `self.x` inside the FIRST block.
        let first_self = sanitised.find("self.x").unwrap();

        assert_eq!(
            remap.remap_offset(dup_self),
            first_self,
            "a label in the duplicated block must redirect onto the first block"
        );
        // An offset in the first block is left untouched.
        assert_eq!(remap.remap_offset(first_self), first_self);
    }

    #[test]
    fn block_remap_ignores_single_impl_block() {
        // A lone `impl Foo:` (no union distribution) must NOT be remapped —
        // its line numbers are already real, so the table is `None`.
        let sanitised = "impl Foo:\n\
                         \x20\x20\x20\x20def m(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.x\n";
        assert!(
            BlockRemap::from_sanitised(sanitised, &[0]).is_none(),
            "a single impl block has no synthetic duplicates to remap"
        );
    }

    #[test]
    fn block_remap_preserves_columns_across_uneven_variant_names() {
        // Variant headers differ in length (`impl Circle:` vs
        // `impl Triangle:`), so the per-line delta — not a single
        // block-level delta — is what preserves the body column. A label
        // in the longer-named block's body must map to the SAME column in
        // the first block's body.
        let sanitised = "impl Circle:\n\
                         \x20\x20\x20\x20def area(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.q\n\
                         \n\
                         impl Triangle:\n\
                         \x20\x20\x20\x20def area(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.q\n";
        // `impl Circle:` (line 0) and `impl Triangle:` (line 4) are the
        // synthetic distribution.
        let remap = BlockRemap::from_sanitised(sanitised, &[0, 4]).unwrap();
        let first_q = sanitised.find("self.q").unwrap();
        let tri = sanitised.find("impl Triangle:").unwrap();
        let dup_q = tri + sanitised[tri..].find("self.q").unwrap();
        assert_eq!(
            remap.remap_offset(dup_q),
            first_q,
            "uneven variant-name lengths must not skew the remapped column"
        );
    }

    #[test]
    fn block_remap_skips_real_adjacent_duplicate_impls() {
        // B15 fix: two genuinely-real, adjacent `impl A:` / `impl B:`
        // blocks with byte-identical bodies must NOT be remapped when no
        // block is flagged as distributed — a label in the SECOND block
        // stays put instead of collapsing onto the first.
        let sanitised = "impl A:\n\
                         \x20\x20\x20\x20def total(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.missing\n\
                         \n\
                         impl B:\n\
                         \x20\x20\x20\x20def total(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.missing\n";
        // Empty distributed set → no remap at all.
        assert!(
            BlockRemap::from_sanitised(sanitised, &[]).is_none(),
            "real adjacent impls with no distributed flag must not remap"
        );
    }

    #[test]
    fn distributed_impl_lines_flags_only_alias_distributions() {
        // The render-time recovery must flag the synthetic
        // `impl Leaf:` / `impl Node:` run produced from a sealed-union
        // alias, but leave a real, non-alias `impl Other:` block alone.
        let sanitised = "type Tree = Leaf | Node\n\
                         impl Leaf:\n\
                         \x20\x20\x20\x20def total(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.x\n\
                         impl Node:\n\
                         \x20\x20\x20\x20def total(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.x\n\
                         impl Other:\n\
                         \x20\x20\x20\x20def total(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.x\n";
        let flagged = distributed_impl_lines(sanitised);
        // `impl Leaf:` is line 1, `impl Node:` is line 4.
        assert_eq!(flagged, vec![1, 4], "only the Leaf/Node distribution");
    }

    #[test]
    fn distributed_impl_lines_empty_for_real_adjacent_impls() {
        // No `type _ = A | B` alias declared → no distribution → nothing
        // flagged, even though the two blocks are byte-identical.
        let sanitised = "impl A:\n\
                         \x20\x20\x20\x20def total(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.missing\n\
                         impl B:\n\
                         \x20\x20\x20\x20def total(self) -> int:\n\
                         \x20\x20\x20\x20\x20\x20\x20\x20return self.missing\n";
        assert!(
            distributed_impl_lines(sanitised).is_empty(),
            "no alias → no synthetic distribution flagged"
        );
    }

    #[test]
    fn sanitize_synthetic_source_blanks_q_scaffolding() {
        // Synthesised `?`-operator scaffolding becomes whitespace so
        // the user sees a clean listing — better than the compiler
        // internals leaking into a `result_error_mismatch` error.
        let raw =
            "def f() -> Result[int, str]:\n    if isinstance(__typhon_q_0__, __typhon_Err__):\n        return __typhon_q_0__\n";
        let out = sanitize_synthetic_source(raw);
        assert_eq!(out.len(), raw.len(), "byte length preserved");
        assert!(
            !out.contains("__typhon_q_"),
            "`?` scaffolding must be hidden, got:\n{out}"
        );
        assert!(
            out.contains("def f() -> Result[int, str]:"),
            "user-authored lines must survive, got:\n{out}"
        );
    }

    #[test]
    fn sanitize_synthetic_source_short_circuits_clean_text() {
        // A source with no `__typhon_*` markers is returned unchanged
        // (the function early-exits on the substring probe so we don't
        // pay a per-line walk on every diagnostic in clean projects).
        let raw = "let x: int = 1\nprint(x)\n";
        assert_eq!(sanitize_synthetic_source(raw), raw);
    }

    #[test]
    fn parse_error_hint_returns_none_for_unrelated_message() {
        // Unrecognised messages must leave the help slot empty so
        // miette skips the help block instead of printing nonsense.
        let source = "let x: int = \n";
        let hint = parse_error_hint("unexpected token", source, 0);
        assert!(
            hint.is_none(),
            "unrelated message should not produce a hint"
        );
    }

    #[test]
    fn type_mismatch_dict_literal_to_model_suggests_constructor() {
        // FINDINGS #38: assigning a dict literal to a model-typed binding
        // should redirect the user to the constructor form. The full
        // "missing fields: age, email" enumeration requires plumbing
        // from tyc-types and is tracked as a TODO in dict_literal_to_model_hint.
        let e = TycError::type_mismatch(
            "UserCreate",
            "dict[str, str]",
            "a.ty",
            "let u: UserCreate = {\"name\": \"Bob\"}",
            0,
            3,
        );
        let TycError::TypeMismatch { suggestion, .. } = &e else {
            panic!("expected TypeMismatch variant");
        };
        assert!(
            suggestion.contains("UserCreate(field1"),
            "model-literal hint must point at the constructor form, got: {suggestion}"
        );
    }

    #[test]
    fn type_mismatch_scalar_keeps_default_hint() {
        // Non-collection mismatches retain the original "widen to T | U"
        // help so we don't regress the documented suggestion for the
        // bulk of TypeMismatch sites.
        let e = TycError::type_mismatch("int", "str", "a.ty", "x", 0, 1);
        let TycError::TypeMismatch { suggestion, .. } = &e else {
            panic!("expected TypeMismatch variant");
        };
        assert!(
            suggestion.contains("int | str"),
            "default help should keep the union-widening suggestion, got: {suggestion}"
        );
    }

    #[test]
    fn nullable_use_contains_expected_type() {
        let e = TycError::nullable_use("x", "str", "a.ty", "val x: str? = None", 0, 1);
        assert!(matches!(e, TycError::NullableUse { .. }));
        // The diagnostic message embeds `expected`; the variable name appears
        // only in the source-span label rendered by miette, not in to_string().
        assert!(e.to_string().contains("str"));
    }

    #[test]
    fn wrong_arg_count_contains_name_and_counts() {
        let e = TycError::wrong_arg_count("f", 2, 3, "a.ty", "f(1, 2, 3)", 0, 9);
        assert!(matches!(e, TycError::WrongArgCount { .. }));
        let msg = e.to_string();
        assert!(msg.contains("f"));
        assert!(msg.contains('2'), "expected count should appear");
        assert!(msg.contains('3'), "actual count should appear");
    }

    #[test]
    fn wrong_arg_count_equal_counts_gets_kw_only_help() {
        // FINDINGS #36: when expected == actual, the headline alone is
        // self-contradictory ("expected 2, got 2"). The constructor
        // populates the help slot with a kw-only nudge so the message
        // makes sense.
        let e = TycError::wrong_arg_count("f", 2, 2, "a.ty", "f(1, 2)", 0, 7);
        let TycError::WrongArgCount { suggestion, .. } = &e else {
            panic!("expected WrongArgCount variant");
        };
        let help = suggestion
            .as_deref()
            .expect("equal-counts case must populate the help slot");
        assert!(
            help.contains("keyword-only"),
            "help should mention keyword-only, got: {help}"
        );
        assert!(help.contains("pass them by name"));
    }

    #[test]
    fn wrong_arg_count_unequal_counts_no_help() {
        // The kw-only hint must NOT fire on a genuine count mismatch
        // (otherwise users would see "pass them by name" for the
        // unrelated forgot-an-argument case).
        let e = TycError::wrong_arg_count("f", 2, 3, "a.ty", "f(1, 2, 3)", 0, 9);
        let TycError::WrongArgCount { suggestion, .. } = &e else {
            panic!("expected WrongArgCount variant");
        };
        assert!(
            suggestion.is_none(),
            "unequal-counts must NOT carry the kw-only hint; got: {suggestion:?}"
        );
    }

    #[test]
    fn not_callable_contains_type() {
        let e = TycError::not_callable("int", "a.ty", "1()", 0, 3);
        assert!(matches!(e, TycError::NotCallable { .. }));
        assert!(e.to_string().contains("int"));
    }

    #[test]
    fn non_exhaustive_match_contains_union_and_missing() {
        let e = TycError::non_exhaustive_match("Shape", "Circle", "a.ty", "match s:", 0, 7);
        assert!(matches!(e, TycError::NonExhaustiveMatch { .. }));
        let msg = e.to_string();
        assert!(msg.contains("Shape"));
        assert!(msg.contains("Circle"));
    }

    #[test]
    fn comptime_contains_name_and_message() {
        let e = TycError::comptime("PORT", "env var missing");
        assert!(matches!(e, TycError::Comptime { .. }));
        let msg = e.to_string();
        assert!(msg.contains("PORT"));
        assert!(msg.contains("env var missing"));
    }

    #[test]
    fn invalid_question_op_is_correct_variant() {
        let e = TycError::invalid_question_op("bad use", "a.ty", "x?", 0, 2);
        assert!(matches!(e, TycError::InvalidQuestionOp { .. }));
        assert!(e.to_string().contains("bad use"));
    }

    #[test]
    fn unused_import_contains_name() {
        let e = TycError::unused_import("os", "a.ty", "import os", 0, 9);
        assert!(matches!(e, TycError::UnusedImport { .. }));
        assert!(e.to_string().contains("os"));
    }

    #[test]
    fn lazy_usage_contains_message() {
        let e = TycError::lazy_usage("unsupported form", "a.ty", "lazy from x import y", 0, 20);
        assert!(matches!(e, TycError::LazyUsage { .. }));
        assert!(e.to_string().contains("unsupported form"));
    }

    #[test]
    fn impure_pure_fn_contains_name_and_reason() {
        let e = TycError::impure_pure_fn(
            "compute",
            "calls I/O",
            "a.ty",
            "@pure\ndef compute(): pass",
            0,
            5,
        );
        assert!(matches!(e, TycError::ImpurePureFn { .. }));
        let msg = e.to_string();
        assert!(msg.contains("compute"));
        assert!(msg.contains("calls I/O"));
    }

    #[test]
    fn interface_isinstance_contains_interface_name() {
        let e = TycError::interface_isinstance(
            "Serialisable",
            "a.ty",
            "isinstance(x, Serialisable)",
            0,
            26,
        );
        assert!(matches!(e, TycError::InterfaceIsinstance { .. }));
        assert!(e.to_string().contains("Serialisable"));
    }

    #[test]
    fn interface_not_conforming_contains_key_fields() {
        let e = TycError::interface_not_conforming(
            "Writer",
            "MyClass",
            "write",
            "a.ty",
            "x: Writer = MyClass()",
            0,
            20,
        );
        assert!(matches!(e, TycError::InterfaceNotConforming { .. }));
        let msg = e.to_string();
        assert!(msg.contains("Writer"));
        assert!(msg.contains("MyClass"));
        assert!(msg.contains("write"));
    }

    #[test]
    fn immutable_assign_contains_name() {
        let e = TycError::immutable_assign("x", "a.ty", "val x: int = 1\nx = 2", 4, 1, 15, 1);
        assert!(matches!(e, TycError::ImmutableAssign { .. }));
        assert!(e.to_string().contains("x"));
    }

    #[test]
    fn frozen_assign_contains_class_and_field() {
        let e = TycError::frozen_assign("Identity", "name", "a.ty", "i.name = \"Bob\"", 0, 6);
        assert!(matches!(e, TycError::FrozenAssign { .. }));
        let msg = e.to_string();
        assert!(msg.contains("Identity"));
        assert!(msg.contains("name"));
    }

    // ── Diagnostics collection API ────────────────────────────────────────────

    #[test]
    fn new_diagnostics_is_empty() {
        let d = Diagnostics::new();
        assert!(!d.has_errors());
        assert_eq!(d.error_count(), 0);
        assert_eq!(d.warning_count(), 0);
    }

    #[test]
    fn push_error_increments_error_count() {
        let mut d = Diagnostics::new();
        d.push_error(TycError::generic("e1"));
        assert!(d.has_errors());
        assert_eq!(d.error_count(), 1);
        assert_eq!(d.warning_count(), 0);
    }

    #[test]
    fn push_warning_increments_warning_count_not_error_count() {
        let mut d = Diagnostics::new();
        d.push_warning(TycError::generic("w1"));
        assert!(!d.has_errors());
        assert_eq!(d.error_count(), 0);
        assert_eq!(d.warning_count(), 1);
    }

    #[test]
    fn extend_merges_both_error_and_warning_lists() {
        let mut a = Diagnostics::new();
        a.push_error(TycError::generic("e1"));
        a.push_warning(TycError::generic("w1"));

        let mut b = Diagnostics::new();
        b.push_error(TycError::generic("e2"));
        b.push_warning(TycError::generic("w2"));

        a.extend(b);
        assert_eq!(a.error_count(), 2);
        assert_eq!(a.warning_count(), 2);
    }

    #[test]
    fn into_parts_separates_errors_and_warnings() {
        let mut d = Diagnostics::new();
        d.push_error(TycError::generic("err"));
        d.push_warning(TycError::generic("warn"));

        let (errors, warnings) = d.into_parts();
        assert_eq!(errors.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(errors[0].to_string().contains("err"));
        assert!(warnings[0].to_string().contains("warn"));
    }

    #[test]
    fn errors_and_warnings_slices_are_consistent_with_counts() {
        let mut d = Diagnostics::new();
        d.push_error(TycError::generic("e1"));
        d.push_error(TycError::generic("e2"));
        d.push_warning(TycError::generic("w1"));

        assert_eq!(d.errors().len(), d.error_count());
        assert_eq!(d.warnings().len(), d.warning_count());
    }

    // ── Diagnostic codes are stable ───────────────────────────────────────────

    #[test]
    fn error_codes_are_stable() {
        use miette::Diagnostic;

        let cases: &[(&str, TycError)] = &[
            ("tyc::generic", TycError::generic("x")),
            ("tyc::io", TycError::io("p", &std::io::Error::other("e"))),
            (
                "tyc::type_mismatch",
                TycError::type_mismatch("int", "str", "f.ty", "x", 0, 1),
            ),
            (
                "tyc::unknown_name",
                TycError::unknown_name("x", "f.ty", "x", 0, 1),
            ),
            (
                "tyc::arg_count",
                TycError::wrong_arg_count("f", 1, 2, "f.ty", "f(1,2)", 0, 1),
            ),
            ("tyc::comptime", TycError::comptime("X", "bad")),
        ];

        for (expected_code, err) in cases {
            let code = err
                .code()
                .expect("diagnostic should have a code")
                .to_string();
            assert_eq!(
                &code, expected_code,
                "code mismatch for {expected_code}: got {code}"
            );
        }
    }

    #[test]
    fn typevar_bound_violation_contains_all_three_names() {
        let e = TycError::typevar_bound_violation("T", "int", "Comparable", "f.ty", "f(1)", 0, 4);
        assert!(matches!(e, TycError::TypeVarBoundViolation { .. }));
        let msg = e.to_string();
        assert!(msg.contains('T'), "typevar name should appear");
        assert!(msg.contains("int"), "actual type should appear");
        assert!(msg.contains("Comparable"), "bound should appear");
    }

    #[test]
    fn typevar_bound_violation_code_is_stable() {
        use miette::Diagnostic;
        let e = TycError::typevar_bound_violation("T", "int", "C", "f.ty", "src", 0, 1);
        let code = e.code().unwrap().to_string();
        assert_eq!(code, "tyc::typevar_bound");
    }

    #[test]
    fn dedup_removes_identical_errors() {
        let mut d = Diagnostics::new();
        d.push_error(TycError::generic("same error"));
        d.push_error(TycError::generic("same error"));
        d.push_error(TycError::generic("different error"));
        d.dedup();
        assert_eq!(d.error_count(), 2, "dedup should remove the duplicate");
    }

    #[test]
    fn dedup_preserves_distinct_warnings() {
        let mut d = Diagnostics::new();
        d.push_warning(TycError::generic("warn a"));
        d.push_warning(TycError::generic("warn b"));
        d.dedup();
        assert_eq!(
            d.warning_count(),
            2,
            "distinct warnings should both survive dedup"
        );
    }

    // ── Doc URLs are wired to every variant ───────────────────────────────────

    #[test]
    fn immutable_assign_has_doc_url() {
        use miette::Diagnostic;
        let e = TycError::immutable_assign("x", "a.ty", "let x = 1\nx = 2", 4, 1, 10, 1);
        let url = e.url().expect("expected a documentation URL").to_string();
        assert!(url.contains("immutable_assign"), "got {url}");
        assert!(url.starts_with("https://"), "got {url}");
    }

    // ── New Phase 5 variants ──────────────────────────────────────────────────

    #[test]
    fn invalid_config_value_contains_key_value_and_allowed() {
        let e = TycError::invalid_config_value(
            "emit.class-default",
            "plain",
            "`dataclass` | `frozen`",
            "typhon.toml",
        );
        assert!(matches!(e, TycError::InvalidConfigValue { .. }));
        let msg = e.to_string();
        assert!(msg.contains("plain"), "value should appear");
        assert!(msg.contains("emit.class-default"), "key should appear");
        assert!(msg.contains("typhon.toml"), "path should appear");
        assert!(msg.contains("dataclass"), "allowed list should appear");
    }

    #[test]
    fn orphan_py_import_contains_import_path() {
        let e =
            TycError::orphan_py_import(".helper", "src/main.ty", "from .helper import foo", 0, 17);
        assert!(matches!(e, TycError::OrphanPyImport { .. }));
        assert!(e.to_string().contains(".helper"));
    }

    #[test]
    fn python_semantic_drift_contains_expression_and_detail() {
        let e = TycError::python_semantic_drift(
            "1 + True",
            "bool is a subtype of int in CPython",
            "a.ty",
            "1 + True",
            0,
            8,
        );
        assert!(matches!(e, TycError::PythonSemanticDrift { .. }));
        let msg = e.to_string();
        assert!(msg.contains("1 + True"));
        assert!(msg.contains("bool is a subtype"));
    }

    #[test]
    fn contains_secret_literal_contains_name_and_env_key() {
        let e = TycError::contains_secret_literal("API_KEY", "MY_API_KEY");
        assert!(matches!(e, TycError::ContainsSecretLiteral { .. }));
        let msg = e.to_string();
        assert!(msg.contains("API_KEY"), "binding name should appear");
    }

    #[test]
    fn new_variant_codes_are_stable() {
        use miette::Diagnostic;
        let cases: &[(&str, TycError)] = &[
            (
                "tyc::invalid_config_value",
                TycError::invalid_config_value("k", "v", "a|b", "typhon.toml"),
            ),
            (
                "tyc::orphan_py_import",
                TycError::orphan_py_import(".x", "a.ty", "from .x import y", 0, 1),
            ),
            (
                "tyc::python_semantic_drift",
                TycError::python_semantic_drift("e", "d", "a.ty", "e", 0, 1),
            ),
            (
                "tyc::contains_secret_literal",
                TycError::contains_secret_literal("API_KEY", "API_KEY"),
            ),
        ];
        for (expected_code, err) in cases {
            let code = err.code().unwrap().to_string();
            assert_eq!(&code, expected_code, "code mismatch: got {code}");
        }
    }
}
