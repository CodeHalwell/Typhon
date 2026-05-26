# Typhon Cookbook — Canonical Patterns

Extracted from `/home/user/Typhon/examples/`. Each pattern is a near-verbatim snippet from a real, type-checked example. When in doubt about how to write something idiomatically, find the closest pattern here and copy from it.

The repo ships 68 stdlib-only exercises (`examples/01..68-*/`) plus 15 production-shaped multi-file apps (`examples/apps/01..15-*/`). Every `.ty` file ships with a paired `.py` showing the lowering.

---

## 1. Hello world (`examples/01-hello-world/hello.ty`)

```python
import sys


def main() -> None:
    let name: str = sys.argv[1] if len(sys.argv) > 1 else "world"
    print(f"Hello, {name}!")


if __name__ == "__main__":
    main()
```

Every function carries full type annotations including `-> None`. Locals use `let`.

---

## 2. `let` vs `mut` + nullable narrowing (`examples/02-variables-and-types/`)

```python
def demo_mutability() -> None:
    let pi: float = 3.14159
    mut counter: int = 0

    counter = counter + 1
    counter = counter * 2

def demo_nullable() -> None:
    let maybe_name: str? = lookup_name(1)
    if maybe_name is None:
        print("anonymous")
        return
    print(f"hi, {maybe_name}")  # narrowed to str

def lookup_name(id: int) -> str?:
    if id == 1:
        return "Ada"
    return None
```

---

## 3. Control flow / comprehensions (`examples/03-control-flow/`)

```python
def sum_evens(xs: list[int]) -> int:
    mut total: int = 0
    for x in xs:
        if x % 2 != 0:
            continue
        total = total + x
    return total


def squares_up_to(n: int) -> list[int]:
    return [i * i for i in range(n)]


def word_lengths(words: list[str]) -> dict[str, int]:
    return {w: len(w) for w in words}
```

---

## 4. Collections / dict.get / tuple destructure (`examples/04-collections/`)

```python
def demo_dicts() -> None:
    let prices: dict[str, float] = {"apple": 0.30, "banana": 0.15}
    for fruit, price in prices.items():
        print(f"{fruit:10s} ${price:.2f}")

    let cherry_price: float? = prices.get("cherry")
    if cherry_price is not None:
        print(f"cherry costs {cherry_price}")

def demo_tuples() -> None:
    let point: tuple[float, float] = (3.0, 4.0)
    let (x, y) = point
    print(f"distance: {(x * x + y * y) ** 0.5}")
```

`dict.get(k)` returns `V?`. Either narrow or use `d[k]` (typed `V`, may raise).

---

## 5. PEP 695 generics + Callable + closures (`examples/05-functions-and-generics/`)

```python
def first[T](xs: list[T]) -> T?:
    if len(xs) == 0:
        return None
    return xs[0]


def map_list[T, U](xs: list[T], f: Callable[[T], U]) -> list[U]:
    return [f(x) for x in xs]


def make_multiplier(factor: int) -> Callable[[int], int]:
    def inner(n: int) -> int:
        return n * factor
    return inner
```

Type parameters live inline on `def`/`class`/`type` via `[T, U]`. Never `from typing import TypeVar`.

---

## 6. Classes / `class frozen` / `model` / `impl` / `extend` (`examples/06-classes-and-models/`)

```python
class User:
    id: int
    name: str
    email: str


impl User:
    def display(self) -> str:
        return f"{self.name} <{self.email}> (#{self.id})"


class Point frozen:
    x: float
    y: float


impl Point:
    def translated(self, dx: float, dy: float) -> Point:
        return Point(x=self.x + dx, y=self.y + dy)


model ApiUser:
    id: int
    name: str
    email: str
    age: int? = None


extend str:
    def slug(self) -> str:
        return self.lower().replace(" ", "-")
```

`class frozen` for immutability; `impl X:` separates methods from data; `model` for boundary types; `extend builtin:` adds methods without monkey-patching.

---

