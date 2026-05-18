//! Compare a `.dty` stub to its implementation module and report
//! mismatches.  This is Typhon's equivalent of mypy's `stubtest`: it
//! diffs the public surface of both modules and emits a list of
//! human-readable findings.
//!
//! The comparison is purely AST-based — no runtime introspection — which
//! keeps the check hermetic and fast.  Trade-off: we cannot validate
//! dynamic attributes that only exist on instances; `stubtest` proper
//! catches those because it imports the module.  Adding a sandboxed
//! runtime probe is a follow-up.
//!
//! The shapes we compare:
//!
//! - **Functions** at module scope — name, positional-parameter names,
//!   parameter type annotations, and return type.
//! - **Classes** at module scope — name, set of methods (with parameter
//!   names and annotations), set of annotated fields.
//!
//! Type annotations are compared textually via the same code printer used
//! for emission, so structurally-equivalent annotations compare equal.
//! Full semantic comparison (resolving type aliases, checking variance) is
//! a follow-up.

use std::collections::BTreeMap;

use ruff_python_ast::{Expr, ModModule, Stmt, StmtClassDef, StmtFunctionDef};

/// One finding produced by [`compare_modules`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubTestFinding {
    /// Human-readable message describing the mismatch.
    pub message: String,
    /// Stub-side kind so callers can group by severity.  All current
    /// findings are errors; warnings are reserved.
    pub kind: StubTestKind,
}

/// Severity-like classification for a stub-test finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StubTestKind {
    /// The stub declares a symbol that the implementation does not expose.
    MissingInImpl,
    /// The implementation declares a symbol that the stub does not record.
    MissingInStub,
    /// The symbol exists in both but its surface signature differs.
    SignatureMismatch,
}

/// Compare two parsed modules and return every difference as a finding.
/// The stub module is the source of truth for the public API; the
/// implementation is what callers actually use at runtime.
pub fn compare_modules(stub: &ModModule, implementation: &ModModule) -> Vec<StubTestFinding> {
    let stub_api = extract_api(stub);
    let impl_api = extract_api(implementation);
    diff_apis(&stub_api, &impl_api)
}

