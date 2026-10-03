//! Class declaration options used by member and operator contracts.
use super::*;
use ruff_python_ast::visitor::{self, Visitor};

pub(super) fn dataclass_option(c: &Checker, name: &str, option: &str) -> Option<bool> {
    struct Scan<'s> {
        name: &'s str,
        option: &'s str,
        result: Option<bool>,
    }
    impl<'a> Visitor<'a> for Scan<'_> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            if let Stmt::ClassDef(cd) = stmt {
                if cd.name.as_str() == self.name {
                    for decorator in &cd.decorator_list {
                        if matches!(&decorator.expression,Expr::Name(n) if n.id.as_str()=="dataclass")
                            || matches!(&decorator.expression,Expr::Attribute(a) if a.attr.as_str()=="dataclass")
                        {
                            self.result = Some(false);
                        }

                        if let Expr::Call(call) = &decorator.expression {
                            let is_dataclass = match call.func.as_ref() {
                                Expr::Name(n) => n.id.as_str() == "dataclass",
                                Expr::Attribute(a) => a.attr.as_str() == "dataclass",
                                _ => false,
                            };
                            if is_dataclass {
                                self.result = Some(call.arguments.keywords.iter().any(|kw| {
                                    kw.arg.as_ref().is_some_and(|n| n.as_str() == self.option)
                                        && matches!(kw.value,Expr::BooleanLiteral(ref b) if b.value)
                                }));
                            }
                        }
                    }
                }
            }
            visitor::walk_stmt(self, stmt);
        }
    }
    let mut scan = Scan {
        name,
        option,
        result: None,
    };
    for stmt in &c.module?.body {
        scan.visit_stmt(stmt);
    }
    scan.result
}

/// IntEnum/StrEnum keys compare and hash like their scalar values.
pub(super) fn enum_key_compatible(c: &Checker, key: &Type, index: &Type) -> bool {
    let Type::Class(name) = key else { return false };
    c.enums.contains_key(name)
        && match index {
            Type::Int | Type::Bool => c.class_derives_from_builtin(
                name,
                &["int", "IntEnum", "enum.IntEnum", "IntFlag", "enum.IntFlag"],
            ),
            Type::Str | Type::LitStr(_) => {
                c.class_derives_from_builtin(name, &["str", "StrEnum", "enum.StrEnum"])
            }
            _ => false,
        }
}

pub(super) fn method_accepts_contract(expected: &MethodSig, actual: &MethodSig) -> bool {
    let e = &expected.arity_info;
    let a = &actual.arity_info;
    if a.min_positional > e.min_positional {
        return false;
    }
    match (e.max_positional, a.max_positional) {
        (None, Some(_)) => return false,
        (Some(e), Some(a)) if a < e => return false,
        _ => {}
    }
    if e.has_kwarg && !a.has_kwarg {
        return false;
    }
    for (i, name) in e.param_names.iter().enumerate() {
        if i >= e.posonly_count && (i < a.posonly_count || a.param_names.get(i) != Some(name)) {
            return false;
        }
    }
    for name in &e.kwonly_names {
        if !a.kwonly_names.contains(name)
            && !a
                .param_names
                .iter()
                .skip(a.posonly_count)
                .any(|n| n == name)
            && !a.has_kwarg
        {
            return false;
        }
    }
    if a.kwonly_required
        .iter()
        .any(|name| !e.kwonly_required.contains(name))
    {
        return false;
    }
    true
}