## 7. Result with `?` + with-chain (`examples/07-error-handling/`)

```python
class ParseError:
    field: str
    reason: str


def parse_port(raw: str) -> Result[int, ParseError]:
    if not raw.isdigit():
        return Err(ParseError(field="port", reason=f"not a number: {raw}"))
    let n: int = int(raw)
    if n < 1 or n > 65535:
        return Err(ParseError(field="port", reason=f"out of range: {n}"))
    return Ok(n)


def parse_addr(host_raw: str, port_raw: str) -> Result[tuple[str, int], ParseError]:
    with host = parse_host(host_raw)?,
         port = parse_port(port_raw)?:
        return Ok((host, port))
    else err:
        print(f"failed parsing {err.field}: {err.reason}")
        return Err(err)


def parse_addr_short(host_raw: str, port_raw: str) -> Result[tuple[str, int], ParseError]:
    let host: str = parse_host(host_raw)?
    let port: int = parse_port(port_raw)?
    return Ok((host, port))
```

`?` short-circuits like Rust. `with a = r1?, b = r2?: ... else err: ...` chains multiple fallibles with a typed error binding.

### Result combinators (v0.6.0)

For heterogeneous error pipelines, lean on `.map_err` / `.map` / `.and_then` / `.or_else`:

```python
let toks: Tokens   = tokenize(src).map_err(_lex_to_pipeline)?
let ast:  Ast      = parse(toks).map_err(_parse_to_pipeline)?
let ty:   TypedAst = check(ast).map_err(_type_to_pipeline)?
```

---

## 8. Sealed union + exhaustive match (`examples/08-sealed-unions-match/`)

```python
type Shape = Circle | Rectangle | Triangle


class Circle:    radius: float
class Rectangle: width: float; height: float
class Triangle:  base: float;  height: float


def area(s: Shape) -> float:
    match s:
        case Circle(radius):
            return 3.14159 * radius * radius
        case Rectangle(width, height):
            return width * height
        case Triangle(base, height):
            return 0.5 * base * height
```

Add a variant to `type Shape`; every match becomes `tyc::non_exhaustive_match` until you handle it.

### Distributed `impl` on the alias (v0.6.0)

```python
type Event = TaskStarted | TaskFinished | TaskFailed

class TaskStarted:  task_id: int
class TaskFinished: task_id: int; output: str
class TaskFailed:   task_id: int; reason: str


impl Event:
    def task_id(self) -> int:
        match self:
            case TaskStarted(tid): return tid
            case TaskFinished(tid, _): return tid
            case TaskFailed(tid, _): return tid
```

The desugar pass replicates the method onto every variant.

### Keyword patterns for many-field variants (v0.6.0)

```python
match event:
    case TaskFinished(task_id=tid, output=out):
        print(f"#{tid} → {out}")
    case TaskFailed(task_id=tid, reason=r):
        print(f"#{tid} failed: {r}")
```

Survives field additions; recommended for variants with ≥3 fields.

### Nullary variants

```python
type State = Red | Yellow | Green

class Red:    pass
class Yellow: pass
class Green:  pass


def next_state(s: State) -> State:
    match s:
        case Red():    return Green()
        case Green():  return Yellow()
        case Yellow(): return Red()
```

Two empty parens `Foo()` — **not** `Foo(_)`, which would be a never-matching positional capture.

---

## 9. Structural interfaces (`examples/09-interfaces/`)

```python
interface Drawable:
    def draw(self) -> None
    def width(self) -> float


interface Serialisable:
    def to_json(self) -> str


class Button:
    label: str


impl Button:
    def draw(self) -> None:
        print(f"[ {self.label} ]")

    def width(self) -> float:
        return float(len(self.label) + 4)

    def to_json(self) -> str:
        return f'{{"type": "button", "label": "{self.label}"}}'


def render(items: list[Drawable]) -> None:
    for item in items:
        item.draw()
```

Same class can satisfy multiple interfaces structurally — no `implements` clause.

---

## 10. Pipes + guards (`examples/10-pipes-and-guards/`)

