//! Hand-curated completion stubs for the most commonly-imported stdlib
//! modules. Used by the LSP's member-completion path: when the user types
//! `os.` after `import os`, we look up `"os"` here and surface a
//! `CompletionItem` per entry.
//!
//! This is a deliberately small, opinionated subset — the top names per
//! module rather than the full surface — so that **a fresh tyc install
//! produces useful autocomplete out of the box** without shelling to
//! Python or shipping the full typeshed corpus. A future pass can drive
//! the same data from `.pyi` ingestion (issue tracked in
//! `docs/roadmap.md`); the call shape inside the LSP doesn't change.
//!
//! Entries are listed in roughly "most-used first" order. The kind hint
//! (function / class / constant) helps editors render the right icon and
//! drives the `detail` line shown in the completion popup. Signatures
//! are short on purpose — the LSP renders them as the `detail` text in
//! the popup, and long lines are truncated by every client.

use tower_lsp_server::ls_types::CompletionItemKind;

/// One member surfaced for a module.
#[derive(Debug, Clone, Copy)]
pub struct StubMember {
    pub name: &'static str,
    pub kind: CompletionItemKind,
    /// One-line signature, e.g. `"open(file, mode='r') -> IO"`. Rendered
    /// as the `detail` text in the popup. `None` for plain constants.
    pub signature: Option<&'static str>,
    /// One-sentence docstring. Rendered as the popup's `documentation`.
    pub documentation: Option<&'static str>,
}

/// One module entry — `name` is the dotted Python module path. Submodules
/// (e.g. `os.path`) are recorded as separate entries; the lookup helper
/// joins receiver dotted-names before searching.
#[derive(Debug)]
pub struct ModuleStub {
    pub module: &'static str,
    pub members: &'static [StubMember],
}

/// Look up the stub for a dotted module path, returning the member list
/// if known. Returns `None` for any module we haven't curated — the
/// caller falls back to bindings-only completion in that case.
pub fn lookup(module: &str) -> Option<&'static [StubMember]> {
    STUBS
        .iter()
        .find(|s| s.module == module)
        .map(|s| s.members)
}

/// Return true when `name` looks like the receiver of a member access
/// that *might* resolve to a curated stub. Used by tests to make sure
/// new stub additions are picked up; production code goes through
/// [`lookup`].
#[cfg(test)]
pub fn known_modules() -> impl Iterator<Item = &'static str> {
    STUBS.iter().map(|s| s.module)
}

// Macro to keep the per-member entries terse. Most members carry a kind
// + signature; the documentation slot is filled selectively for names
// where a one-liner adds genuine value.
macro_rules! func {
    ($name:literal, $sig:literal) => {
        StubMember {
            name: $name,
            kind: CompletionItemKind::FUNCTION,
            signature: Some($sig),
            documentation: None,
        }
    };
    ($name:literal, $sig:literal, $doc:literal) => {
        StubMember {
            name: $name,
            kind: CompletionItemKind::FUNCTION,
            signature: Some($sig),
            documentation: Some($doc),
        }
    };
}

macro_rules! class {
    ($name:literal, $sig:literal) => {
        StubMember {
            name: $name,
            kind: CompletionItemKind::CLASS,
            signature: Some($sig),
            documentation: None,
        }
    };
    ($name:literal, $sig:literal, $doc:literal) => {
        StubMember {
            name: $name,
            kind: CompletionItemKind::CLASS,
            signature: Some($sig),
            documentation: Some($doc),
        }
    };
}

macro_rules! konst {
    ($name:literal, $sig:literal) => {
        StubMember {
            name: $name,
            kind: CompletionItemKind::CONSTANT,
            signature: Some($sig),
            documentation: None,
        }
    };
}

macro_rules! module {
    ($name:literal) => {
        StubMember {
            name: $name,
            kind: CompletionItemKind::MODULE,
            signature: None,
            documentation: None,
        }
    };
}

