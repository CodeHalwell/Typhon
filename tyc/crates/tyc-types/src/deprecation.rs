//! PEP 702 deprecations (`tyc::deprecated`) and calls on third-party
//! instances that cannot be called (`tyc::not_callable`).
//!
//! A deprecation comes from venv introspection (`__deprecated__` on a
//! dependency's function, class, method, property getter or `__call__`) or
//! from a `@deprecated("...")` decorator in `.ty` source. Every report is a
//! warning: the API still runs, and CPython only emits a
//! `DeprecationWarning`.
use super::*;

/// The message of a `@deprecated("...")` decorator (bare, or qualified as
/// `warnings.deprecated` / `typing_extensions.deprecated`). Only the call
/// form with a string-literal message counts, which is the only form PEP 702
/// allows.
pub(super) fn from_decorators(decorators: &[ruff_python_ast::Decorator]) -> Option<String> {
    decorators.iter().find_map(|d| {
        let Expr::Call(call) = &d.expression else {
            return None;
        };
        let named = match call.func.as_ref() {
            Expr::Name(n) => n.id.as_str() == "deprecated",
            Expr::Attribute(a) => {
                a.attr.as_str() == "deprecated"
                    && matches!(
                        a.value.as_ref(),
                        Expr::Name(m) if matches!(m.id.as_str(), "warnings" | "typing_extensions")
                    )
            }
            _ => false,
        };
        if !named {
            return None;
        }
        match call.arguments.args.first() {
            Some(Expr::StringLiteral(s)) => Some(s.value.to_str().to_owned()),
            _ => None,
        }
    })
}

/// The class an instance-typed value belongs to: `Foo`, `Foo[int]`, and the
/// same through `T?` (a nullable receiver is its own diagnostic).
fn class_of(t: &Type) -> Option<String> {
    match t.strip_none() {
        Type::Class(n) | Type::Generic(n, _) => Some(n),
        _ => None,
    }
}

fn report(c: &mut Checker, what: String, message: &str, span: (usize, usize)) {
    if !c.deprecation_reported.insert(span) {
        return;
    }
    let length = span.1.saturating_sub(span.0).max(1);
    c.diagnostics.push_warning(TycError::deprecated(
        what, message, &c.path, c.source, span.0, length,
    ));
}

/// What calling a value of type `t` resolves to on its class: the
/// deprecation of its `__call__`, and whether it cannot be called at all.
/// Only a followed third-party class (always an instance, never a class
/// object) is judged not callable, and only when introspection proved it.
fn instance_call(c: &Checker, t: &Type) -> (Option<String>, bool) {
    let Some(cls) = class_of(t) else {
        return (None, false);
    };
    if let Some(sig) = c.find_method(&cls, "__call__") {
        return (sig.arity_info.deprecated.clone(), false);
    }
    let not_callable = c
        .foreign_class(&cls)
        .is_some_and(|shape| shape.instances_not_callable);
    (None, not_callable)
}

/// Report a deprecated callee, or a call on a third-party instance that
/// has no `__call__`. Called once per inference of an `Expr::Call`; repeats
/// are dropped by span.
pub(super) fn check_call(c: &mut Checker, call: &ruff_python_ast::ExprCall) {
    if c.unsafe_depth > 0 {
        return;
    }
    let func = call.func.as_ref();
    let span = (
        func.range().start().to_usize(),
        func.range().end().to_usize(),
    );
    match func {
        Expr::Name(n) => {
            let name = n.id.as_str();
            // A local binding shadows everything below.
            let bound = c.env.lookup(name).map(|b| b.narrowed.clone());
            // Module-level functions live in the outermost scope; a parameter
            // or local of the same name in an inner scope is a different value.
            let shadowed_locally = c.env.scope_of(name).is_some_and(|idx| idx > 0);
            if let Some(msg) = c
                .function_arity_info
                .get(name)
                .and_then(|info| info.deprecated.clone())
            {
                if !shadowed_locally && !matches!(bound, Some(Type::Class(_) | Type::Generic(..))) {
                    report(c, format!("{name}()"), &msg, span);
                    return;
                }
            }
            if !shadowed_locally
                && (c.classes.iter().any(|k| k == name) || c.class_shapes.contains_key(name))
            {
                if let Some(msg) = c
                    .class_shapes
                    .get(name)
                    .and_then(|shape| shape.deprecated.clone())
                {
                    report(c, format!("{name}()"), &msg, span);
                }
                return;
            }
            if let Some(t) = bound {
                let (msg, not_callable) = instance_call(c, &t);
                if let Some(msg) = msg {
                    report(c, format!("{name}()"), &msg, span);
                } else if not_callable {
                    c.not_callable(&t, span);
                }
            }
        }
        Expr::Attribute(a) => {
            let attr = a.attr.as_str();
            let recv = infer_expr_readonly(c, &a.value);
            if let Type::Module(module) = &recv {
                let Some(shapes) = c.module_registry.get(module) else {
                    return;
                };
                let msg = shapes
                    .function_arities
                    .get(attr)
                    .and_then(|info| info.deprecated.clone())
                    .or_else(|| {
                        shapes
                            .class_shapes
                            .get(attr)
                            .and_then(|shape| shape.deprecated.clone())
                    });
                if let Some(msg) = msg {
                    let local = match a.value.as_ref() {
                        Expr::Name(m) => m.id.as_str().to_owned(),
                        _ => module.clone(),
                    };
                    report(c, format!("{local}.{attr}()"), &msg, span);
                }
                return;
            }
            let Some(cls) = class_of(&recv) else {
                return;
            };
            // `obj.method(...)`: the method's own deprecation. `obj.prop(...)`
            // calls whatever the property returns, so judge that value; the
            // same for a field holding an instance.
            let member = c.find_method(&cls, attr).cloned();
            let value = match &member {
                Some(sig) if sig.is_property => sig.return_type.clone(),
                Some(sig) => {
                    if let Some(msg) = sig.arity_info.deprecated.clone() {
                        report(c, format!("{}.{attr}()", short(&cls)), &msg, span);
                    }
                    return;
                }
                None => match c.find_field(&cls, attr) {
                    Some(t) => t.clone(),
                    None => return,
                },
            };
            let (msg, not_callable) = instance_call(c, &value);
            if let Some(msg) = msg {
                report(c, format!("{}.{attr}()", short(&cls)), &msg, span);
            } else if not_callable {
                c.not_callable(&value, span);
            }
        }
        _ => {}
    }
}

/// Report a read of a deprecated property (`obj.prop`).
pub(super) fn check_attribute_read(c: &mut Checker, a: &ruff_python_ast::ExprAttribute) {
    if c.unsafe_depth > 0 {
        return;
    }
    let recv = infer_expr_readonly(c, &a.value);
    let Some(cls) = class_of(&recv) else {
        return;
    };
    let attr = a.attr.as_str();
    let Some(msg) = c
        .find_method(&cls, attr)
        .filter(|sig| sig.is_property)
        .and_then(|sig| sig.arity_info.deprecated.clone())
    else {
        return;
    };
    let span = (a.range().start().to_usize(), a.range().end().to_usize());
    report(c, format!("{}.{attr}", short(&cls)), &msg, span);
}

/// A class name as a user reads it: the last segment of a followed
/// third-party class's qualified name (`pydantic_ai.run.AgentRunResult` →
/// `AgentRunResult`).
fn short(cls: &str) -> &str {
    cls.rsplit('.').next().unwrap_or(cls)
}