```python
def clean(raw: str) -> str:
    return raw |> str.strip() |> str.lower() |> str.replace(",", "")


def normalise_username(raw: str?) -> str:
    guard u = raw else:
        return "anonymous"
    let trimmed: str = u.strip()
    if len(trimmed) == 0:
        return "anonymous"
    return trimmed.lower()


def fmt_words(words: list[str]) -> str:
    return (
        words
        |> filter_nonempty()
        |> dedupe()
        |> sort_alpha()
        |> ", ".join()
    )
```

`|>` fills the first positional slot of the next call. Multi-line pipes require parens around the whole chain.

---

## 11. Comptime config from env (`examples/15-comptime-config/`)

```python
comptime let APP_NAME: str = "research-assistant"
comptime let PORT: int = int(env("PORT", "8080"))
comptime let LOG_LEVEL: str = env("LOG_LEVEL", "info").lower()
comptime let IS_PROD: bool = env("BUILD_TAG", "dev") == "prod"
comptime let SUPPORTED_LANGS: list[str] = ["en", "fr", "de", "es", "ja"]


comptime def feature(name: str) -> bool:
    return env("FEATURE_" + name.upper(), "0") == "1"


comptime let SHIPS_AUTH: bool = feature("auth")
```

Build-time constants. `comptime def` functions stay callable at runtime too.

---

## 12. Pydantic `model` + JSON load (`examples/17-file-io-json/`)

```python
model Person:
    name: str
    age: int
    email: str
    address: Address?
    tags: list[str] = []


def load_people(path: Path) -> Result[list[Person], str]:
    try:
        let raw: str = path.read_text(encoding="utf-8")
        let parsed: list[dict[str, object]] = json.loads(raw)
        return Ok([Person.model_validate(p) for p in parsed])
    except FileNotFoundError:
        return Err(f"missing: {path}")
    except json.JSONDecodeError as e:
        return Err(f"invalid json: {e}")
```

`model` for boundary types — Pydantic validates the dict, then downstream code uses fully-typed `Person` values.

---

## 13. Subclassing stdlib (`examples/20-logging/`)

```python
class JsonFormatter(logging.Formatter):
    pass


impl JsonFormatter:
    def format(self, record: logging.LogRecord) -> str:
        let payload: dict[str, object] = {
            "ts": datetime.now(timezone.utc).isoformat(),
            "level": record.levelname,
            "logger": record.name,
            "msg": record.getMessage(),
        }
        if record.exc_info is not None:
            payload["exc"] = self.formatException(record.exc_info)
        return json.dumps(payload)
```

Inheritance declaration in `class X(Parent): pass`, methods in `impl X:`.

---

## 14. Argparse + sealed-union command + match dispatch (`examples/21-cli-tool/`)

```python
type Command = AddCmd | ListCmd | DoneCmd


class AddCmd:  text: str
class ListCmd: show_done: bool
class DoneCmd: index: int


def parse_args(argv: list[str]) -> Result[Command, str]:
    let parser = argparse.ArgumentParser(prog="todo")
    let subs = parser.add_subparsers(dest="cmd", required=True)
    # …
    if ns.cmd == "add":  return Ok(AddCmd(text=ns.text))
    if ns.cmd == "list": return Ok(ListCmd(show_done=ns.all))
    if ns.cmd == "done": return Ok(DoneCmd(index=ns.index))
    return Err(f"unknown command: {ns.cmd}")


def run(cmd: Command) -> int:
    mut items: list[str] = load_items()
    match cmd:
        case AddCmd(text):
            items.append(f"[ ] {text}")
            save_items(items)
            return 0
        case ListCmd(show_done):
            …
        case DoneCmd(index):
            …
```

Canonical CLI shape: parse → sealed-union command → match dispatch.

---

## 15. Async + Result + `?` on await (`examples/23-async-basics/`)

