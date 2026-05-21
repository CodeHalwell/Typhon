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
    #[error("parse error in '{path}'")]
    #[diagnostic(code(tyc::parse), url("https://typhon.dev/lang/diagnostics/parse"))]
    Parse {
        path: String,
        message: String,
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
    #[error("type mismatch: expected `{expected}`, found `{actual}`")]
    #[diagnostic(
        code(tyc::type_mismatch),
        url("https://typhon.dev/lang/diagnostics/type_mismatch"),
        help("change the value, or update the annotation to `{actual}`")
    )]
    TypeMismatch {
        expected: String,
        actual: String,
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
    #[error("wrong number of arguments to `{name}`: expected {expected}, got {actual}")]
    #[diagnostic(
        code(tyc::arg_count),
        url("https://typhon.dev/lang/diagnostics/arg_count")
    )]
    WrongArgCount {
        name: String,
        expected: usize,
        actual: usize,
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

    /// The `?` error-propagation operator was used outside a `Result`-returning function.
    #[error("{message}")]
    #[diagnostic(
        code(tyc::invalid_question_op),
        url("https://typhon.dev/lang/diagnostics/invalid_question_op"),
        help("the `?` operator is only valid inside a function returning `Result[T, E]`")
    )]
    InvalidQuestionOp {
        message: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("invalid use of `?` here")]
        span: SourceSpan,
    },

    /// An imported name is never used in the module.
    #[error("imported name '{name}' is never used")]
    #[diagnostic(
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
        help("annotate each field as `ClassVar[T]` (from `typing`) so `@dataclass(slots=True)` excludes them from `__slots__`")
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
    #[error("`{what}` on `{function}` is missing a type annotation")]
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
    /// lazily, but no caller can ever resolve the type. FINDINGS #81.
    #[error("type alias `{name}` is part of a cycle")]
    #[diagnostic(
        code(tyc::cyclic_type_alias),
        url("https://typhon.dev/lang/diagnostics/cyclic_type_alias"),
        help("break the cycle by pointing at a concrete type or removing one of the alias declarations")
    )]
    CyclicTypeAlias {
        name: String,
        #[source_code]
        src: NamedSource<String>,
        #[label("alias here is in a cycle")]
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
    pub fn parse(
        path: impl Into<String>,
        source: impl Into<String>,
        message: impl Into<String>,
        offset: usize,
    ) -> Self {
        let path = path.into();
        let source = source.into();
        let message = message.into();
        let span = SourceSpan::new(SourceOffset::from(offset), 0usize);
        Self::Parse {
            src: NamedSource::new(path.clone(), source),
            path,
            message,
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
    pub fn type_mismatch(
        expected: impl Into<String>,
        actual: impl Into<String>,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::TypeMismatch {
            expected: expected.into(),
            actual: actual.into(),
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
    pub fn wrong_arg_count(
        name: impl Into<String>,
        expected: usize,
        actual: usize,
        path: impl Into<String>,
        source: impl Into<String>,
        offset: usize,
        length: usize,
    ) -> Self {
        Self::WrongArgCount {
            name: name.into(),
            expected,
            actual,
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
