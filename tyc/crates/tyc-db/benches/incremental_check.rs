//! Benchmarks for the Typhon check pipeline.
//!
//! These measure the end-to-end latency of `check_file`, which runs
//! preprocess → parse → resolve → type-check and returns diagnostics.
//!
//! Implementation note: the heavy resolve and type-check passes currently run
//! directly (outside Salsa tracked queries) because their output types don't
//! yet implement `salsa::Update`. The `preprocessed_text` and
//! `module_decl_names` queries are Salsa-cached, but they are not on the hot
//! path of `check_file`. As a result the benchmarks here measure the raw
//! pipeline cost per call; true Salsa-incremental behaviour will be visible
//! once resolve and type-check are migrated to tracked queries.
//!
//! Run with:
//!   cargo bench -p tyc-db
//!
//! The project targets sub-100 ms analysis latency for typical modules.
//! Regressions above 20 % of the recorded baseline should be investigated
//! before merging.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use tyc_db::{check_file, TycDatabase};

const PATH: &str = "<bench>";

/// A representative small module that exercises resolve and type-check paths:
/// bindings, function signatures, nullable annotations, class declarations.
const MODULE_V1: &str = r#"
let host: str = "localhost"
mut port: int = 8080

class Config:
    host: str
    port: int

def make_config(host: str, port: int) -> Config:
    return Config(host=host, port=port)

def greet(name: str?) -> str:
    if name is None:
        return "hello, stranger"
    return "hello, " + name
"#;

/// `MODULE_V1` with one value changed (8080 → 9090). Salsa should skip
/// unaffected queries, so this measures only the incremental re-check cost.
const MODULE_V2: &str = r#"
let host: str = "localhost"
mut port: int = 9090

class Config:
    host: str
    port: int

def make_config(host: str, port: int) -> Config:
    return Config(host=host, port=port)

def greet(name: str?) -> str:
    if name is None:
        return "hello, stranger"
    return "hello, " + name
"#;

/// A medium module with more declarations to stress the resolver.
const MEDIUM_MODULE: &str = r#"
let app: str = "demo"
mut counter: int = 0

class User:
    id: int
    name: str
    email: str? = None

class Post:
    id: int
    title: str
    body: str

def make_user(id: int, name: str) -> User:
    return User(id=id, name=name)

def make_post(id: int, title: str, body: str) -> Post:
    return Post(id=id, title=title, body=body)

def greet_user(user: User?) -> str:
    if user is None:
        return "nobody"
    return "hello " + user.name

def post_title(post: Post?) -> str:
    if post is None:
        return ""
    return post.title

def clamp(value: int, lo: int, hi: int) -> int:
    if value < lo:
        return lo
    if value > hi:
        return hi
    return value
"#;

/// A large synthetic module (~250 lines) with many declarations, nullable
/// annotations, and function calls to exercise non-linear resolver paths and
/// expose performance regressions that short modules cannot trigger.
const LARGE_MODULE: &str = r#"
let app_name: str = "typhon-bench"
mut request_id: int = 0
let base_url: str = "https://example.com"

class User:
    id: int
    name: str
    email: str? = None
    role: str = "user"

class Post:
    id: int
    title: str
    body: str
    author_id: int
    published: bool = False

class Comment:
    id: int
    post_id: int
    author_id: int
    text: str

class Tag:
    id: int
    name: str
    slug: str

class Category:
    id: int
    name: str
    parent_id: int? = None

class ApiRequest:
    path: str
    method: str
    body: str? = None

class ApiResponse:
    status: int
    body: str
    error: str? = None

class Pagination:
    page: int
    per_page: int
    total: int

def make_user(id: int, name: str, email: str?) -> User:
    return User(id=id, name=name, email=email)

def make_post(id: int, title: str, body: str, author_id: int) -> Post:
    return Post(id=id, title=title, body=body, author_id=author_id)

def make_comment(id: int, post_id: int, author_id: int, text: str) -> Comment:
    return Comment(id=id, post_id=post_id, author_id=author_id, text=text)