```python
async def fetch(url: str) -> Result[str, FetchError]:
    await asyncio.sleep(0.1)
    if "404" in url:
        return Err(FetchError(url=url, reason="not found"))
    return Ok(f"<body for {url}>")


async def fetch_and_size(url: str) -> Result[int, FetchError]:
    let body: str = await fetch(url)?
    return Ok(len(body))


async def main_async() -> None:
    match await fetch_and_size("https://example.com/page"):
        case Ok(n):
            print(f"size: {n}")
        case Err(e):
            print(f"err: {e.reason}")


def main() -> None:
    asyncio.run(main_async())
```

`await fetch(url)?` combines await with the `?` operator.

---

## 16. gather: + @gatherable + go (`examples/24-async-gather-and-go/`)

```python
@gatherable
async def fetch_user(uid: int) -> User:
    await asyncio.sleep(0.05)
    return User(id=uid, name=f"user-{uid}")


@gatherable
async def fetch_posts(uid: int) -> Posts:
    await asyncio.sleep(0.10)
    return Posts(items=[f"post-{uid}-{i}" for i in range(3)])


async def load_dashboard(uid: int) -> Dashboard:
    gather:
        user   = fetch_user(uid)
        posts  = fetch_posts(uid)
        notifs = fetch_notifs(uid)
    return Dashboard(user=user, posts=posts, notifs=notifs)


async def handle_request(uid: int) -> Dashboard:
    let dash: Dashboard = await load_dashboard(uid)
    go log_visit(uid)
    return dash
```

`gather:` for parallel awaits; `go` for structured fire-and-forget. `@gatherable` is the opt-in marker for auto-gather.

---

## 17. FastAPI server (`examples/28-fastapi-server/`)

```python
model NewTask:
    title: str
    priority: int = Field(default=1, ge=1, le=5)


model Task:
    id: int
    title: str
    priority: int
    done: bool


class TaskStore:
    items: dict[int, Task]
    next_id: int


impl TaskStore:
    def add(self, new: NewTask) -> Task:
        let task: Task = Task(id=self.next_id, title=new.title,
                              priority=new.priority, done=False)
        self.items[task.id] = task
        self.next_id = self.next_id + 1
        return task


let store: TaskStore = TaskStore(items={}, next_id=1)
let app: FastAPI = FastAPI(title="Typhon Tasks")


@app.post("/tasks", response_model=Task, status_code=201)
def create_task(payload: NewTask, s: TaskStore = Depends(get_store)) -> Task:
    return s.add(payload)


@app.get("/tasks/{task_id}", response_model=Task)
def get_task(task_id: int, s: TaskStore = Depends(get_store)) -> Task:
    let found: Task? = s.get(task_id)
    if found is None:
        raise HTTPException(status_code=404, detail=f"no such task: {task_id}")
    return found
```

`model` aligns with Pydantic; `class` is dataclass-shaped. Module-level `let` for the singleton.

---

## 18. Lazy import for heavyweight deps (`examples/29-numpy-arrays/`)

```python
lazy import np = numpy


def demo_creation() -> None:
    let zeros = np.zeros((3, 4))
    let arange = np.arange(0, 10, 2)
    let rand = np.random.default_rng(seed=42).standard_normal((2, 3))


def demo_linalg() -> None:
    let a = np.array([[3.0, 1.0], [1.0, 2.0]])
    let b = np.array([9.0, 8.0])
    let x = np.linalg.solve(a, b)
```

`lazy import np = numpy` defers + aliases. Use `lazy import torch = torch` for PyTorch, etc. NEVER `lazy from numpy import array` — that's a parse error.

---

## 19. PyTorch tensors (`examples/33-pytorch-tensors/`)

```python
lazy import torch = torch


def pick_device() -> torch.device:
    if torch.cuda.is_available():
        return torch.device("cuda")
    if torch.backends.mps.is_available():
        return torch.device("mps")
    return torch.device("cpu")


def demo_autograd() -> None:
    let x: torch.Tensor = torch.tensor([2.0, 3.0], requires_grad=True)
    let y: torch.Tensor = x.pow(2).sum() + 4.0 * x.sum()
    y.backward()
    print(f"dy/dx = {x.grad}")
```

