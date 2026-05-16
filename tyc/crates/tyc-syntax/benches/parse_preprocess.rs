//! Benchmarks for the Typhon parse + preprocess pipeline.
//!
//! These measure the latency of the two hottest paths in incremental compilation:
//!
//! 1. `preprocess` — strips `val`/`var`/`model`/etc. line-prefix keywords and
//!    expands `T?` nullable shorthand so the Python parser sees plain Python.
//! 2. `parse` — the `rustpython-parser` full parse of the preprocessed source.
//!
//! Run with:
//!   cargo bench -p tyc-syntax
//!
//! The goal is sub-100 ms on a representative module. Regressions above 20 %
//! of the baseline should trigger a review before merging.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use rustpython_parser::{parse, Mode};
use tyc_syntax::preprocess::preprocess;

/// A small but representative Typhon module that exercises the common
/// preprocessing paths: `val`/`var` bindings, `T?` nullable annotations,
/// function definitions, and class declarations.
const SMALL_MODULE: &str = r#"
val host: str = "localhost"
var port: int = 8080
val db_url: str? = None

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
val APP_NAME: str = "typhon-demo"
var request_count: int = 0

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

val DEFAULT_HOST: str = "0.0.0.0"
var current_port: int = 9000

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

fn bench_preprocess(c: &mut Criterion) {
    let mut group = c.benchmark_group("preprocess");

    group.bench_with_input(
        BenchmarkId::new("small", "module"),
        SMALL_MODULE,
        |b, src| {
            b.iter(|| preprocess(black_box(src)));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("medium", "module"),
        MEDIUM_MODULE,
        |b, src| {
            b.iter(|| preprocess(black_box(src)));
        },
    );

    group.finish();
}

fn bench_parse(c: &mut Criterion) {
    let small_prep = preprocess(SMALL_MODULE);
    let medium_prep = preprocess(MEDIUM_MODULE);

    let mut group = c.benchmark_group("parse");

    group.bench_with_input(
        BenchmarkId::new("small", "module"),
        &small_prep.python_source,
        |b, src| {
            b.iter(|| parse(black_box(src.as_str()), Mode::Module, "<bench>"));
        },
    );

    group.bench_with_input(
        BenchmarkId::new("medium", "module"),
        &medium_prep.python_source,
        |b, src| {
            b.iter(|| parse(black_box(src.as_str()), Mode::Module, "<bench>"));
        },
    );

    group.finish();
}

fn bench_preprocess_then_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("preprocess_then_parse");

    group.bench_function("small module", |b| {
        b.iter(|| {
            let prep = preprocess(black_box(SMALL_MODULE));
            parse(prep.python_source.as_str(), Mode::Module, "<bench>")
        });
    });

    group.bench_function("medium module", |b| {
        b.iter(|| {
            let prep = preprocess(black_box(MEDIUM_MODULE));
            parse(prep.python_source.as_str(), Mode::Module, "<bench>")
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_preprocess,
    bench_parse,
    bench_preprocess_then_parse
);
criterion_main!(benches);
