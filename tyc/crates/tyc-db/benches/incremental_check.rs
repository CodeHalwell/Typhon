//! Benchmarks for the Typhon incremental check pipeline.
//!
//! These measure the end-to-end latency of `check_file` — the Salsa-backed
//! query that runs preprocess → parse → resolve → type-check and returns
//! diagnostics — both for a cold database (first check) and for warm
//! incremental re-checks after a small edit.
//!
//! Run with:
//!   cargo bench -p tyc-db
//!
//! The project targets sub-100 ms incremental feedback for typical modules.
//! Regressions above 20 % of the recorded baseline should be investigated
//! before merging.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use tyc_db::{check_file, TycDatabase};

const PATH: &str = "<bench>";

/// A representative module that exercises resolve and type-check paths:
/// bindings, function signatures, nullable annotations, class declarations.
const MODULE_V1: &str = r#"
val host: str = "localhost"
var port: int = 8080

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

/// The same module with a small edit: `port` value changed from 8080 to 9090.
/// Salsa should only re-run queries whose inputs changed.
const MODULE_V2: &str = r#"
val host: str = "localhost"
var port: int = 9090

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

/// A slightly larger module with more declarations to stress the resolver.
const LARGER_MODULE: &str = r#"
val app: str = "demo"
var counter: int = 0

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

fn bench_cold_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("cold_check");

    group.bench_with_input(BenchmarkId::new("small", "module"), MODULE_V1, |b, src| {
        b.iter(|| {
            let mut db = TycDatabase::new();
            check_file(
                &mut db,
                black_box(PATH.to_owned()),
                black_box(src.to_owned()),
            )
        });
    });

    group.bench_with_input(
        BenchmarkId::new("larger", "module"),
        LARGER_MODULE,
        |b, src| {
            b.iter(|| {
                let mut db = TycDatabase::new();
                check_file(
                    &mut db,
                    black_box(PATH.to_owned()),
                    black_box(src.to_owned()),
                )
            });
        },
    );

    group.finish();
}

fn bench_incremental_recheck(c: &mut Criterion) {
    let mut group = c.benchmark_group("incremental_recheck");

    // Warm the database with V1, then benchmark the incremental re-check of V2
    // (one value changed). Salsa should skip unaffected queries.
    group.bench_function("small edit delta", |b| {
        let mut db = TycDatabase::new();
        // Prime the database with the initial version.
        check_file(&mut db, PATH.to_owned(), MODULE_V1.to_owned());

        b.iter(|| {
            check_file(
                &mut db,
                black_box(PATH.to_owned()),
                black_box(MODULE_V2.to_owned()),
            )
        });
    });

    group.finish();
}

criterion_group!(benches, bench_cold_check, bench_incremental_recheck);
criterion_main!(benches);