For framework bases that own their own `__init__`, use `class!`:

```python
class! Net(nn.Module):
    layer1: nn.Linear
    layer2: nn.Linear


impl Net:
    def forward(self, x: torch.Tensor) -> torch.Tensor:
        let h: torch.Tensor = torch.relu(self.layer1(x))
        return self.layer2(h)
```

`class!` synthesises `__init__` calling `super().__init__()` then assigning fields, leaving the body to host methods via `impl`.

---

## 20. LLM client with Result-wrapping (`examples/38-llm-anthropic/`)

```python
class LlmError:
    kind: str
    message: str


def get_client() -> Result[Anthropic, LlmError]:
    let key: str? = os.environ.get("ANTHROPIC_API_KEY")
    if key is None:
        return Err(LlmError(kind="config", message="set ANTHROPIC_API_KEY"))
    return Ok(Anthropic(api_key=key))


def ask(client: Anthropic, prompt: str, system: str = "You are concise.") -> Result[str, LlmError]:
    try:
        let resp = client.messages.create(
            model="claude-opus-4-7",
            max_tokens=1024,
            system=system,
            messages=[{"role": "user", "content": prompt}],
        )
    except Exception as e:
        return Err(LlmError(kind="api", message=str(e)))

    mut parts: list[str] = []
    for block in resp.content:
        if block.type == "text":
            parts.append(block.text)
    return Ok("".join(parts))
```

`try/except` lives inside `Result`-returning functions to translate exceptions to `Err`. Downstream code uses `?`.

---

## 21. Tool-use loop with `unsafe:` (`examples/40-llm-tool-use/`)

```python
def _eval_arith(node: object) -> float:
    unsafe:
        if isinstance(node, ast.Constant):
            let v = node.value
            if isinstance(v, int) or isinstance(v, float):
                return float(v)
            raise ValueError(f"non-numeric constant: {v!r}")
        if isinstance(node, ast.BinOp):
            let left: float = _eval_arith(node.left)
            let right: float = _eval_arith(node.right)
            if isinstance(node.op, ast.Add): return left + right
            …
        raise ValueError(f"forbidden node: {type(node).__name__}")
    raise RuntimeError("unreachable")
```

The `unsafe:` block opts out of nullable/narrowing checks for AST traversal; always end with an unreachable `raise` so the type checker accepts the return-path coverage.

---

## 22. Generic agent framework with Callable fields (`examples/43-agent-framework/`)

```python
class Tool:
    name: str
    description: str
    input_schema: dict[str, object]
    run: Callable[[dict[str, object]], str]


class Agent:
    client: Anthropic
    model: str
    tools: dict[str, Tool]
    system: str
    history: list[dict[str, object]]


impl Agent:
    def register(self, tool: Tool) -> None:
        self.tools[tool.name] = tool

    def step(self, max_turns: int = 6) -> Result[str, AgentError]:
        mut turn: int = 0
        while turn < max_turns:
            try:
                let resp = self.client.messages.create(…)
            except Exception as e:
                return Err(AgentError(stage="api", message=str(e)))
            …
        return Err(AgentError(stage="loop", message=f"max_turns={max_turns} exhausted"))

    def ask(self, question: str) -> Result[str, AgentError]:
        self.history.append({"role": "user", "content": question})
        return self.step()
```

`Callable[[dict[str, object]], str]` as a class field — callable-typed slots are first-class. Inner functions populate the field.

---

## 23. Multi-file mini app (`examples/47-mini-app/`)

Layout:

```
mini-app/
├── typhon.toml
├── src/
│   ├── models.ty
│   ├── store.ty       # SQLite-backed NoteStore with @contextmanager
│   ├── agent.ty       # Anthropic-backed Agent with event stream
│   ├── api.ty         # FastAPI app
│   └── main.ty        # entry point
└── tests/
```

`src/store.ty` — `@contextmanager` on a method, `Result[Note, StoreError]`:

```python
impl NoteStore:
    @contextmanager
    def _connect(self) -> Iterator[sqlite3.Connection]:
        let conn: sqlite3.Connection = sqlite3.connect(str(self.db_path))
        try:
            yield conn
            conn.commit()
        finally:
            conn.close()

    def save(self, title: str, body: str) -> Result[Note, StoreError]:
        let now: datetime = datetime.now(timezone.utc)
        try:
            with self._connect() as conn:
                let cur = conn.execute(
                    "INSERT INTO notes (title, body, created_at) VALUES (?, ?, ?)",
                    (title, body, now.isoformat()),
                )
                let new_id: int = int(cur.lastrowid or 0)
            return Ok(Note(id=new_id, title=title, body=body, created_at=now))
        except sqlite3.Error as e:
            return Err(StoreError(op="save", reason=str(e)))
```

`src/api.ty` — module-level `let` for singletons, `match` translates `Err` into `HTTPException`:

```python
let store: NoteStore = open_store(DB_PATH)
let client: Anthropic = Anthropic(api_key=_require_api_key())
let agent: Agent = Agent(client=client, store=store)
let app: FastAPI = FastAPI(title="Typhon Research Assistant")


@app.post("/ask", response_model=AskResponse)
def ask(req: AskRequest, a: Agent = Depends(get_agent)) -> AskResponse:
    match a.run(req.question):
        case Ok(result):
            return AskResponse(answer=result.answer, …)
        case Err(e):
            raise HTTPException(status_code=502, detail=f"{e.stage}: {e.message}")
    raise RuntimeError("unreachable")
```

`typhon.toml`:

```toml
[project]
name = "mini-research-assistant"
src  = "src"
out  = "build"

[python]
target = "3.13"

[emit]
class-default = "dataclass"
format = true

[strictness]
no-implicit-any   = true
exhaustive-match  = "error"
```

---

## 24. Newtype IDs across boundaries (`examples/48-newtype-ids/`)

```python
newtype UserId = int
newtype PostId = int
newtype Email = str


def greet(uid: UserId, email: Email) -> str:
    return f"hi user#{uid} ({email})"


def double(n: int) -> int:
    return n * 2


def demo_escape_upward() -> None:
    # newtype value flows freely into a slot typed as its base
    let me: UserId = UserId(21)
    let twice: int = double(me)  # OK


def demo_cross_newtype_rejected() -> None:
    # the next two lines would fail tyc check:
    #     let post: PostId = PostId(42)
    #     greet(post, Email("x@y.z"))  # tyc::newtype_violation
    pass
```

Asymmetric subtyping: newtype → base (free); base → newtype (explicit `UserId(x)`).

### Same-newtype arithmetic preserves the type (v0.7.0)

```python
newtype LogIndex = int

let a: LogIndex = LogIndex(5)
let b: LogIndex = LogIndex(2)
let c: LogIndex = a + b         # ✅ LogIndex preserved across + - * // % **
let d: float    = a / 2          # ✅ / always widens to float (Python's true division)
# let e: LogIndex = a + Term(1)  # ❌ tyc::operator_type_mismatch across distinct newtypes
```

---

## 25. Generic sealed-union linked list (`examples/50-linked-list/`)

```python
type Node[T] = Cons[T] | Nil


class Cons[T]:
    head: T
    tail: Node[T]


class Nil:
    pass


def length[T](n: Node[T]) -> int:
    match n:
        case Cons(_, tail):
            return 1 + length(tail)
        case Nil():
            return 0


def reverse[T](n: Node[T]) -> Node[T]:
    mut acc: Node[T] = Nil()
    mut cur: Node[T] = n
    mut done: bool = False
    while not done:
        match cur:
            case Cons(head, tail):
                acc = Cons(head=head, tail=acc)
                cur = tail
            case Nil():
                done = True
    return acc
```

Generic sealed unions with `[T]`. Iterative tail-walks via `mut done: bool` are idiomatic — Typhon doesn't TCO.

