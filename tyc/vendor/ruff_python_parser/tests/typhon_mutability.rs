//! Typhon-specific parser tests for the `val` / `var` mutability prefix.
//!
//! These tests live in the vendored fork; they prove the lexer recognises the
//! new soft keywords and the parser attaches a `Mutability` to the resulting
//! assignment AST node.

use ruff_python_ast::{Mutability, Stmt};
use ruff_python_parser::parse_module;

fn first_stmt(src: &str) -> Stmt {
    parse_module(src)
        .expect("parse should succeed")
        .into_syntax()
        .body
        .into_iter()
        .next()
        .expect("module body should be non-empty")
}

#[test]
fn val_assignment_carries_val_mutability() {
    let Stmt::Assign(a) = first_stmt("val x = 1\n") else {
        panic!("expected StmtAssign");
    };
    assert_eq!(a.mutability, Some(Mutability::Val));
}

#[test]
fn var_assignment_carries_var_mutability() {
    let Stmt::Assign(a) = first_stmt("var counter = 0\n") else {
        panic!("expected StmtAssign");
    };
    assert_eq!(a.mutability, Some(Mutability::Var));
}

#[test]
fn val_annotated_assignment_carries_mutability() {
    let Stmt::AnnAssign(a) = first_stmt("val x: int = 1\n") else {
        panic!("expected StmtAnnAssign");
    };
    assert_eq!(a.mutability, Some(Mutability::Val));
}

#[test]
fn var_annotated_assignment_carries_mutability() {
    let Stmt::AnnAssign(a) = first_stmt("var y: str = \"hi\"\n") else {
        panic!("expected StmtAnnAssign");
    };
    assert_eq!(a.mutability, Some(Mutability::Var));
}

#[test]
fn plain_assignment_has_no_mutability() {
    let Stmt::Assign(a) = first_stmt("x = 1\n") else {
        panic!("expected StmtAssign");
    };
    assert_eq!(a.mutability, None);
}

#[test]
fn val_as_identifier_outside_statement_start() {
    // `val` mid-expression should remain a regular identifier.
    let module = parse_module("y = val + 1\n").expect("parse should succeed");
    let stmt = module.into_syntax().body.into_iter().next().unwrap();
    let Stmt::Assign(a) = stmt else {
        panic!("expected StmtAssign");
    };
    assert_eq!(a.mutability, None);
}

#[test]
fn var_as_identifier_in_function_argument() {
    // `f(var)` — `var` is just an identifier here.
    let module = parse_module("f(var)\n").expect("parse should succeed");
    let stmt = module.into_syntax().body.into_iter().next().unwrap();
    assert!(matches!(stmt, Stmt::Expr(_)));
}
