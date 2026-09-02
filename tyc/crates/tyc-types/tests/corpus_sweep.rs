//! Checker-only corpus sweep (a development aid, `#[ignore]`d by default).
//!
//! Runs the preprocess → resolve → type-check pipeline over every `.ty` path
//! listed in the file named by `TYC_SWEEP_FILES` and writes one line per
//! file that produced an error — `path<TAB>ErrorVariant,ErrorVariant,…` —
//! to the file named by `TYC_SWEEP_OUT`. Diffing two such reports (one from
//! the committed tree, one from the working tree) shows exactly which corpus
//! units a checker change starts or stops rejecting, without needing the
//! full `tyc` binary (whose other crates may not compile mid-change).
//!
//!     TYC_SWEEP_FILES=files.txt TYC_SWEEP_OUT=report.txt \
//!         cargo test -p tyc-types --test corpus_sweep -- --ignored
use std::fs;

use tyc_resolve::resolve_module;
use tyc_syntax::preprocess::{expand_sugar, preprocess};
use tyc_types::check_module_with;

fn variant_name(debug: &str) -> String {
    debug
        .split([' ', '{', '('])
        .next()
        .unwrap_or("?")
        .to_owned()
}

/// The same sugar chain `tyc check` runs.
fn expand(src: &str) -> String {
    expand_sugar(src, true)
}

#[test]
#[ignore = "development aid: needs TYC_SWEEP_FILES / TYC_SWEEP_OUT"]
fn corpus_sweep() {
    // The checker is a recursive descent over large files; a test thread's
    // default 2 MiB stack is far below what the `tyc` binary gives it.
    std::thread::Builder::new()
        .stack_size(512 << 20)
        .spawn(corpus_sweep_body)
        .expect("spawn")
        .join()
        .expect("sweep thread");
}

fn corpus_sweep_body() {
    let list = std::env::var("TYC_SWEEP_FILES").expect("TYC_SWEEP_FILES");
    let out_path = std::env::var("TYC_SWEEP_OUT").expect("TYC_SWEEP_OUT");
    let mut out = String::new();
    for path in fs::read_to_string(&list).expect("file list").lines() {
        if path.trim().is_empty() {
            continue;
        }
        eprintln!("sweep: {path}");
        let Ok(src) = fs::read_to_string(path) else {
            out.push_str(&format!("{path}\tUNREADABLE\n"));
            continue;
        };
        let result = std::panic::catch_unwind(|| {
            let prep = preprocess(&expand(&src));
            let module = match tyc_syntax::parse_module(&prep.python_source) {
                Ok(m) => m.into_syntax(),
                Err(_) => return vec!["PARSE_ERROR".to_owned()],
            };
            let (resolved, resolver_diags) =
                resolve_module(path.to_owned(), &prep.python_source, &module);
            let diags = check_module_with(
                path,
                &prep.python_source,
                &resolved,
                &module,
                &prep.unsafe_lines,
                &prep.frozen_class_lines,
                &prep.impl_distributed_lines,
            );
            if std::env::var("TYC_SWEEP_VERBOSE").is_ok() {
                for e in resolver_diags.errors().iter().chain(diags.errors().iter()) {
                    eprintln!("  {path}: {e}");
                }
            }
            let mut names: Vec<String> = resolver_diags
                .errors()
                .iter()
                .chain(diags.errors().iter())
                .map(|e| variant_name(&format!("{e:?}")))
                .collect();
            names.sort();
            names.dedup();
            names
        });
        match result {
            Ok(names) if names.is_empty() => {}
            Ok(names) => out.push_str(&format!("{path}\t{}\n", names.join(","))),
            Err(_) => out.push_str(&format!("{path}\tPANIC\n")),
        }
    }
    fs::write(&out_path, out).expect("write report");
}

