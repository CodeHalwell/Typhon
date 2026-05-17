//! Benchmarks for the Typhon parse + preprocess pipeline.
//!
//! These measure the latency of the two hottest paths in incremental compilation:
//!
//! 1. `preprocess` — strips `model`/`impl`/`extend`/etc. line-prefix keywords and
//!    expands `T?` nullable shorthand so the Python parser sees plain Python.
//! 2. `parse` — the vendored Ruff parser run on the preprocessed source.
//!
//! Run with:
//!   cargo bench -p tyc-syntax
//!
//! The goal is sub-100 ms on a representative module. Regressions above 20 %
//! of the baseline should trigger a review before merging.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use tyc_syntax::{parse_module, preprocess::preprocess};

/// A small but representative Typhon module that exercises the common
/// preprocessing paths: `let`/`mut` bindings, `T?` nullable annotations,
/// function definitions, and class declarations.
const SMALL_MODULE: &str = r#"
let host: str = "localhost"
mut port: int = 8080
let db_url: str? = None

class Config:
    host: str
    port: int
    db_url: str? = None

def make_config(host: str, port: int) -> Config:
    return Config(host=host, port=port)

def greet(name: str?) -> str:
    if name is None:
        return "hello, stranger"
    return "hello, " + name
"#;

/// A larger module (~100 lines) with more language features to stress-test
/// the preprocessor regex passes.
const MEDIUM_MODULE: &str = r#"
let APP_NAME: str = "typhon-demo"
mut request_count: int = 0

class User:
    id: int
    name: str
    email: str? = None

class Post:
    id: int
    title: str
    author: User
    body: str

class Comment:
    id: int
    post: Post
    author: User
    text: str

interface Repository:
    def find(self, id: int) -> User?: ...
    def save(self, user: User) -> None: ...

def find_user(repo: Repository, id: int) -> User?:
    return repo.find(id)

def create_post(author: User, title: str, body: str) -> Post:
    return Post(id=0, title=title, author=author, body=body)

def add_comment(post: Post, author: User, text: str) -> Comment:
    return Comment(id=0, post=post, author=author, text=text)

def summarise(post: Post) -> str:
    return post.title + " by " + post.author.name

def process(users: list[User]) -> list[str]:
    results: list[str] = []
    for user in users:
        results.append(user.name)
    return results

let DEFAULT_HOST: str = "0.0.0.0"
mut current_port: int = 9000

def build_url(host: str, port: int, path: str?) -> str:
    base: str = "http://" + host + ":" + str(port)
    if path is None:
        return base
    return base + path

def is_valid_email(email: str?) -> bool:
    if email is None:
        return False
    return "@" in email

def clamp(value: int, lo: int, hi: int) -> int:
    if value < lo:
        return lo
    if value > hi:
        return hi
    return value

class ApiResponse:
    status: int
    body: str
    error: str? = None

def ok_response(body: str) -> ApiResponse:
    return ApiResponse(status=200, body=body)

def err_response(message: str) -> ApiResponse:
    return ApiResponse(status=500, body="", error=message)
"#;

/// A large module (~250 lines) that exercises the preprocessor's regex passes
/// and the parser at scale. Includes nullable annotations on nearly every
/// declaration and deeply nested function signatures to expose non-linear
/// performance regressions that shorter modules cannot trigger.
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
    return user.name

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

def tag_slug(tag: Tag?) -> str:
    if tag is None:
        return ""
    return tag.slug

def pagination_info(p: Pagination) -> str:
    return str(p.page) + "/" + str(p.total)

def request_method(req: ApiRequest?) -> str:
    if req is None:
        return "GET"
    return req.method

def has_body(req: ApiRequest?) -> bool:
    if req is None:
        return False
    if req.body is None:
        return False
    return True
"#;

fn bench_preprocess(c: &mut Criterion) {
    let mut group = c.benchmark_group("preprocess");

    for (label, src) in [
        ("small", SMALL_MODULE),
        ("medium", MEDIUM_MODULE),
        ("large", LARGE_MODULE),
    ] {
        group.bench_with_input(BenchmarkId::new(label, "module"), src, |b, src| {
            b.iter(|| preprocess(black_box(src)));
        });
    }

    group.finish();
}

fn bench_parse(c: &mut Criterion) {
    let small_prep = preprocess(SMALL_MODULE);
    let medium_prep = preprocess(MEDIUM_MODULE);
    let large_prep = preprocess(LARGE_MODULE);

    let mut group = c.benchmark_group("parse");

    for (label, src) in [
        ("small", small_prep.python_source.as_str()),
        ("medium", medium_prep.python_source.as_str()),
        ("large", large_prep.python_source.as_str()),
    ] {
        group.bench_with_input(BenchmarkId::new(label, "module"), src, |b, src| {
            // black_box both the input and result so the compiler cannot elide
            // the parse work in tight loops.
            b.iter(|| black_box(parse_module(black_box(src))));
        });
    }

    group.finish();
}

fn bench_preprocess_then_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("preprocess_then_parse");

    for (label, src) in [
        ("small", SMALL_MODULE),
        ("medium", MEDIUM_MODULE),
        ("large", LARGE_MODULE),
    ] {
        group.bench_function(label, |b| {
            b.iter(|| {
                let prep = preprocess(black_box(src));
                // black_box the result to prevent the compiler from eliding
                // the parse work in tight loops.
                black_box(parse_module(prep.python_source.as_str()))
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_preprocess,
    bench_parse,
    bench_preprocess_then_parse
);
criterion_main!(benches);