/// The full set of curated module stubs. Order doesn't matter — `lookup`
/// is a linear scan, which is fine for ~20 entries; the const slice keeps
/// the lookup table in `.rodata` with zero startup cost.
static STUBS: &[ModuleStub] = &[
    ModuleStub {
        module: "os",
        members: &[
            func!(
                "getcwd",
                "getcwd() -> str",
                "Return the current working directory."
            ),
            func!("chdir", "chdir(path: str) -> None"),
            func!("listdir", "listdir(path: str = '.') -> list[str]"),
            func!("makedirs", "makedirs(path: str, exist_ok: bool = False) -> None"),
            func!("mkdir", "mkdir(path: str) -> None"),
            func!("remove", "remove(path: str) -> None"),
            func!("rename", "rename(src: str, dst: str) -> None"),
            func!("rmdir", "rmdir(path: str) -> None"),
            func!("getenv", "getenv(key: str, default: str | None = None) -> str | None"),
            func!("system", "system(command: str) -> int"),
            func!(
                "walk",
                "walk(top: str) -> Iterator[tuple[str, list[str], list[str]]]"
            ),
            module!("path"),
            konst!("sep", "sep: str"),
            konst!("linesep", "linesep: str"),
            konst!("environ", "environ: Mapping[str, str]"),
            konst!("name", "name: str"),
            konst!("devnull", "devnull: str"),
        ],
    },
    ModuleStub {
        module: "os.path",
        members: &[
            func!("join", "join(*paths: str) -> str"),
            func!("exists", "exists(path: str) -> bool"),
            func!("isfile", "isfile(path: str) -> bool"),
            func!("isdir", "isdir(path: str) -> bool"),
            func!("dirname", "dirname(path: str) -> str"),
            func!("basename", "basename(path: str) -> str"),
            func!("splitext", "splitext(path: str) -> tuple[str, str]"),
            func!("abspath", "abspath(path: str) -> str"),
            func!("relpath", "relpath(path: str, start: str = '.') -> str"),
            func!("expanduser", "expanduser(path: str) -> str"),
            func!("expandvars", "expandvars(path: str) -> str"),
            func!("normpath", "normpath(path: str) -> str"),
            func!("realpath", "realpath(path: str) -> str"),
            func!("getsize", "getsize(path: str) -> int"),
        ],
    },
    ModuleStub {
        module: "sys",
        members: &[
            konst!("argv", "argv: list[str]"),
            konst!("path", "path: list[str]"),
            konst!("platform", "platform: str"),
            konst!("version", "version: str"),
            konst!("version_info", "version_info: tuple[int, int, int, str, int]"),
            konst!("stdin", "stdin: TextIO"),
            konst!("stdout", "stdout: TextIO"),
            konst!("stderr", "stderr: TextIO"),
            konst!("maxsize", "maxsize: int"),
            konst!("modules", "modules: dict[str, ModuleType]"),
            func!("exit", "exit(code: int = 0) -> NoReturn"),
            func!(
                "getrecursionlimit",
                "getrecursionlimit() -> int"
            ),
            func!("setrecursionlimit", "setrecursionlimit(limit: int) -> None"),
            func!("getsizeof", "getsizeof(obj: object) -> int"),
        ],
    },
    ModuleStub {
        module: "json",
        members: &[
            func!(
                "loads",
                "loads(s: str) -> Any",
                "Decode a JSON document from a string."
            ),
            func!(
                "load",
                "load(fp: IO) -> Any",
                "Decode a JSON document from a readable file-like object."
            ),
            func!(
                "dumps",
                "dumps(obj: Any, *, indent: int | None = None) -> str",
                "Serialize `obj` to a JSON string."
            ),
            func!(
                "dump",
                "dump(obj: Any, fp: IO, *, indent: int | None = None) -> None",
                "Serialize `obj` as JSON to a writable file-like object."
            ),
            class!("JSONDecoder", "class JSONDecoder"),
            class!("JSONEncoder", "class JSONEncoder"),
            class!("JSONDecodeError", "class JSONDecodeError(ValueError)"),
        ],
    },
    ModuleStub {
        module: "math",
        members: &[
            konst!("pi", "pi: float"),
            konst!("e", "e: float"),
            konst!("tau", "tau: float"),
            konst!("inf", "inf: float"),
            konst!("nan", "nan: float"),
            func!("sqrt", "sqrt(x: float) -> float"),
            func!("pow", "pow(x: float, y: float) -> float"),
            func!("exp", "exp(x: float) -> float"),
            func!("log", "log(x: float, base: float = e) -> float"),
            func!("log2", "log2(x: float) -> float"),
            func!("log10", "log10(x: float) -> float"),
            func!("floor", "floor(x: float) -> int"),
            func!("ceil", "ceil(x: float) -> int"),
            func!("trunc", "trunc(x: float) -> int"),
            func!("fabs", "fabs(x: float) -> float"),
            func!("isnan", "isnan(x: float) -> bool"),
            func!("isinf", "isinf(x: float) -> bool"),
            func!("isclose", "isclose(a: float, b: float, *, rel_tol: float = 1e-9, abs_tol: float = 0.0) -> bool"),
            func!("sin", "sin(x: float) -> float"),
            func!("cos", "cos(x: float) -> float"),
            func!("tan", "tan(x: float) -> float"),
            func!("atan2", "atan2(y: float, x: float) -> float"),
            func!("gcd", "gcd(*integers: int) -> int"),
            func!("lcm", "lcm(*integers: int) -> int"),
            func!("factorial", "factorial(n: int) -> int"),
        ],
    },
    ModuleStub {
        module: "re",
        members: &[
            func!(
                "match",
                "match(pattern: str, string: str, flags: int = 0) -> Match | None",
                "Try to apply `pattern` at the start of `string`."
            ),
            func!(
                "search",
                "search(pattern: str, string: str, flags: int = 0) -> Match | None",
                "Scan `string` for the first match of `pattern`."
            ),
            func!(
                "fullmatch",
                "fullmatch(pattern: str, string: str, flags: int = 0) -> Match | None"
            ),
            func!(
                "findall",
                "findall(pattern: str, string: str, flags: int = 0) -> list[str]"
            ),
            func!(
                "finditer",
                "finditer(pattern: str, string: str, flags: int = 0) -> Iterator[Match]"
            ),
            func!(
                "sub",
                "sub(pattern: str, repl: str, string: str, count: int = 0, flags: int = 0) -> str"
            ),
            func!(
                "subn",
                "subn(pattern: str, repl: str, string: str, count: int = 0, flags: int = 0) -> tuple[str, int]"
            ),
            func!(
                "split",
                "split(pattern: str, string: str, maxsplit: int = 0, flags: int = 0) -> list[str]"
            ),
            func!(
                "escape",
                "escape(pattern: str) -> str",
                "Return `pattern` with all regex metacharacters backslash-escaped."
            ),
            func!(
                "compile",
                "compile(pattern: str, flags: int = 0) -> Pattern"
            ),
            class!("Pattern", "class Pattern"),
            class!("Match", "class Match"),
            konst!("IGNORECASE", "IGNORECASE: int"),
            konst!("MULTILINE", "MULTILINE: int"),
            konst!("DOTALL", "DOTALL: int"),
            konst!("VERBOSE", "VERBOSE: int"),
            konst!("ASCII", "ASCII: int"),
        ],
    },
    ModuleStub {
        module: "pathlib",
        members: &[
            class!(
                "Path",
                "class Path(*pathsegments)",
                "Object-oriented filesystem path."
            ),
            class!("PurePath", "class PurePath(*pathsegments)"),
            class!("PurePosixPath", "class PurePosixPath(*pathsegments)"),
            class!("PureWindowsPath", "class PureWindowsPath(*pathsegments)"),
            class!("PosixPath", "class PosixPath(*pathsegments)"),
            class!("WindowsPath", "class WindowsPath(*pathsegments)"),
        ],
    },
    ModuleStub {
        module: "datetime",
        members: &[
            class!("date", "class date(year: int, month: int, day: int)"),
            class!(
                "time",
                "class time(hour: int = 0, minute: int = 0, second: int = 0, microsecond: int = 0)"
            ),
            class!(
                "datetime",
                "class datetime(year, month, day, hour=0, minute=0, second=0, microsecond=0, tzinfo=None)"
            ),
            class!(
                "timedelta",
                "class timedelta(days=0, seconds=0, microseconds=0)"
            ),
            class!("timezone", "class timezone(offset: timedelta, name: str = '')"),
            class!("tzinfo", "class tzinfo"),
            konst!("MINYEAR", "MINYEAR: int"),
            konst!("MAXYEAR", "MAXYEAR: int"),
            konst!("UTC", "UTC: timezone"),
        ],
    },
    ModuleStub {
        module: "collections",
        members: &[
            class!(
                "deque",
                "class deque(iterable: Iterable = (), maxlen: int | None = None)"
            ),
            class!("Counter", "class Counter(iterable: Iterable = ())"),
            class!(
                "defaultdict",
                "class defaultdict(default_factory: Callable | None = None, **kwargs)"
            ),
            class!("OrderedDict", "class OrderedDict"),
            class!("ChainMap", "class ChainMap(*maps: Mapping)"),
            func!(
                "namedtuple",
                "namedtuple(typename: str, field_names: Iterable[str]) -> type"
            ),
            module!("abc"),
        ],
    },
    ModuleStub {
        module: "itertools",
        members: &[
            func!("chain", "chain(*iterables: Iterable[T]) -> Iterator[T]"),
            func!("count", "count(start: int = 0, step: int = 1) -> Iterator[int]"),
            func!("cycle", "cycle(iterable: Iterable[T]) -> Iterator[T]"),
            func!("repeat", "repeat(object: T, times: int | None = None) -> Iterator[T]"),
            func!(
                "accumulate",
                "accumulate(iterable: Iterable[T], func: Callable = operator.add) -> Iterator[T]"
            ),
            func!(
                "combinations",
                "combinations(iterable: Iterable[T], r: int) -> Iterator[tuple[T, ...]]"
            ),
            func!(
                "combinations_with_replacement",
                "combinations_with_replacement(iterable: Iterable[T], r: int) -> Iterator[tuple[T, ...]]"
            ),
            func!(
                "permutations",
                "permutations(iterable: Iterable[T], r: int | None = None) -> Iterator[tuple[T, ...]]"
            ),
            func!(
                "product",
                "product(*iterables: Iterable, repeat: int = 1) -> Iterator[tuple]"
            ),
            func!(
                "groupby",
                "groupby(iterable: Iterable[T], key: Callable | None = None) -> Iterator[tuple[K, Iterator[T]]]"
            ),
            func!("islice", "islice(iterable: Iterable[T], *args: int) -> Iterator[T]"),
            func!(
                "takewhile",
                "takewhile(predicate: Callable, iterable: Iterable[T]) -> Iterator[T]"
            ),
            func!(
                "dropwhile",
                "dropwhile(predicate: Callable, iterable: Iterable[T]) -> Iterator[T]"
            ),
            func!(
                "filterfalse",
                "filterfalse(predicate: Callable, iterable: Iterable[T]) -> Iterator[T]"
            ),
            func!(
                "starmap",
                "starmap(function: Callable, iterable: Iterable[tuple]) -> Iterator"
            ),
            func!(
                "tee",
                "tee(iterable: Iterable[T], n: int = 2) -> tuple[Iterator[T], ...]"
            ),
            func!(
                "zip_longest",
                "zip_longest(*iterables: Iterable, fillvalue: object = None) -> Iterator[tuple]"
            ),
            func!("pairwise", "pairwise(iterable: Iterable[T]) -> Iterator[tuple[T, T]]"),
        ],
    },
    ModuleStub {
        module: "functools",
        members: &[
            func!(
                "cache",
                "cache(user_function: Callable) -> Callable",
                "Unbounded memoising decorator (Python 3.9+)."
            ),
            func!(
                "lru_cache",
                "lru_cache(maxsize: int | None = 128, typed: bool = False) -> Callable"
            ),
            func!(
                "partial",
                "partial(func: Callable, *args, **kwargs) -> Callable"
            ),
            func!(
                "partialmethod",
                "partialmethod(func: Callable, *args, **kwargs)"
            ),
            func!(
                "reduce",
                "reduce(function: Callable, iterable: Iterable, initializer: T = ...) -> T"
            ),
            func!("wraps", "wraps(wrapped: Callable) -> Callable"),
            class!("cached_property", "class cached_property(func: Callable)"),
            func!(
                "singledispatch",
                "singledispatch(func: Callable) -> Callable"
            ),
            func!(
                "singledispatchmethod",
                "singledispatchmethod(func: Callable) -> Callable"
            ),
            func!(
                "total_ordering",
                "total_ordering(cls: type) -> type",
                "Class decorator that fills in missing rich-comparison methods."
            ),
            func!(
                "cmp_to_key",
                "cmp_to_key(func: Callable) -> Callable"
            ),
        ],
    },
    ModuleStub {
        module: "typing",
        members: &[
            // Typhon explicitly rejects `TypeVar` imports — surface a hint
            // anyway so the user sees the diagnostic instead of confusing
            // silent-failure if they happen to type `typing.Type`.
            class!("Any", "Any"),
            class!("Optional", "Optional[T]"),
            class!("Union", "Union[A, B, ...]"),
            class!("Callable", "Callable[[...], R]"),
            class!("Iterator", "Iterator[T]"),
            class!("Iterable", "Iterable[T]"),
            class!("Generator", "Generator[Y, S, R]"),
            class!("AsyncIterator", "AsyncIterator[T]"),
            class!("AsyncIterable", "AsyncIterable[T]"),
            class!("AsyncGenerator", "AsyncGenerator[Y, S]"),
            class!("Awaitable", "Awaitable[T]"),
            class!("Coroutine", "Coroutine[Y, S, R]"),
            class!("Mapping", "Mapping[K, V]"),
            class!("Sequence", "Sequence[T]"),
            class!("Protocol", "class X(Protocol): ..."),
            class!("TypedDict", "class X(TypedDict): ..."),
            class!("NamedTuple", "class X(NamedTuple): ..."),
            class!("Literal", "Literal[...]"),
            class!("Final", "Final[T]"),
            class!("ClassVar", "ClassVar[T]"),
            class!("cast", "cast(typ: type[T], val: object) -> T"),
            func!("get_type_hints", "get_type_hints(obj: Any) -> dict[str, Any]"),
            func!("overload", "overload(func: Callable) -> Callable"),
            func!(
                "runtime_checkable",
                "runtime_checkable(cls: type) -> type",
                "Mark a `Protocol` as supporting runtime `isinstance` checks."
            ),
        ],
    },
    ModuleStub {
        module: "asyncio",
        members: &[
            func!(
                "run",
                "run(coro: Coroutine, *, debug: bool = False) -> Any",
                "Run a coroutine until it completes; entry point for async programs."
            ),
            func!(
                "sleep",
                "sleep(delay: float, result: T | None = None) -> Coroutine"
            ),
            func!(
                "gather",
                "gather(*aws: Awaitable, return_exceptions: bool = False) -> Coroutine"
            ),
            func!("wait", "wait(aws: Iterable[Awaitable], *, timeout: float | None = None) -> Coroutine"),
            func!(
                "wait_for",
                "wait_for(aw: Awaitable, timeout: float) -> Coroutine"
            ),
            func!(
                "create_task",
                "create_task(coro: Coroutine, *, name: str | None = None) -> Task"
            ),
            func!("ensure_future", "ensure_future(obj: Awaitable) -> Task | Future"),
            func!("current_task", "current_task() -> Task | None"),
            func!("all_tasks", "all_tasks() -> set[Task]"),
            func!("get_event_loop", "get_event_loop() -> AbstractEventLoop"),
            func!("new_event_loop", "new_event_loop() -> AbstractEventLoop"),
            func!("get_running_loop", "get_running_loop() -> AbstractEventLoop"),
            class!("Task", "class Task[T]"),
            class!("Future", "class Future[T]"),
            class!("TaskGroup", "class TaskGroup", "Structured-concurrency context manager (3.11+)."),
            class!("Lock", "class Lock"),
            class!("Event", "class Event"),
            class!("Queue", "class Queue[T](maxsize: int = 0)"),
            class!("Semaphore", "class Semaphore(value: int = 1)"),
            class!("TimeoutError", "class TimeoutError(Exception)"),
            class!("CancelledError", "class CancelledError(BaseException)"),
        ],
    },
    ModuleStub {
        module: "logging",
        members: &[
            func!("debug", "debug(msg: str, *args, **kwargs) -> None"),
            func!("info", "info(msg: str, *args, **kwargs) -> None"),
            func!("warning", "warning(msg: str, *args, **kwargs) -> None"),
            func!("error", "error(msg: str, *args, **kwargs) -> None"),
            func!("critical", "critical(msg: str, *args, **kwargs) -> None"),
            func!("exception", "exception(msg: str, *args, **kwargs) -> None"),
            func!(
                "basicConfig",
                "basicConfig(*, level: int = WARNING, format: str | None = None, **kwargs) -> None"
            ),
            func!("getLogger", "getLogger(name: str | None = None) -> Logger"),
            class!("Logger", "class Logger"),
            class!("Handler", "class Handler"),
            class!("Formatter", "class Formatter(fmt: str | None = None)"),
            class!("StreamHandler", "class StreamHandler(stream: IO | None = None)"),
            class!("FileHandler", "class FileHandler(filename: str, mode: str = 'a')"),
            konst!("DEBUG", "DEBUG: int"),
            konst!("INFO", "INFO: int"),
            konst!("WARNING", "WARNING: int"),
            konst!("ERROR", "ERROR: int"),
            konst!("CRITICAL", "CRITICAL: int"),
            konst!("NOTSET", "NOTSET: int"),
        ],
    },
    ModuleStub {
        module: "dataclasses",
        members: &[
            func!(
                "dataclass",
                "dataclass(*, init=True, repr=True, eq=True, order=False, frozen=False, slots=False) -> Callable"
            ),
            func!(
                "field",
                "field(*, default=MISSING, default_factory=MISSING, init=True, repr=True, hash=None, compare=True) -> Field"
            ),
            func!("fields", "fields(class_or_instance) -> tuple[Field, ...]"),
            func!("asdict", "asdict(instance) -> dict[str, Any]"),
            func!("astuple", "astuple(instance) -> tuple"),
            func!("is_dataclass", "is_dataclass(obj) -> bool"),
            func!("replace", "replace(instance, **changes) -> Any"),
            class!("Field", "class Field"),
            konst!("MISSING", "MISSING: object"),
        ],
    },
];


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_lookup_returns_path_module_entry() {
        // Sanity: `os.path` is reachable both as a member of `os` (so
        // `os.<TAB>` surfaces it) and as a stand-alone module
        // (`os.path.<TAB>` surfaces its members).
        let os = lookup("os").expect("os stub");
        assert!(os.iter().any(|m| m.name == "path"));
        let os_path = lookup("os.path").expect("os.path stub");
        assert!(os_path.iter().any(|m| m.name == "join"));
    }

    #[test]
    fn unknown_module_returns_none() {
        assert!(lookup("definitely_not_a_real_module").is_none());
    }

    #[test]
    fn every_curated_module_has_at_least_one_member() {
        for name in known_modules() {
            let members = lookup(name).unwrap_or_else(|| panic!("missing members for {name}"));
            assert!(!members.is_empty(), "{name} stub is empty");
        }
    }

}
