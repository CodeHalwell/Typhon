## Bolt Journal
## 2025-03-27 - Refactor module_level_bound_names return type in tyc-desugar
**Learning:** Returning `HashSet<String>` from `module_level_bound_names` requires creating many owned copies of strings when traversing the AST, whereas `HashSet<&str>` avoids allocation by borrowing directly from `ruff_python_ast::Expr::Name` and `Stmt::Import` via string references (the nodes have a tied lifetime). This follows the specific project memory: "use `HashSet<&str>` to borrow the strings directly from the AST nodes with the node's lifetime, preventing multiple redundant heap allocations during AST traversal in hot paths."
**Action:** When implementing or modifying AST walkers, avoid `.to_owned()` if returning strings inside collections, and use borrowed references whenever possible.
