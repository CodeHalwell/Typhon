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

/// Every `class` statement in the module, nested ones included, in source
/// (pre-order) order. Built once per check: [`property`] runs for each
/// attribute read, and walking the whole module for each one made a long
/// attribute chain cubic in its length.
fn class_defs<'c, 'a>(c: &'c Checker<'a>) -> &'c [&'a ruff_python_ast::StmtClassDef] {
    struct Collect<'a>(Vec<&'a ruff_python_ast::StmtClassDef>);
    impl<'a> Visitor<'a> for Collect<'a> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            if let Stmt::ClassDef(cd) = stmt {
                self.0.push(cd);
            }
            visitor::walk_stmt(self, stmt);
        }
        // No statement lives inside an expression.
        fn visit_expr(&mut self, _: &'a Expr) {}
    }
    let Some(module) = c.module else {
        return &[];
    };
    c.class_defs.get_or_init(|| {
        let mut collect = Collect(Vec::new());
        for stmt in &module.body {
            collect.visit_stmt(stmt);
        }
        collect.0
    })
}

pub(super) fn property(c: &Checker, class: &str, field: &str) -> Option<(Type, Option<Type>)> {
    fn own(c: &Checker, class: &str, field: &str) -> (Option<Type>, Option<Type>) {
        let mut getter: Option<Type> = None;
        let mut setter: Option<Type> = None;
        let mut tps: Option<Vec<String>> = None;
        for cd in class_defs(c) {
            let real = cd
                .name
                .as_str()
                .strip_prefix("__typhon_impl_")
                .unwrap_or(cd.name.as_str());
            if real != class {
                continue;
            }
            for stmt in &cd.body {
                let Stmt::FunctionDef(f) = stmt else {
                    continue;
                };
                if f.name.as_str() != field {
                    continue;
                }
                let tps = tps.get_or_insert_with(|| {
                    c.class_type_params.get(class).cloned().unwrap_or_default()
                });
                if is_property_getter(f) {
                    getter = Some(
                        f.returns
                            .as_deref()
                            .map(|r| type_from_annotation_with_params(r, &c.classes, tps))
                            .unwrap_or(Type::Unknown),
                    );
                    let readonly = f.decorator_list.iter().any(|d| {
                        matches!(&d.expression, Expr::Name(n) if n.id.as_str() == "property")
                            || matches!(&d.expression, Expr::Attribute(a) if a.attr.as_str() == "property")
                    });
                    if !readonly {
                        setter = getter.clone();
                    }
                } else if is_property_setter(f) {
                    setter = Some(
                        f.parameters
                            .posonlyargs
                            .iter()
                            .chain(&f.parameters.args)
                            .nth(1)
                            .and_then(|p| p.parameter.annotation.as_deref())
                            .map(|r| type_from_annotation_with_params(r, &c.classes, tps))
                            .unwrap_or(Type::Unknown),
                    );
                }
            }
        }
        (getter, setter)
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
    callables::class_name(c, n.id.as_str()).is_some()
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

pub(super) fn descriptor_used(c: &Checker, class: &str, field: &str) -> bool {
    struct Scan<'c, 's> {
        c: &'c Checker<'s>,
        uses: HashSet<(String, String)>,
    }
    impl<'a> Visitor<'a> for Scan<'_, '_> {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Attribute(a) = expr {
                if let Expr::Name(n) = a.value.as_ref() {
                    let mut scope =
                        Some(self.c.resolved.scope_at_offset(n.range.start().to_usize()));
                    while let Some(id) = scope {
                        let s = &self.c.resolved.scopes[id];
                        if let Some(b) = s.lookup_local(n.id.as_str()) {
                            if b.kind == BindingKind::Class {
                                self.uses
                                    .insert((n.id.as_str().to_owned(), a.attr.as_str().to_owned()));
                            }
                            break;
                        }
                        scope = s.parent;
                    }
                }
            }
            visitor::walk_expr(self, expr);
        }
    }
    c.descriptor_uses
        .get_or_init(|| {
            let mut scan = Scan {
                c,
                uses: HashSet::new(),
            };
            if let Some(module) = c.module {
                for stmt in &module.body {
                    scan.visit_stmt(stmt);
                }
            }
            scan.uses
        })
        .contains(&(class.to_owned(), field.to_owned()))
}