def make_tag(id: int, name: str, slug: str) -> Tag:
    return Tag(id=id, name=name, slug=slug)

def make_category(id: int, name: str) -> Category:
    return Category(id=id, name=name)

def ok_response(body: str) -> ApiResponse:
    return ApiResponse(status=200, body=body)

def err_response(message: str) -> ApiResponse:
    return ApiResponse(status=500, body="", error=message)

def not_found_response() -> ApiResponse:
    return ApiResponse(status=404, body="", error="not found")

def format_user(user: User?) -> str:
    if user is None:
        return "<none>"
    return user.name + " <" + user.email if user.email is not None else user.name

def format_post(post: Post?) -> str:
    if post is None:
        return ""
    return post.title

def is_valid_email(email: str?) -> bool:
    if email is None:
        return False
    return "@" in email

def is_published(post: Post?) -> bool:
    if post is None:
        return False
    return post.published

def clamp(value: int, lo: int, hi: int) -> int:
    if value < lo:
        return lo
    if value > hi:
        return hi
    return value

def paginate(page: int, per_page: int, total: int) -> Pagination:
    return Pagination(page=page, per_page=per_page, total=total)

def slug(name: str) -> str:
    return name

def build_url(path: str) -> str:
    return base_url + path

def user_url(user: User) -> str:
    return build_url("/users/" + str(user.id))

def post_url(post: Post) -> str:
    return build_url("/posts/" + str(post.id))

def category_path(cat: Category?) -> str:
    if cat is None:
        return "/"
    if cat.parent_id is None:
        return "/" + cat.name
    return "/parent/" + cat.name

def compose(a: str, b: str) -> str:
    return a + b

def repeat(s: str, n: int) -> str:
    result: str = ""
    i: int = 0
    while i < n:
        result = result + s
        i = i + 1
    return result

def safe_div(a: int, b: int) -> int:
    if b == 0:
        return 0
    return a

def max_of(a: int, b: int) -> int:
    if a > b:
        return a
    return b

def min_of(a: int, b: int) -> int:
    if a < b:
        return a
    return b

def abs_val(n: int) -> int:
    if n < 0:
        return 0 - n
    return n

def is_admin(user: User?) -> bool:
    if user is None:
        return False
    return user.role == "admin"

def can_publish(user: User?, post: Post?) -> bool:
    if user is None:
        return False
    if post is None:
        return False
    return is_admin(user)

def format_response(resp: ApiResponse) -> str:
    if resp.error is not None:
        return "error: " + resp.error
    return resp.body
"#;

fn bench_cold_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_check");

    // `iter_batched` keeps DB allocation in the (unmeasured) setup closure so
    // only the analysis itself contributes to the timing.
    for (label, src) in [
        ("small", MODULE_V1),
        ("medium", MEDIUM_MODULE),
        ("large", LARGE_MODULE),
    ] {
        group.bench_with_input(BenchmarkId::new(label, "module"), src, |b, src| {
            b.iter_batched(
                TycDatabase::new,
                |mut db| {
                    check_file(
                        &mut db,
                        black_box(PATH.to_owned()),
                        black_box(src.to_owned()),
                    )
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_second_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("second_check");

    // Measure the cost of a second `check_file` call after a small content
    // change. `iter_batched` runs the first check (setup) outside the timed
    // region so only the second call is measured. Note: because resolve and
    // type-check run outside Salsa tracked queries today, this benchmark
    // effectively measures the pipeline cost on V2, not true incremental delta.
    // It will become a meaningful Salsa-incremental benchmark once those passes
    // are migrated to tracked queries.
    group.bench_function("small content change", |b| {
        b.iter_batched(
            || {
                let mut db = TycDatabase::new();
                check_file(&mut db, PATH.to_owned(), MODULE_V1.to_owned());
                db
            },
            |mut db| {
                check_file(
                    &mut db,
                    black_box(PATH.to_owned()),
                    black_box(MODULE_V2.to_owned()),
                )
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, bench_cold_check, bench_second_check);
criterion_main!(benches);