/// Error-variant set of one source text through the same pipeline as the sweep.
fn variants_of(path: &str, src: &str) -> Vec<String> {
    let prep = preprocess(&expand(src));
    let module = match tyc_syntax::parse_module(&prep.python_source) {
        Ok(m) => m.into_syntax(),
        Err(_) => return vec!["PARSE_ERROR".to_owned()],
    };
    let (resolved, resolver_diags) = resolve_module(path.to_owned(), &prep.python_source, &module);
    let diags = check_module_with(
        path,
        &prep.python_source,
        &resolved,
        &module,
        &prep.unsafe_lines,
        &prep.frozen_class_lines,
        &prep.impl_distributed_lines,
    );
    let mut names: Vec<String> = resolver_diags
        .errors()
        .iter()
        .chain(diags.errors().iter())
        .map(|e| variant_name(&format!("{e:?}")))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Surface-level mutations that must not change what a program means: the
/// preprocessor works on text, so each one probes a different assumption
/// (line endings, a missing final newline, a byte-order mark, comments at
/// column zero inside blocks, tab indentation).
fn mutations(src: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    out.push(("crlf", src.replace('\n', "\r\n")));
    out.push(("no-final-newline", src.trim_end_matches('\n').to_owned()));
    out.push(("bom", format!("\u{feff}{src}")));
    // A column-zero comment after every block header that is not string content.
    let mask = tyc_syntax::lexmask::LexMask::new(src);
    let mut with_comments = String::with_capacity(src.len() + 256);
    for (i, line) in src.split_inclusive('\n').enumerate() {
        with_comments.push_str(line);
        let code_end = mask.line_code_end(i).min(line.len());
        let code = line[..code_end].trim_end();
        if !mask.line_starts_in_string(i) && code.ends_with(':') && line.ends_with('\n') {
            with_comments.push_str("# mutation: column-zero comment\n");
        }
    }
    out.push(("col0-comment", with_comments));
    // Tabs: only when every indent is a multiple of four spaces and no line is
    // string content (which the mask tells us), so the mutation is faithful.
    let tabbable = src.split_inclusive('\n').enumerate().all(|(i, l)| {
        mask.line_starts_in_string(i) || {
            let indent = l.len() - l.trim_start_matches(' ').len();
            indent % 4 == 0 && !l.starts_with('\t')
        }
    });
    if tabbable {
        let tabbed: String = src
            .split_inclusive('\n')
            .enumerate()
            .map(|(i, l)| {
                if mask.line_starts_in_string(i) {
                    return l.to_owned();
                }
                let indent = l.len() - l.trim_start_matches(' ').len();
                format!("{}{}", "\t".repeat(indent / 4), &l[indent..])
            })
            .collect();
        out.push(("tabs", tabbed));
    }
    out
}

#[test]
#[ignore = "development aid: needs TYC_SWEEP_FILES / TYC_SWEEP_OUT"]
fn corpus_mutation_sweep() {
    std::thread::Builder::new()
        .stack_size(512 << 20)
        .spawn(corpus_mutation_sweep_body)
        .expect("spawn")
        .join()
        .expect("sweep thread");
}

fn corpus_mutation_sweep_body() {
    let list = std::env::var("TYC_SWEEP_FILES").expect("TYC_SWEEP_FILES");
    let out_path = std::env::var("TYC_SWEEP_OUT").expect("TYC_SWEEP_OUT");
    let mut out = String::new();
    for path in fs::read_to_string(&list).expect("file list").lines() {
        if path.trim().is_empty() {
            continue;
        }
        let Ok(src) = fs::read_to_string(path) else {
            continue;
        };
        if src.contains('\r') || src.starts_with('\u{feff}') {
            continue;
        }
        let base = match std::panic::catch_unwind(|| variants_of(path, &src)) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for (name, mutated) in mutations(&src) {
            let got = match std::panic::catch_unwind(|| variants_of(path, &mutated)) {
                Ok(v) => v,
                Err(_) => vec!["PANIC".to_owned()],
            };
            if got != base {
                out.push_str(&format!(
                    "{path}\t{name}\t[{}] -> [{}]\n",
                    base.join(","),
                    got.join(",")
                ));
            }
        }
    }
    fs::write(&out_path, out).expect("write report");
}