---

## 26. Iterators / generators (`examples/57-iterators-generators/`)

```python
def naturals() -> Iterator[int]:
    mut n: int = 1
    while True:
        yield n
        n = n + 1


def evens() -> Iterator[int]:
    for n in naturals():
        if n % 2 == 0:
            yield n


def windowed[T](xs: list[T], size: int) -> Iterator[list[T]]:
    mut i: int = 0
    while i + size <= len(xs):
        yield xs[i:i + size]
        i = i + 1


def take[T](src: Iterator[T], n: int) -> list[T]:
    mut out: list[T] = []
    mut left: int = n
    for x in src:
        if left <= 0:
            break
        out.append(x)
        left = left - 1
    return out
```

Generator return type is `Iterator[T]`. `tyc::generator_return_type` fires when a function has `yield` but a non-iterator return type.

---

## 27. Context managers via `@contextmanager` (`examples/58-context-managers/`)

```python
@contextmanager
def timed(label: str) -> Iterator[None]:
    let start: float = time.perf_counter()
    try:
        yield None
    finally:
        let elapsed_ms: float = (time.perf_counter() - start) * 1000.0
        print(f"[{label}] {elapsed_ms:.2f} ms")


@contextmanager
def indent_block(depth: int) -> Iterator[str]:
    let prefix: str = "  " * depth
    print(f"{prefix}<begin depth={depth}>")
    try:
        yield prefix
    finally:
        print(f"{prefix}<end>")


def main() -> None:
    with timed("small"):
        ...
    with indent_block(0) as p:
        print(f"{p}line A")
```

Return type is `Iterator[T]`; single `yield`. v0.7.0's with-as inference reads the yield type back to the `as`-target.

---

## 28. JSON-RPC builder with `unsafe:` coercion (`examples/68-json-rpc-builder/`)

```python
newtype RequestId = int


class JsonRpcError:
    code: int
    message: str


type Response = Success | Failure


class Success:
    id: RequestId
    result: dict[str, str]


class Failure:
    id: RequestId
    error: JsonRpcError


def parse_response(text: str, expect_id: RequestId) -> Result[Response, JsonRpcError]:
    try:
        let raw: dict[str, object] = json.loads(text)
    except json.JSONDecodeError as e:
        return Err(JsonRpcError(code=-32700, message=f"parse error: {e}"))
    if "error" in raw:
        let err_dict: dict[str, object] = raw["error"]
        let err: JsonRpcError = JsonRpcError(code=int(err_dict["code"]),
                                              message=str(err_dict["message"]))
        return Ok(Failure(id=expect_id, error=err))
    if "result" in raw:
        mut typed_res: dict[str, str] = {}
        unsafe:
            let res_raw = raw["result"]
            for k, v in res_raw.items():
                typed_res[str(k)] = str(v)
        return Ok(Success(id=expect_id, result=typed_res))
    return Err(JsonRpcError(code=-32603, message="missing result and error"))
```

Newtype + sealed-union + `unsafe:` for dict-coercion. Success/Failure are themselves Ok variants — `Err` means transport failure.

---

## 29. Pytest + match on Result (`examples/testing/`)

```python
class DivideByZero:
    pass


def div(a: float, b: float) -> Result[float, DivideByZero]:
    if b == 0.0:
        return Err(DivideByZero())
    return Ok(a / b)
```

```python
def test_div_by_zero() -> None:
    match div(1.0, 0.0):
        case Ok(_):
            pytest.fail("expected Err")
        case Err(e):
            assert isinstance(e, DivideByZero)


@pytest.mark.parametrize("xs,expected", [
    ([1.0], 1.0),
    ([1.0, 3.0], 2.0),
    ([2.0, 2.0, 2.0, 2.0], 2.0),
])
def test_average_parametrised(xs: list[float], expected: float) -> None:
    match average(xs):
        case Ok(v):
            assert v == expected
        case Err(_):
            pytest.fail("unexpected Err")
```