/// Snapshot of the public surface of a module, used for diffing.
#[derive(Debug, Default)]
struct ModuleApi {
    /// Module-level function name → signature shape.
    functions: BTreeMap<String, FunctionShape>,
    /// Module-level class name → class shape.
    classes: BTreeMap<String, ClassShape>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionShape {
    /// Positional parameter names in declaration order.  Receivers
    /// (`self`/`cls`) are kept because their absence/presence is a real
    /// difference between a stub and an implementation.
    params: Vec<String>,
    /// Rendered annotation text for each positional parameter, in the same
    /// order as `params`.  `None` when no annotation is present.
    param_annotations: Vec<Option<String>>,
    /// Rendered return-type annotation, or `None` if absent.
    return_annotation: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ClassShape {
    /// Method name → signature shape.
    methods: BTreeMap<String, FunctionShape>,
    /// Field name → declared annotation text (rendered with the printer
    /// so two equivalent ASTs compare equal).  Annotations whose textual
    /// form is empty are recorded as the empty string.
    fields: BTreeMap<String, String>,
}

fn extract_api(module: &ModModule) -> ModuleApi {
    let mut api = ModuleApi::default();
    for stmt in &module.body {
        collect_top_level_stmt(stmt, &mut api);
    }
    api
}

fn collect_top_level_stmt(stmt: &Stmt, api: &mut ModuleApi) {
    match stmt {
        // Ruff collapses sync/async function defs into a single
        // `StmtFunctionDef`; we treat both as the same surface shape for
        // v1 so a sync stub of an async impl is flagged via the signature
        // diff (parameter list match) rather than a separate axis.
        // Improving this is a follow-up.
        Stmt::FunctionDef(f) => {
            api.functions
                .insert(f.name.as_str().to_owned(), function_shape(f));
        }
        Stmt::ClassDef(c) => {
            api.classes
                .insert(c.name.as_str().to_owned(), class_shape(c));
        }
        _ => {}
    }
}

fn render_expr(expr: &ruff_python_ast::Expr) -> String {
    let mut e = crate::Emitter::new();
    e.emit_expr(expr);
    e.finish().trim().to_owned()
}

fn function_shape(f: &StmtFunctionDef) -> FunctionShape {
    let mut params = Vec::new();
    let mut param_annotations = Vec::new();
    for arg in f
        .parameters
        .posonlyargs
        .iter()
        .chain(f.parameters.args.iter())
    {
        params.push(arg.parameter.name.as_str().to_owned());
        param_annotations.push(arg.parameter.annotation.as_deref().map(render_expr));
    }
    let return_annotation = f.returns.as_deref().map(render_expr);
    FunctionShape {
        params,
        param_annotations,
        return_annotation,
    }
}

/// Describe the specific differences between two function shapes for
/// inclusion in a `SignatureMismatch` diagnostic message.
fn function_diff_detail(stub: &FunctionShape, implementation: &FunctionShape) -> String {
    let mut parts = Vec::new();
    if stub.params != implementation.params {
        parts.push(format!(
            "parameter names {:?} vs {:?}",
            stub.params, implementation.params
        ));
    }
    if stub.param_annotations != implementation.param_annotations {
        parts.push(format!(
            "parameter annotations {:?} vs {:?}",
            stub.param_annotations, implementation.param_annotations
        ));
    }
    if stub.return_annotation != implementation.return_annotation {
        parts.push(format!(
            "return type {:?} vs {:?}",
            stub.return_annotation, implementation.return_annotation
        ));
    }
    if parts.is_empty() {
        "unknown difference".to_owned()
    } else {
        parts.join("; ")
    }
}

fn class_shape(c: &StmtClassDef) -> ClassShape {
    let mut shape = ClassShape::default();
    for stmt in &c.body {
        match stmt {
            Stmt::FunctionDef(f) => {
                shape
                    .methods
                    .insert(f.name.as_str().to_owned(), function_shape(f));
            }
            Stmt::AnnAssign(a) => {
                if let Expr::Name(n) = a.target.as_ref() {
                    // Render the annotation through the same printer used by
                    // emit() so that two structurally-equivalent annotations
                    // produce the same text.
                    let mut emitter = crate::Emitter::new();
                    emitter.emit_expr(&a.annotation);
                    shape
                        .fields
                        .insert(n.id.as_str().to_owned(), emitter.finish().trim().to_owned());
                }
            }
            _ => {}
        }
    }
    shape
}

fn diff_apis(stub: &ModuleApi, implementation: &ModuleApi) -> Vec<StubTestFinding> {
    let mut findings = Vec::new();

    // Functions in stub but missing or different in implementation.
    for (name, stub_fn) in &stub.functions {
        match implementation.functions.get(name) {
            None => findings.push(StubTestFinding {
                message: format!("stub declares function `{name}` but the implementation does not"),
                kind: StubTestKind::MissingInImpl,
            }),
            Some(impl_fn) if impl_fn != stub_fn => {
                findings.push(StubTestFinding {
                    message: format!(
                        "stub function `{name}` signature mismatch: {}",
                        function_diff_detail(stub_fn, impl_fn)
                    ),
                    kind: StubTestKind::SignatureMismatch,
                });
            }
            _ => {}
        }
    }
    // Functions in implementation that the stub forgot about.  Private
    // names (leading underscore) are excluded since stubs intentionally
    // hide them.
    for name in implementation.functions.keys() {
        if name.starts_with('_') {
            continue;
        }
        if !stub.functions.contains_key(name) {
            findings.push(StubTestFinding {
                message: format!(
                    "implementation exposes function `{name}` but the stub does not declare it"
                ),
                kind: StubTestKind::MissingInStub,
            });
        }
    }

    // Classes — diff name presence, then member shape.
    for (name, stub_class) in &stub.classes {
        match implementation.classes.get(name) {
            None => findings.push(StubTestFinding {
                message: format!("stub declares class `{name}` but the implementation does not"),
                kind: StubTestKind::MissingInImpl,
            }),
            Some(impl_class) => diff_classes(name, stub_class, impl_class, &mut findings),
        }
    }
    for name in implementation.classes.keys() {
        if name.starts_with('_') {
            continue;
        }
        if !stub.classes.contains_key(name) {
            findings.push(StubTestFinding {
                message: format!(
                    "implementation exposes class `{name}` but the stub does not declare it"
                ),
                kind: StubTestKind::MissingInStub,
            });
        }
    }

    findings
}

fn diff_classes(
    name: &str,
    stub: &ClassShape,
    implementation: &ClassShape,
    findings: &mut Vec<StubTestFinding>,
) {
    for (mname, stub_method) in &stub.methods {
        match implementation.methods.get(mname) {
            None => findings.push(StubTestFinding {
                message: format!(
                    "stub class `{name}` declares method `{mname}` but the implementation does not"
                ),
                kind: StubTestKind::MissingInImpl,
            }),
            Some(impl_method) if impl_method != stub_method => {
                findings.push(StubTestFinding {
                    message: format!(
                        "stub method `{name}.{mname}` signature mismatch: {}",
                        function_diff_detail(stub_method, impl_method)
                    ),
                    kind: StubTestKind::SignatureMismatch,
                });
            }
            _ => {}
        }
    }
    for mname in implementation.methods.keys() {
        if mname.starts_with('_') {
            continue;
        }
        if !stub.methods.contains_key(mname) {
            findings.push(StubTestFinding {
                message: format!(
                    "implementation class `{name}` exposes method `{mname}` but the stub does not declare it"
                ),
                kind: StubTestKind::MissingInStub,
            });
        }
    }

    for (fname, stub_ann) in &stub.fields {
        match implementation.fields.get(fname) {
            None => findings.push(StubTestFinding {
                message: format!(
                    "stub class `{name}` declares field `{fname}` but the implementation does not annotate it"
                ),
                kind: StubTestKind::MissingInImpl,
            }),
            Some(impl_ann) if impl_ann != stub_ann => {
                findings.push(StubTestFinding {
                    message: format!(
                        "stub field `{name}.{fname}` has annotation `{stub_ann}` but implementation has `{impl_ann}`"
                    ),
                    kind: StubTestKind::SignatureMismatch,
                });
            }
            _ => {}
        }
    }
    for fname in implementation.fields.keys() {
        if fname.starts_with('_') {
            continue;
        }
        if !stub.fields.contains_key(fname) {
            findings.push(StubTestFinding {
                message: format!(
                    "implementation class `{name}` annotates field `{fname}` but the stub does not declare it"
                ),
                kind: StubTestKind::MissingInStub,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tyc_syntax::parse_module;

    fn parse_mod(src: &str) -> ModModule {
        parse_module(src).expect("parse failed").into_syntax()
    }

    #[test]
    fn matching_function_produces_no_findings() {
        let stub = parse_mod("def add(a: int, b: int) -> int: ...\n");
        let imp = parse_mod("def add(a: int, b: int) -> int:\n    return a + b\n");
        assert!(compare_modules(&stub, &imp).is_empty());
    }

    #[test]
    fn missing_in_impl_is_flagged() {
        let stub = parse_mod("def add(a: int, b: int) -> int: ...\n");
        let imp = parse_mod("def subtract(a: int, b: int) -> int: return a - b\n");
        let findings = compare_modules(&stub, &imp);
        assert!(
            findings
                .iter()
                .any(|f| f.kind == StubTestKind::MissingInImpl && f.message.contains("add")),
            "expected MissingInImpl for `add`, got: {findings:?}"
        );
    }

    #[test]
    fn missing_in_stub_is_flagged() {
        let stub = parse_mod("def add(a: int, b: int) -> int: ...\n");
        let imp = parse_mod(
            "def add(a: int, b: int) -> int: return 0\ndef secret(a: int) -> int: return 0\n",
        );
        let findings = compare_modules(&stub, &imp);
        assert!(
            findings
                .iter()
                .any(|f| f.kind == StubTestKind::MissingInStub && f.message.contains("secret")),
            "expected MissingInStub for `secret`, got: {findings:?}"
        );
    }

    #[test]
    fn private_impl_function_not_flagged() {
        let stub = parse_mod("def public() -> int: ...\n");
        let imp = parse_mod("def public() -> int: return 0\ndef _private() -> int: return 0\n");
        let findings = compare_modules(&stub, &imp);
        assert!(
            findings.is_empty(),
            "_private should not be flagged; got: {findings:?}"
        );
    }

    #[test]
    fn parameter_mismatch_is_flagged() {
        let stub = parse_mod("def add(a: int, b: int) -> int: ...\n");
        let imp = parse_mod("def add(x: int, y: int) -> int: return x + y\n");
        let findings = compare_modules(&stub, &imp);
        assert!(
            findings
                .iter()
                .any(|f| f.kind == StubTestKind::SignatureMismatch),
            "rename should be flagged as signature mismatch; got: {findings:?}"
        );
    }

    #[test]
    fn class_method_diff_is_flagged() {
        let stub = parse_mod("class User:\n    def name(self) -> str: ...\n");
        let imp = parse_mod("class User:\n    def name(self) -> str: return ''\n    def email(self) -> str: return ''\n");
        let findings = compare_modules(&stub, &imp);
        assert!(
            findings.iter().any(|f| f.message.contains("email")),
            "extra impl method should be flagged; got: {findings:?}"
        );
    }

    #[test]
    fn class_field_diff_is_flagged() {
        let stub = parse_mod("class User:\n    id: int\n    name: str\n");
        let imp = parse_mod("class User:\n    id: int\n");
        let findings = compare_modules(&stub, &imp);
        assert!(
            findings.iter().any(|f| f.message.contains("name")),
            "missing impl field should be flagged; got: {findings:?}"
        );
    }

    #[test]
    fn return_type_mismatch_is_flagged() {
        let stub = parse_mod("def compute(n: int) -> int: ...\n");
        let imp = parse_mod("def compute(n: int) -> str: return str(n)\n");
        let findings = compare_modules(&stub, &imp);
        assert!(
            findings
                .iter()
                .any(|f| f.kind == StubTestKind::SignatureMismatch && f.message.contains("return")),
            "return type mismatch should be flagged; got: {findings:?}"
        );
    }

    #[test]
    fn parameter_annotation_mismatch_is_flagged() {
        let stub = parse_mod("def process(x: int) -> str: ...\n");
        let imp = parse_mod("def process(x: str) -> str: return x\n");
        let findings = compare_modules(&stub, &imp);
        assert!(
            findings
                .iter()
                .any(|f| f.kind == StubTestKind::SignatureMismatch
                    && f.message.contains("annotation")),
            "parameter annotation mismatch should be flagged; got: {findings:?}"
        );
    }

    #[test]
    fn matching_annotations_produce_no_findings() {
        // Same names and same annotations — should be perfectly clean.
        let stub = parse_mod("def add(a: int, b: int) -> int: ...\n");
        let imp = parse_mod("def add(a: int, b: int) -> int:\n    return a + b\n");
        assert!(
            compare_modules(&stub, &imp).is_empty(),
            "identical annotations should not produce findings"
        );
    }
}