pub(super) fn property(c: &Checker, class: &str, field: &str) -> Option<(Type, Option<Type>)> {
    fn own(c: &Checker, class: &str, field: &str) -> (Option<Type>, Option<Type>) {
        struct Scan<'a, 'c> {
            c: &'a Checker<'c>,
            class: &'a str,
            field: &'a str,
            getter: Option<Type>,
            setter: Option<Type>,
        }
        impl<'s> Visitor<'s> for Scan<'_, '_> {
            fn visit_stmt(&mut self, stmt: &'s Stmt) {
                if let Stmt::ClassDef(cd) = stmt {
                    let real = cd
                        .name
                        .as_str()
                        .strip_prefix("__typhon_impl_")
                        .unwrap_or(cd.name.as_str());
                    if real == self.class {
                        let tps = self
                            .c
                            .class_type_params
                            .get(self.class)
                            .cloned()
                            .unwrap_or_default();
                        for stmt in &cd.body {
                            if let Stmt::FunctionDef(f) = stmt {
                                if f.name.as_str() != self.field {
                                    continue;
                                }
                                if is_property_getter(f) {
                                    self.getter = Some(
                                        f.returns
                                            .as_deref()
                                            .map(|r| {
                                                type_from_annotation_with_params(
                                                    r,
                                                    &self.c.classes,
                                                    &tps,
                                                )
                                            })
                                            .unwrap_or(Type::Unknown),
                                    );
                                    let readonly=f.decorator_list.iter().any(|d|matches!(&d.expression,Expr::Name(n) if n.id.as_str()=="property") || matches!(&d.expression,Expr::Attribute(a) if a.attr.as_str()=="property"));
                                    if !readonly {
                                        self.setter = self.getter.clone();
                                    }
                                } else if is_property_setter(f) {
                                    self.setter = Some(
                                        f.parameters
                                            .posonlyargs
                                            .iter()
                                            .chain(&f.parameters.args)
                                            .nth(1)
                                            .and_then(|p| p.parameter.annotation.as_deref())
                                            .map(|r| {
                                                type_from_annotation_with_params(
                                                    r,
                                                    &self.c.classes,
                                                    &tps,
                                                )
                                            })
                                            .unwrap_or(Type::Unknown),
                                    );
                                }
                            }
                        }
                    }
                }
                visitor::walk_stmt(self, stmt);
            }
        }
        let mut scan = Scan {
            c,
            class,
            field,
            getter: None,
            setter: None,
        };
        if let Some(module) = c.module {
            for stmt in &module.body {
                scan.visit_stmt(stmt);
            }
        }
        (scan.getter, scan.setter)
    }
    fn resolve(
        c: &Checker,
        class: &str,
        field: &str,
        seen: &mut HashSet<String>,
    ) -> Option<(Type, Option<Type>)> {
        if !seen.insert(class.into()) {
            return None;
        }
        let (getter, setter) = own(c, class, field);
        if let Some(getter) = getter {
            return Some((getter, setter));
        }
        if let Some(shape) = c.class_shapes.get(class) {
            for base in &shape.bases {
                if let Some((getter, inherited_setter)) = resolve(c, base, field, seen) {
                    return Some((getter, setter.or(inherited_setter)));
                }
            }
        }
        None
    }
    resolve(c, class, field, &mut HashSet::new())
}

pub(super) fn class_object(c: &Checker, expr: &Expr) -> bool {
    let Expr::Name(n) = expr else { return false };
    let Some(binding) = c.env.lookup(n.id.as_str()) else {
        return false;
    };
    c.resolved.scopes.iter().flat_map(|s| &s.bindings).any(|b| {
        b.name == n.id.as_str() && b.kind == BindingKind::Class && b.span.0 == binding.span.0
    })
}

pub(super) fn classvar(c: &Checker, class: &str, field: &str) -> bool {
    let mut stack = vec![class.to_owned()];
    let mut seen = HashSet::new();
    while let Some(class) = stack.pop() {
        if !seen.insert(class.clone()) {
            continue;
        }
        if c.class_var_attrs
            .get(&class)
            .is_some_and(|fields| fields.contains(field))
        {
            return true;
        }
        if let Some(shape) = c.class_shapes.get(&class) {
            stack.extend(shape.bases.iter().cloned());
        }
    }
    false
}

pub(super) fn call_kwarg_type(c: &Checker, func: &Expr) -> Option<Type> {
    let Expr::Attribute(a) = func else {
        return None;
    };
    let receiver = infer_expr_readonly(c, &a.value);
    let class = match &receiver {
        Type::Class(n) | Type::Generic(n, _) => n.as_str(),
        _ => return None,
    };
    struct Scan<'c, 's> {
        c: &'c Checker<'s>,
        class: &'c str,
        method: &'c str,
        result: Option<Type>,
    }
    impl<'a> Visitor<'a> for Scan<'_, '_> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            if let Stmt::ClassDef(cd) = stmt {
                if cd.name.as_str() == self.class
                    || cd.name.as_str() == format!("__typhon_impl_{}", self.class)
                {
                    for stmt in &cd.body {
                        if let Stmt::FunctionDef(f) = stmt {
                            if f.name.as_str() == self.method {
                                self.result = f
                                    .parameters
                                    .kwarg
                                    .as_ref()
                                    .and_then(|p| p.annotation.as_deref())
                                    .map(|a| {
                                        type_from_annotation_with_params(
                                            a,
                                            &self.c.classes,
                                            &type_param_names_from(cd.type_params.as_deref()),
                                        )
                                    });
                            }
                        }
                    }
                }
            }
            visitor::walk_stmt(self, stmt);
        }
    }
    let mut scan = Scan {
        c,
        class,
        method: a.attr.as_str(),
        result: None,
    };
    for stmt in &c.module?.body {
        scan.visit_stmt(stmt);
    }
    scan.result
}
