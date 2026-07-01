import re

with open("tyc/crates/tyc-types/src/lib.rs", "r") as f:
    content = f.read()

content = content.replace("fn collect_names_in_expr(expr: &Expr) -> std::collections::HashSet<String> {", "fn collect_names_in_expr<'a>(expr: &'a Expr) -> std::collections::HashSet<&'a str> {")
content = content.replace("struct V {\n        names: std::collections::HashSet<String>,\n    }", "struct V<'a> {\n        names: std::collections::HashSet<&'a str>,\n    }")
content = content.replace("impl<'a> SourceOrderVisitor<'a> for V {", "impl<'a> SourceOrderVisitor<'a> for V<'a> {")
content = content.replace("self.names.insert(n.id.as_str().to_owned());", "self.names.insert(n.id.as_str());")
content = content.replace("let mut v = V {\n        names: std::collections::HashSet::new(),\n    };", "let mut v = V::<'a> {\n        names: std::collections::HashSet::new(),\n    };")


# Now replace the calls
content = re.sub(r'for n in collect_names_in_expr\((.*?)\) \{\n(.*?)tracked\.remove\(&n\);', r'for n in collect_names_in_expr(\1) {\n\2tracked.remove(n);', content)

# 6372: get(n) -> get(*n), map(|info| (n.clone(), info.clone())) -> map(|info| (n.to_string(), info.clone()))
content = re.sub(r'\.get\(&?n\)\n\s*\.filter\(\|info\| !info\.missing\.is_empty\(\)\)\n\s*\.map\(\|info\| \(n\.clone\(\), info\.clone\(\)\)\)',
                 r'.get(*n)\n                .filter(|info| !info.missing.is_empty())\n                .map(|info| (n.to_string(), info.clone()))', content)

content = re.sub(r'\.any\(\|n\| c\.uninit_instances\.contains_key\(n\)\)', r'.any(|n| c.uninit_instances.contains_key(*n))', content)

with open("tyc/crates/tyc-types/src/lib.rs", "w") as f:
    f.write(content)