Test files use `.ty`. Pattern-match Result in tests; `pytest.fail` on the unexpected branch.

---

## 30. Production-shaped apps (`examples/apps/`)

The 15 apps under `examples/apps/01..15-*/` are the templates for non-trivial multi-file Typhon. Each is a complete `typhon.toml` + `src/` project with grouped subdirectories (`domain/`, `runtime/`, `storage/`, `transport/`, etc.) and `pub *` aggregation via `__init__.ty`.

Pick the closest app for your shape:

| App | Highlights |
|---|---|
| `01-task-scheduler` | `gather:` + `go`, newtype IDs, sealed-union events |
| `02-trading-engine` | Newtypes for money/price/qty, `frozen class`, `pub`, exhaustive match |
| `03-ml-orchestrator` | `lazy import` numpy/torch, PEP-695 generics over pipeline stages |
| `04-event-banking` | Event sourcing with sealed unions, projections, `Result` chains |
| `05-web-crawler` | `gather(strategy="best-effort")`, `go`, rate limits |
| `06-graphql-server` | Generic `class[K, V]` + `impl[K, V]`, recursive query AST, `Callable` fields |
| `07-game-ecs` | Newtype `EntityId`, component dataclasses, system functions |
| `08-mini-compiler` | 13-variant recursive `Expr`, cross-module recursive types, 4-stage `Result` pipeline with heterogeneous errors via `.map_err` |
| `09-search-engine` | Inverted index, ranking, async query path |
| `10-distributed-kv` | 7-variant Raft `Message` union, role state machine |
| `11-game-server` | Real-time, async I/O, structured spawn |
| `12-static-site-gen` | File pipelines, SQLite manifest, `@contextmanager` |
| `13-vector-db` | Generic `Collection[D]`, sealed-union `Metric` + `FilterExpr`, `async with` RW-lock |
| `14-api-gateway` | `Callable`-field middleware pipeline, circuit-breaker state machine, `Callable[[Req], Awaitable[Resp]]` |
| `15-stream-processor` | Generic `ListSource[T]` / `Operator[I, O]`, watermark arithmetic via `newtype WatermarkTs` |

Each app exercises a distinct corner of the language. When in doubt about how to structure something, find the closest app and copy its layout.

---

## Quick recipe index

| Task | Where |
|---|---|
| Hello world / argv | `01-hello-world` |
| `let` vs `mut`, `T?` narrowing | `02-variables-and-types` |
| Comprehensions, loops | `03-control-flow` |
| Collections, `dict.get`, destructure | `04-collections` |
| Generics, `Callable`, closures | `05-functions-and-generics` |
| Classes, `frozen`, `model`, `impl`, `extend` | `06-classes-and-models` |
| `Result` + `?` + `with`-chain | `07-error-handling` |
| Sealed unions + match | `08-sealed-unions-match` |
| Interfaces | `09-interfaces` |
| Pipes + guards | `10-pipes-and-guards` |
| comptime config | `15-comptime-config` |
| JSON I/O + `model.model_validate` | `17-file-io-json` |
| Logging subclass | `20-logging` |
| Argparse CLI | `21-cli-tool` |
| Async basics | `23-async-basics` |
| `gather:` + `go` | `24-async-gather-and-go` |
| FastAPI | `28-fastapi-server` |
| Numpy with lazy import | `29-numpy-arrays` |
| PyTorch with `class!` | `33-pytorch-tensors` |
| Anthropic client | `38-llm-anthropic` |
| Tool-use loop | `40-llm-tool-use` |
| Agent framework | `43-agent-framework` |
| Multi-file mini-app | `47-mini-app` |
| Newtype IDs | `48-newtype-ids` |
| Linked list (generic sealed union) | `50-linked-list` |
| State machine | `56-state-machine` |
| Iterators/generators | `57-iterators-generators` |
| Context managers | `58-context-managers` |
| JSON-RPC builder | `68-json-rpc-builder` |
| Pytest + Result | `testing/` |
| Production-shaped multi-file | `apps/01..15-*/` |
