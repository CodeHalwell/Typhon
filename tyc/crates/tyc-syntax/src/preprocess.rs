//! Pre-processing pass: strip Typhon-specific syntax so the underlying
//! Python parser can handle the source.
//!
//! Transformations performed here:
//!
//! 1. **`let` / `mut` line prefixes** — `let x: T = expr` and
//!    `mut x: T = expr` are reduced to `x: T = expr`. The stripped keyword
//!    is recorded so the formatter can restore it.
//!
//! 2. **`T?` nullability sugar** — every `?` outside a string or comment is
//!    replaced with ` | None`, so an annotation like `email: str?` becomes
//!    `email: str | None`. Positions are recorded so the formatter can
//!    restore the `?` sugar exactly.
//!
//! 3. **`model X:` class declarations** — `model ClassName:` is rewritten to
//!    `class ClassName(BaseModel):`. The preprocessor records the line index
//!    so the formatter can restore the `model` keyword.
//!
//! 4. **`comptime let/mut X: T = expr`** — the `comptime` prefix (and the
//!    immediately following `let`/`mut`) is stripped, leaving `X: T = expr`.
//!    The binding name and line index are recorded so the build command can
//!    evaluate the RHS at compile time and inline the result.
//!
//! A separate function [`expand_question_ops`] rewrites `expr?` (the error-
//! propagation operator) into the multi-statement guard form before the main
//! pass runs. It is called explicitly by pipeline commands that need valid
//! Python output (`tyc build`, `tyc check`) but *not* by `tyc fmt`, which
//! must preserve the Typhon source unchanged.
//!
//! Anything else is left untouched.

use crate::lexer::TyphonKeyword;

/// One stripped keyword and the 0-based line index in the source where it
/// appeared.
#[derive(Debug, Clone)]
pub struct StrippedKeyword {
    /// 0-based line index in both the original source and the preprocessed
    /// source (stripping a keyword never changes the line count).
    pub line_index: usize,
    pub keyword: TyphonKeyword,
}

/// A `?` that was rewritten to ` | None` during preprocessing, with the
/// information needed to put it back.
#[derive(Debug, Clone)]
pub struct StrippedOptional {
    /// 0-based line index of the original `?`.
    pub line_index: usize,
    /// 0-based column where the `T | None` substitution begins on that line
    /// in the *pre-processed* source (i.e. immediately after the type name).
    pub python_col: usize,
}

/// A `lazy import` declaration — `lazy import ALIAS = MODULE`.
///
/// At check/format time the preprocessor converts this to `import MODULE as
/// ALIAS` so the rest of the pipeline sees a standard Python import.  At build
/// time [`expand_lazy_imports`] replaces it with a thread-safe, on-first-use
/// loader before the main preprocess pass runs.
#[derive(Debug, Clone)]
pub struct LazyImport {
    /// 0-based line index in both the original and preprocessed source.
    pub line_index: usize,
    /// The local alias (`np` in `lazy import np = numpy`).
    pub alias: String,
    /// The module being imported (`numpy` in `lazy import np = numpy`).
    pub module: String,
}

/// A `comptime` binding whose RHS must be evaluated at build time.
#[derive(Debug, Clone)]
pub struct ComptimeBinding {
    /// The name of the comptime variable (e.g. `PORT` from `comptime val PORT: int = …`).
    pub name: String,
    /// 0-based line index of the binding in the **preprocessed** source (after
    /// keyword stripping). This is identical to the original-source line index
    /// because keyword stripping never changes the line count.
    pub line_index: usize,
}

/// Result of pre-processing a Typhon source file.
#[derive(Debug)]
pub struct PreprocessResult {
    /// The Python-compatible source (val/var stripped, `?` rewritten).
    pub python_source: String,
    /// Metadata about every stripped keyword, in source order.
    pub stripped: Vec<StrippedKeyword>,
    /// Metadata about every `?` that was rewritten, in source order.
    pub optionals: Vec<StrippedOptional>,
    /// Bindings that were declared `comptime` and whose RHS the build command
    /// must evaluate and inline.
    pub comptime_bindings: Vec<ComptimeBinding>,
    /// Names of `comptime def` functions declared at module top level.
    /// The analyser turns these into a registry so the comptime
    /// evaluator can dispatch into them when a `comptime let` binding's
    /// RHS references the function. Bodies are read from the parsed AST
    /// at evaluation time; this list only carries the function names
    /// (which the evaluator uses to decide which `def` statements are
    /// callable from a comptime context).
    pub comptime_functions: Vec<String>,
    /// Lazy import declarations, in source order.  Each entry records the
    /// alias and module so [`postprocess`] can restore the original
    /// `lazy import ALIAS = MODULE` syntax.
    pub lazy_imports: Vec<LazyImport>,
    /// 0-based line indices on which an `unsafe:` block opens.  Both the
    /// original and preprocessed sources share line numbering, so these
    /// indices can be used by downstream passes (resolver, type checker)
    /// to locate the corresponding `if True:` statement in the parsed AST.
    pub unsafe_lines: Vec<usize>,
    /// 0-based line indices on which a `class!` (raw class) declaration
    /// appears.  The preprocessor strips the `!` so the Python parser sees
    /// a plain `class Foo:`; downstream passes consult this list to know
    /// the class should NOT receive a `@dataclass` decorator at desugar
    /// time.
    pub raw_class_lines: Vec<usize>,
    /// 0-based line indices on which a `class NAME frozen:` declaration
    /// appears.  The preprocessor strips the `frozen` modifier so the
    /// Python parser sees a plain `class Foo:` / `class Foo(Base):`;
    /// downstream passes consult this list to emit
    /// `@dataclasses.dataclass(slots=True, frozen=True)` instead of the
    /// default decorator.
    pub frozen_class_lines: Vec<usize>,
    /// 0-based line indices on which a `plain class NAME(...):`
    /// declaration appears.  The preprocessor strips the leading
    /// `plain ` so the Python parser sees an ordinary
    /// `class NAME(...):`; downstream passes consult this list to know
    /// the class should NOT receive a `@dataclass` decorator and that no
    /// `__init__` should be synthesised — the body is emitted verbatim.
    pub plain_class_lines: Vec<usize>,
    /// Names of module-level declarations annotated with `pub`.  Drives
    /// `__all__` synthesis in the desugar pass: modules with at least
    /// one `pub` symbol get an `__all__ = [...]` list emitted at the
    /// top; modules with none are left alone, preserving the current
    /// `from foo import *` semantics for legacy `.ty` files.
    pub pub_names: Vec<String>,
    /// 0-based line indices where the source contained a `pub *`
    /// wildcard-re-export statement. Only legal in `__init__.ty`; the
    /// build orchestrator aggregates each direct-sibling module's
    /// `pub_names` into a synthesised `from .sibling import …` block
    /// at this position. Outside `__init__.ty` the build emits an
    /// advice-level `tyc::pub_star_outside_init` diagnostic.
    pub pub_star_lines: Vec<usize>,
}

/// Convert a list of 0-based line indices into byte offsets pointing at
/// the first non-whitespace character on each line of `source`.  The
/// returned vector is sorted so callers can binary-search it.  Mirrors
/// the private `unsafe_byte_starts` helper in `tyc-types`; lifted here
/// so multiple consumers (desugar, type-checker) can share one
/// implementation.
pub fn line_byte_starts(source: &str, lines: &[usize]) -> Vec<u32> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut line_starts: Vec<usize> = vec![0];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let mut starts: Vec<u32> = Vec::with_capacity(lines.len());
    for &line in lines {
        if let Some(&offset) = line_starts.get(line) {
            let rest = &source[offset..];
            let lead = rest
                .bytes()
                .take_while(|&b| b == b' ' || b == b'\t')
                .count();
            starts.push((offset + lead) as u32);
        }
    }
    starts.sort_unstable();
    starts
}

/// Strip Typhon-specific syntax from `source` and return the Python-
/// compatible string together with restoration metadata.
pub fn preprocess(source: &str) -> PreprocessResult {
    // Pre-pass: walk every line, strip a leading `pub ` modifier (at
    // module level — i.e. zero indentation), record the declared name,
    // and feed the rest of the pipeline a source string with `pub ` no
    // longer present. The line indices stay aligned because we only
    // mutate the leading prefix. A StrippedKeyword::Pub entry is
    // emitted so `postprocess` restores `pub ` for `tyc fmt`.
    let mut pub_names: Vec<String> = Vec::new();
    let mut pub_lines: Vec<usize> = Vec::new();
    let mut pub_star_lines: Vec<usize> = Vec::new();
    let source_owned =
        strip_pub_prefixes(source, &mut pub_names, &mut pub_lines, &mut pub_star_lines);
    let source = source_owned.as_str();

    let mut python_source = String::with_capacity(source.len());
    let mut stripped = Vec::new();
    let mut optionals = Vec::new();
    let mut comptime_bindings = Vec::new();
    let mut comptime_functions: Vec<String> = Vec::new();
    let mut lazy_imports = Vec::new();
    let mut unsafe_lines: Vec<usize> = Vec::new();
    let mut raw_class_lines: Vec<usize> = Vec::new();
    let mut frozen_class_lines: Vec<usize> = Vec::new();
    let mut plain_class_lines: Vec<usize> = Vec::new();
    // String state carried across lines (triple-quoted strings may span them).
    let mut in_string: Option<StringMode> = None;
    // When a `freeze let` RHS spans multiple lines (e.g. a multi-line dict
    // literal), the opening `__typhon_freeze__(` is emitted on the first
    // line but the matching `)` has to land *after* the closing bracket of
    // the RHS expression. `freeze_let_depth` tracks the residual bracket
    // depth left over from earlier `freeze let` lines; when it returns to
    // zero we close the call by appending `)` to the current line.
    let mut freeze_let_depth: i32 = 0;

    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        // Keyword stripping only applies outside of string content.
        let line_after_keyword = if in_string.is_none() {
            let indent_len = line
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(line.len());
            let indent = &line[..indent_len];
            let rest = &line[indent_len..];

            // ── `freeze let NAME[: T] = EXPR` → `let NAME[: T] = __typhon_freeze__(EXPR)` ─
            // Wraps the RHS in a runtime deep-freeze call. Module-level
            // only for v1; in-function uses fall through to the Python
            // parser (which rejects `freeze let` as a syntax error).
            // The `let` keyword is preserved so the existing
            // binding-immutability machinery still fires; `freeze` adds
            // the recursive value-immutability layer on top.
            if indent_len == 0 {
                if let Some(after_raw) = rest.strip_prefix("freeze let ") {
                    let after = after_raw.trim_end_matches(['\n', '\r']);
                    if let Some(wrap) = wrap_freeze_let_with_depth(after) {
                        stripped.push(StrippedKeyword {
                            line_index,
                            keyword: TyphonKeyword::Freeze,
                        });
                        // If the RHS opened more brackets than it closed
                        // on this line, postpone the closing `)` and let
                        // the residual-depth tracker emit it on the line
                        // that brings the depth back to zero. The wrap
                        // helper splits the rewrite into a `head`
                        // (`X = __typhon_freeze__(rhs`) and a `tail`
                        // (`<comment>` or empty) so we can place the
                        // closing `)` between them on the single-line
                        // case.
                        let new_line = if wrap.residual > 0 {
                            freeze_let_depth = wrap.residual;
                            // Comment-on-open-line is rejected by
                            // wrap_freeze_let_with_depth in the
                            // multi-line case, so `tail` is empty here.
                            debug_assert!(wrap.tail.is_empty());
                            format!("{}let {}\n", indent, wrap.head)
                        } else {
                            format!("{}let {}){}\n", indent, wrap.head, wrap.tail)
                        };
                        let (rewritten, marks) = rewrite_optionals(&new_line, &mut in_string);
                        for col in marks {
                            optionals.push(StrippedOptional {
                                line_index,
                                python_col: col,
                            });
                        }
                        python_source.push_str(&rewritten);
                        continue;
                    }
                }
            }
            // Already inside a multi-line `freeze let` RHS — adjust the
            // depth by this line's net brackets. When the depth hits zero
            // we emit a closing `)` after the bracket that closed it; on
            // earlier lines we forward the content verbatim.
            if freeze_let_depth > 0 {
                let line_no_nl = line.trim_end_matches(['\n', '\r']);
                let net = bracket_delta_outside_strings(line_no_nl, &mut in_string);
                freeze_let_depth += net;
                if freeze_let_depth <= 0 {
                    freeze_let_depth = 0;
                    let trailing = &line[line_no_nl.len()..];
                    let emitted = format!("{})", line_no_nl);
                    python_source.push_str(&emitted);
                    python_source.push_str(trailing);
                } else {
                    python_source.push_str(line);
                }
                continue;
            }

            // ── `newtype Name = Base` → `Name = NewType("Name", Base)` ───────
            // Module-level only. The desugar pass injects
            // `from typing import NewType` when at least one such
            // declaration is detected in the emitted AST. The type
            // checker recognises the pattern directly and registers
            // `Name` as a nominal newtype distinct from `Base`.
            if indent_len == 0 {
                if let Some(after_raw) = rest.strip_prefix("newtype ") {
                    let after = after_raw.trim_end_matches(['\n', '\r']);
                    if let Some((name, base)) = parse_newtype_decl(after) {
                        stripped.push(StrippedKeyword {
                            line_index,
                            keyword: TyphonKeyword::Newtype,
                        });
                        let new_line = format!("{} = NewType(\"{}\", {})\n", name, name, base);
                        let (rewritten, marks) = rewrite_optionals(&new_line, &mut in_string);
                        for col in marks {
                            optionals.push(StrippedOptional {
                                line_index,
                                python_col: col,
                            });
                        }
                        python_source.push_str(&rewritten);
                        continue;
                    }
                    // Unrecognised `newtype` form — fall through to produce
                    // a parse error from the Python parser.
                }
            }

            // ── `lazy import ALIAS = MODULE` → `import MODULE as ALIAS` ─────
            // Only recognised at module level (indent_len == 0) so that
            // indented `lazy` expressions (rare but valid Python identifiers)
            // are not mistakenly rewritten.
            if indent_len == 0 {
                if let Some(after_raw) = rest.strip_prefix("lazy import ") {
                    let after = after_raw.trim_end_matches(['\n', '\r']);
                    if let Some((alias, module)) = parse_lazy_import(after) {
                        lazy_imports.push(LazyImport {
                            line_index,
                            alias: alias.clone(),
                            module: module.clone(),
                        });
                        // Emit a standard Python import so downstream passes see a
                        // valid import statement.
                        let new_line = format!("import {} as {}\n", module, alias);
                        python_source.push_str(&new_line);
                        continue;
                    }
                    // Unrecognised `lazy import` form — fall through to produce a
                    // parse error from the Python parser.
                }
            }

            // ── `impl ClassName:` → `class __typhon_impl_ClassName(object):` ─
            // Also `impl[T, U] ClassName[T, U]:` for generic impl blocks
            // (PEP 695); the type parameters are forwarded to the pseudo
            // class so methods can resolve `T`/`U` while the desugar pass
            // merges them back into the real `ClassName[T, U]`.
            let after_impl_kw = if let Some(s) = rest.strip_prefix("impl ") {
                Some(s)
            } else if rest.starts_with("impl[") {
                Some(&rest["impl".len()..])
            } else {
                None
            };
            if let Some(after_impl) = after_impl_kw {
                let first = after_impl.as_bytes().first().copied().unwrap_or(0);
                if first.is_ascii_alphanumeric() || first == b'_' || first == b'[' {
                    if let Some(class_header) = make_impl_class_line(after_impl) {
                        stripped.push(StrippedKeyword {
                            line_index,
                            keyword: TyphonKeyword::Impl,
                        });
                        let new_line = format!("{}class {}", indent, class_header);
                        let (rewritten, marks) = rewrite_optionals(&new_line, &mut in_string);
                        for col in marks {
                            optionals.push(StrippedOptional {
                                line_index,
                                python_col: col,
                            });
                        }
                        python_source.push_str(&rewritten);
                        continue;
                    }
                }
            }

            // ── `extend ClassName:` → `class __typhon_impl_ClassName(object):` ─
            // For Typhon v1, `extend` on user-defined classes is an alias for
            // `impl` — the methods are merged into the target class at desugar.
            // For built-ins (`extend str:`, `extend list:`, …) the same
            // preprocess emits a `class __typhon_builtin_ext_BUILTIN:` stub
            // that a later analyse pass extracts to free functions and uses
            // to drive call-site rewriting.
            if rest.starts_with("extend ")
                && rest.len() > "extend ".len()
                && (rest.as_bytes()["extend ".len()].is_ascii_alphanumeric()
                    || rest.as_bytes()["extend ".len()] == b'_')
            {
                let after_extend = &rest["extend ".len()..];
                let target = extract_extend_target(after_extend);
                let header_opt = if target.map(is_builtin_extend_target).unwrap_or(false) {
                    make_builtin_extend_class_line(after_extend)
                } else {
                    make_impl_class_line(after_extend)
                };
                if let Some(class_header) = header_opt {
                    stripped.push(StrippedKeyword {
                        line_index,
                        keyword: TyphonKeyword::Extend,
                    });
                    let new_line = format!("{}class {}", indent, class_header);
                    let (rewritten, marks) = rewrite_optionals(&new_line, &mut in_string);
                    for col in marks {
                        optionals.push(StrippedOptional {
                            line_index,
                            python_col: col,
                        });
                    }
                    python_source.push_str(&rewritten);
                    continue;
                }
            }

            // ── `interface Name:` → `class Name(Protocol):` ─────────────────
            if rest.starts_with("interface ")
                && rest.len() > "interface ".len()
                && (rest.as_bytes()["interface ".len()].is_ascii_alphanumeric()
                    || rest.as_bytes()["interface ".len()] == b'_')
            {
                let after_iface = &rest["interface ".len()..];
                if let Some(class_header) = make_interface_class_line(after_iface) {
                    stripped.push(StrippedKeyword {
                        line_index,
                        keyword: TyphonKeyword::Interface,
                    });
                    let new_line = format!("{}class {}", indent, class_header);
                    let (rewritten, marks) = rewrite_optionals(&new_line, &mut in_string);
                    for col in marks {
                        optionals.push(StrippedOptional {
                            line_index,
                            python_col: col,
                        });
                    }
                    python_source.push_str(&rewritten);
                    continue;
                }
            }

            // ── interface-style `def NAME(...) -> T` (no body) ──────────────
            // The docs show declaration-only methods inside `interface` blocks
            // (`def draw() -> None`). The Python parser rejects a `def` line
            // with no body, so we auto-append `: ...` here. The same rewrite
            // applies inside `class!`/`class` bodies too — `def f() -> int`
            // without a body is invalid Python anyway, and `: ...` is a strict
            // upgrade that never changes a valid program's meaning.
            if let Some(with_body) = append_ellipsis_to_bodiless_def(line) {
                let (rewritten, marks) = rewrite_optionals(&with_body, &mut in_string);
                for col in marks {
                    optionals.push(StrippedOptional {
                        line_index,
                        python_col: col,
                    });
                }
                python_source.push_str(&rewritten);
                continue;
            }

            // ── `unsafe:` → `if True:  # __typhon_unsafe__` ────────────────
            // The body is a no-op wrapper that preserves Python scoping. The
            // type checker tracks the marker so it can permit `Any` to flow
            // freely inside, and require explicit assignments at the boundary.
            if rest.starts_with("unsafe")
                && (rest.len() == "unsafe".len()
                    || matches!(
                        rest.as_bytes().get("unsafe".len()).copied(),
                        Some(b':') | Some(b' ') | Some(b'\t')
                    ))
            {
                if let Some(line_body) = rewrite_unsafe_block_line(rest) {
                    stripped.push(StrippedKeyword {
                        line_index,
                        keyword: TyphonKeyword::Unsafe,
                    });
                    unsafe_lines.push(line_index);
                    let new_line = format!("{}{}", indent, line_body);
                    let (rewritten, marks) = rewrite_optionals(&new_line, &mut in_string);
                    for col in marks {
                        optionals.push(StrippedOptional {
                            line_index,
                            python_col: col,
                        });
                    }
                    python_source.push_str(&rewritten);
                    continue;
                }
            }

            // ── `plain class ClassName(...):` → `class ClassName(...):` ─────
            // The `plain ` prefix is stripped and the line index is recorded
            // so the desugar pass knows to skip its automatic `@dataclass`
            // injection on this class.  Unlike `class!`, the body is left
            // exactly as written — no `__init__` is synthesised.
            if rest.starts_with("plain class ")
                && rest.len() > "plain class ".len()
                && (rest.as_bytes()["plain class ".len()].is_ascii_alphanumeric()
                    || rest.as_bytes()["plain class ".len()] == b'_')
            {
                stripped.push(StrippedKeyword {
                    line_index,
                    keyword: TyphonKeyword::PlainClass,
                });
                plain_class_lines.push(line_index);
                let after_marker = &rest["plain ".len()..]; // "class NAME(...):\n"
                let new_line = format!("{}{}", indent, after_marker);
                let (rewritten, marks) = rewrite_optionals(&new_line, &mut in_string);
                for col in marks {
                    optionals.push(StrippedOptional {
                        line_index,
                        python_col: col,
                    });
                }
                python_source.push_str(&rewritten);
                continue;
            }

            // ── `class! ClassName(...):` → `class ClassName(...):` ──────────
            // The `!` is stripped and the line index is recorded so the
            // desugar pass knows to skip its automatic `@dataclass` injection
            // on this class.
            if rest.starts_with("class! ")
                && rest.len() > "class! ".len()
                && (rest.as_bytes()["class! ".len()].is_ascii_alphanumeric()
                    || rest.as_bytes()["class! ".len()] == b'_')
            {
                stripped.push(StrippedKeyword {
                    line_index,
                    keyword: TyphonKeyword::RawClass,
                });
                raw_class_lines.push(line_index);
                let after_marker = &rest["class! ".len()..];
                let new_line = format!("{}class {}", indent, after_marker);
                let (rewritten, marks) = rewrite_optionals(&new_line, &mut in_string);
                for col in marks {
                    optionals.push(StrippedOptional {
                        line_index,
                        python_col: col,
                    });
                }
                python_source.push_str(&rewritten);
                continue;
            }

            // ── `class NAME frozen:` / `class NAME frozen(BASES):` ──────────
            // Strip the `frozen` modifier and record the line so the desugar
            // pass can emit `@dataclasses.dataclass(slots=True, frozen=True)`
            // for this class. Plain `class NAME:` (no modifier) is left to
            // fall through to the Python parser unchanged.
            if rest.starts_with("class ") && rest.contains(" frozen") {
                if let Some(rewritten_class) = strip_frozen_modifier(rest) {
                    frozen_class_lines.push(line_index);
                    let new_line = format!("{}{}", indent, rewritten_class);
                    let (rewritten, marks) = rewrite_optionals(&new_line, &mut in_string);
                    for col in marks {
                        optionals.push(StrippedOptional {
                            line_index,
                            python_col: col,
                        });
                    }
                    python_source.push_str(&rewritten);
                    continue;
                }
            }

            // ── `guard NAME = EXPR else: BODY` (single-line) ────────────────
            // Lowers to a None-check + an early-return body + a `let`
            // binding of the narrowed value. Stashes EXPR in a per-line
            // temp so the narrowing applies to the binding (the checker
            // can only narrow Name expressions, not arbitrary call
            // results).
            //
            //   guard w = weight else: return
            //
            //   →   let __typhon_guard_<N> = (weight)
            //       if __typhon_guard_<N> is None: return
            //       let w = __typhon_guard_<N>
            //
            // Only the single-line form is recognised; multi-line guards
            // (`else:\n    return`) are deferred.
            if rest.starts_with("guard ") {
                if let Some(rewritten) = expand_guard_one_liner(rest, indent, line_index) {
                    let (rewritten, marks) = rewrite_optionals(&rewritten, &mut in_string);
                    for col in marks {
                        optionals.push(StrippedOptional {
                            line_index,
                            python_col: col,
                        });
                    }
                    python_source.push_str(&rewritten);
                    continue;
                }
            }

            // ── `model ClassName:` → `class ClassName(BaseModel):` ──────────
            if rest.starts_with("model ")
                && rest.len() > "model ".len()
                && (rest.as_bytes()["model ".len()].is_ascii_alphanumeric()
                    || rest.as_bytes()["model ".len()] == b'_')
            {
                let after_model = &rest["model ".len()..]; // e.g. "User:\n"
                                                           // Find the `:` that opens the class body (last `:` not inside
                                                           // parentheses/brackets on this line).
                if let Some(class_body) = make_model_class_line(after_model) {
                    stripped.push(StrippedKeyword {
                        line_index,
                        keyword: TyphonKeyword::Model,
                    });
                    let new_line = format!("{}class {}", indent, class_body);
                    // Fall through to optional rewriting with the new line.
                    let (rewritten, marks) = rewrite_optionals(&new_line, &mut in_string);
                    for col in marks {
                        optionals.push(StrippedOptional {
                            line_index,
                            python_col: col,
                        });
                    }
                    python_source.push_str(&rewritten);
                    continue;
                }
            }

            // ── `comptime [let|mut|def] name…` → strip only the `comptime`
            // prefix; any inner `let`/`mut` is left for the Ruff parser.
            // `comptime` is a module-level concept; bindings and function
            // defs inside functions or classes cannot be evaluated at
            // build time. Only record top-level (indent_len == 0)
            // comptime declarations.
            let mut stripped_line: Option<String> = None;
            if indent_len == 0 && rest.starts_with("comptime ") {
                let payload = &rest["comptime ".len()..];

                // Function declaration: `comptime def NAME(...):` — the
                // function becomes callable from comptime expression
                // evaluation. The Python `def` body is left intact for
                // the parser; the evaluator pulls the body straight from
                // the parsed AST when a comptime call dispatches to this
                // name.
                if let Some(after_def) = payload.strip_prefix("def ") {
                    let name = after_def.split('(').next().unwrap_or("").trim().to_owned();
                    if !name.is_empty() {
                        comptime_functions.push(name);
                        stripped.push(StrippedKeyword {
                            line_index,
                            keyword: TyphonKeyword::Comptime,
                        });
                        stripped_line = Some(format!("{}{}", indent, payload));
                    }
                } else {
                    // Skip past any inner let/mut to find the binding name.
                    let name_source = if let Some(s) = payload.strip_prefix("let ") {
                        s
                    } else if let Some(s) = payload.strip_prefix("mut ") {
                        s
                    } else {
                        payload
                    };
                    let binding_name = name_source
                        .split([':', '='])
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_owned();

                    if !binding_name.is_empty() {
                        comptime_bindings.push(ComptimeBinding {
                            name: binding_name,
                            line_index,
                        });
                        stripped.push(StrippedKeyword {
                            line_index,
                            keyword: TyphonKeyword::Comptime,
                        });
                        stripped_line = Some(format!("{}{}", indent, payload));
                    }
                }
            }

            // ── `lazy [let|mut] name…` → strip only the `lazy` prefix; the
            // inner `let`/`mut` (if any) is left for the Ruff parser. This
            // entry is independent of `lazy import`, which is handled by
            // the parallel `lazy_imports` mechanism.
            if stripped_line.is_none() && rest.starts_with("lazy ") && rest.len() > 5 {
                let after_lazy = &rest["lazy ".len()..];
                // Only strip when followed by a `let`/`mut` binding so we
                // don't accidentally swallow `lazy import …` here (that's
                // handled above).
                if after_lazy.starts_with("let ") || after_lazy.starts_with("mut ") {
                    stripped.push(StrippedKeyword {
                        line_index,
                        keyword: TyphonKeyword::Lazy,
                    });
                    stripped_line = Some(format!("{}{}", indent, after_lazy));
                }
            }

            // `let` / `mut` line prefixes are recognised natively by the
            // Ruff parser as soft keywords, so the preprocessor no longer
            // strips them. The resolver reads `mutability` directly from
            // the AST.

            stripped_line.unwrap_or_else(|| line.to_owned())
        } else {
            line.to_owned()
        };

        let (rewritten, marks) = rewrite_optionals(&line_after_keyword, &mut in_string);
        for col in marks {
            optionals.push(StrippedOptional {
                line_index,
                python_col: col,
            });
        }
        python_source.push_str(&rewritten);
    }

    // Pub entries are appended LAST so postprocess restores them on top
    // of any other prefix that lives on the same line (e.g. `pub let X`
    // first becomes `let X`, then `pub let X`). The postprocess loop
    // sorts insertions descending by line_index with a stable sort, so
    // same-line entries execute in insertion order — last pushed runs
    // last → prefix appears at the front of the final restored line.
    for &line_idx in &pub_lines {
        stripped.push(StrippedKeyword {
            line_index: line_idx,
            keyword: TyphonKeyword::Pub,
        });
    }
    PreprocessResult {
        python_source,
        stripped,
        optionals,
        comptime_bindings,
        comptime_functions,
        lazy_imports,
        unsafe_lines,
        raw_class_lines,
        frozen_class_lines,
        plain_class_lines,
        pub_names,
        pub_star_lines,
    }
}

/// Pre-pass that walks every line of `source`, strips a leading
/// `pub ` modifier on module-level declarations, and records the
/// declared name. Returns the rewritten source with `pub ` removed
/// from each affected line; `pub_names` is appended with the names in
/// source order, and `pub_lines` gets the 0-based line index for each.
///
/// Recognised forms (zero indent only):
///   pub def NAME(...)            pub class NAME...            pub class! NAME...
///   pub plain class NAME...      pub model NAME:              pub interface NAME:
///   pub let NAME[: T] = EXPR     pub mut NAME[: T] = EXPR
///   pub newtype NAME = BASE      pub type NAME = ...
///   pub async def NAME(...)
///
/// Carry-forward over a `@decorator` line (so the next decl picks up
/// the `pub` marker) is **not** supported in v1 — the pre-pass is
/// stateless, one line at a time. Users wanting to decorate a `pub`
/// function should write the decorator on the line above and place
/// `pub` directly on the `def` itself:
///
/// ```text
/// @cached
/// pub def fetch(...) -> ...:
/// ```
///
/// `pub` inside a function body (`indent_len > 0`), inside a string,
/// or on an unrecognised form is left untouched so the Python parser
/// surfaces the syntax error the user wrote.
fn strip_pub_prefixes(
    source: &str,
    pub_names: &mut Vec<String>,
    pub_lines: &mut Vec<usize>,
    pub_star_lines: &mut Vec<usize>,
) -> String {
    let mut out = String::with_capacity(source.len());
    let mut in_string: Option<StringMode> = None;
    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        if in_string.is_some() {
            // Track string state without touching content.
            let _ = rewrite_optionals(line, &mut in_string);
            out.push_str(line);
            continue;
        }
        let indent_len = line
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(line.len());
        if indent_len != 0 {
            let _ = rewrite_optionals(line, &mut in_string);
            out.push_str(line);
            continue;
        }
        let rest = &line[indent_len..];
        // R3 frontier: `pub *` (with optional comment / trailing
        // whitespace) is a wildcard re-export marker. Strip the line
        // entirely so the Python parser doesn't see it, and record the
        // line index so the build orchestrator can:
        //   - In `__init__.ty`: synthesise `from .sibling import …`
        //     for each direct-sibling module's `pub` names.
        //   - In any other file: emit `tyc::pub_star_outside_init`.
        if is_pub_star_line(rest) {
            pub_star_lines.push(line_index);
            // Replace with a blank line so line numbers stay aligned
            // and the rest of the pipeline ignores it.
            if line.ends_with('\n') {
                out.push('\n');
            }
            continue;
        }
        if let Some(after_pub) = rest.strip_prefix("pub ") {
            if let Some(name) = pub_decl_name(after_pub) {
                pub_names.push(name);
                pub_lines.push(line_index);
                out.push_str(after_pub);
                // Track string state for the rewritten line.
                let _ = rewrite_optionals(after_pub, &mut in_string);
                continue;
            }
        }
        let _ = rewrite_optionals(line, &mut in_string);
        out.push_str(line);
    }
    out
}

/// Recognise a `pub *` wildcard-re-export marker line. Accepts:
///   `pub *`, `pub *  ` (trailing whitespace), `pub *  # comment`
/// Rejects any non-comment text after the `*`.
fn is_pub_star_line(rest: &str) -> bool {
    let Some(after) = rest.strip_prefix("pub *") else {
        return false;
    };
    let trimmed = after.trim_start_matches([' ', '\t']);
    let body = trimmed.trim_end_matches(['\n', '\r']);
    body.is_empty() || body.starts_with('#')
}

/// Given a line that originally followed `pub ` (so `def foo(...)`,
/// `class Foo:`, `let X = ...`, etc.), return the declared name. Used
/// by [`strip_pub_prefixes`] to populate the `pub_names` registry.
/// Exposed for the build orchestrator's `pub *` aggregation pre-pass,
/// which needs to extract sibling-module pub names without paying for
/// a full preprocess.
pub fn pub_decl_name(body: &str) -> Option<String> {
    let body = body.trim_end_matches(['\n', '\r']);
    // Skip a leading `async ` for `pub async def f(...)`.
    let body = body.strip_prefix("async ").unwrap_or(body);
    let body = body.trim_start();
    // Multi-word keywords first.
    if let Some(rest) = body.strip_prefix("plain class ") {
        return ident_prefix(rest);
    }
    if let Some(rest) = body.strip_prefix("class! ") {
        return ident_prefix(rest);
    }
    if let Some(rest) = body.strip_prefix("freeze let ") {
        return ident_prefix(rest);
    }
    // Single-word keywords.
    let single_keyword_forms = [
        "def ",
        "class ",
        "model ",
        "interface ",
        "newtype ",
        "type ",
        "let ",
        "mut ",
    ];
    for kw in &single_keyword_forms {
        if let Some(rest) = body.strip_prefix(kw) {
            return ident_prefix(rest);
        }
    }
    None
}

/// Extract the leading Python identifier from `s`, ignoring whatever
/// follows (parameter list, base classes, type annotation, `=` RHS).
fn ident_prefix(s: &str) -> Option<String> {
    let s = s.trim_start();
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let name = &s[..end];
    if is_python_ident(name) {
        Some(name.to_owned())
    } else {
        None
    }
}

/// Reverse [`wrap_freeze_let`]: strip the `__typhon_freeze__(...)`
/// wrapper from the RHS so `tyc fmt` restores the original
/// `freeze let X = EXPR` shape. Returns the line content with the
/// wrapper removed (the `freeze ` prefix is prepended by the
/// caller); `None` if the wrapper isn't present.
///
/// Handles a trailing `# comment` symmetrically with [`wrap_freeze_let`]
/// so a round-trip on `freeze let X = [1, 2]  # note` is exact.
fn unwrap_freeze_let(content: &str) -> Option<String> {
    let (code, comment) = match content.find('#') {
        Some(i) => (&content[..i], &content[i..]),
        None => (content, ""),
    };
    let eq = code.find('=')?;
    let lhs = code[..eq].trim_end();
    let rhs = code[eq + 1..].trim();
    let inner = rhs.strip_prefix("__typhon_freeze__(")?;
    let inner = inner.strip_suffix(')')?;
    let suffix = if comment.is_empty() {
        String::new()
    } else {
        format!("  {}", comment.trim_end())
    };
    Some(format!("{} = {}{}", lhs, inner, suffix))
}

/// Wrap the RHS of a `freeze let` binding in `__typhon_freeze__(...)`.
/// `tail` is the part of the line after `freeze let ` — typically
/// `NAME = EXPR` or `NAME: T = EXPR`. The function locates the
/// top-level `=` and inserts the wrapper around what follows.
/// Returns `None` if no `=` is found (the parser will then surface
/// the user's syntax error verbatim).
///
/// A trailing `# comment` is split off before wrapping and
/// reattached after the closing `)` so
/// `freeze let X = [1, 2]  # tags` still lowers to valid Python
/// (`let X = __typhon_freeze__([1, 2])  # tags`) rather than
/// burying the comment inside the call expression. Mirrors the
/// simple split-on-first-`#` convention used by
/// [`parse_newtype_decl`] and [`parse_lazy_import`].
#[cfg_attr(not(test), allow(dead_code))]
fn wrap_freeze_let(tail: &str) -> Option<String> {
    let wrap = wrap_freeze_let_with_depth(tail)?;
    // The single-line form was: `lhs = __typhon_freeze__(rhs)<tail>` —
    // append the closing `)` here so existing callers keep working.
    // Refuse the multi-line case: callers of the legacy single-line
    // helper expect a balanced expression.
    if wrap.residual != 0 {
        return None;
    }
    Some(format!("{}){}", wrap.head, wrap.tail))
}

/// Result of [`wrap_freeze_let_with_depth`].
struct FreezeLetWrap {
    /// The rewrite up to (but not including) the closing `)` of the
    /// `__typhon_freeze__(` call — `"X = __typhon_freeze__(rhs"`.
    head: String,
    /// Anything that must follow the closing `)` on the original line —
    /// typically a trailing comment `"  # note"`. Empty when there was
    /// no comment, and guaranteed empty in the multi-line case (the
    /// open-line cannot carry a comment because Python would treat the
    /// continuation as part of it).
    tail: String,
    /// Bracket depth remaining at end-of-line. Zero for the single-line
    /// case; positive when the RHS opens a multi-line container literal.
    residual: i32,
}

/// Rewrite the tail of `freeze let X = ...` to
/// `X = __typhon_freeze__(...` (note the *unclosed* call). Returns the
/// rewritten text and the *residual bracket depth* of the RHS on this
/// physical line:
///
/// * residual == 0 → the entire RHS fits on one line and the caller
///   should close the wrap by appending `)`.
/// * residual >  0 → the RHS opens a multi-line literal; the caller
///   must track depth across subsequent lines and append `)` when the
///   depth returns to zero.
///
/// The unclosed shape lets the multi-line `freeze let` case work
/// without joining lines (which would shift line numbers and break the
/// source map). A line-tail comment is preserved on single-line wraps;
/// on multi-line wraps we cannot meaningfully keep it on the open line
/// (it would be interpreted as a comment by Python and consume the rest
/// of the source line), so we forbid that combination.
fn wrap_freeze_let_with_depth(tail: &str) -> Option<FreezeLetWrap> {
    // We need the FIRST `=` that isn't inside square brackets or parens
    // — `Dict[str, int]` and similar must not confuse us. Track depth
    // and detect comments only outside of brackets.
    let bytes = tail.as_bytes();
    let mut depth: i32 = 0;
    let mut eq_idx: Option<usize> = None;
    let mut comment_idx: Option<usize> = None;
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'#' if depth == 0 && eq_idx.is_some() => {
                comment_idx = Some(i);
                break;
            }
            b'\'' | b'"' => in_str = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 && eq_idx.is_none() => {
                let prev = if i == 0 { 0u8 } else { bytes[i - 1] };
                let next = bytes.get(i + 1).copied().unwrap_or(0);
                if next == b'='
                    || matches!(
                        prev,
                        b'=' | b'>'
                            | b'<'
                            | b'!'
                            | b':'
                            | b'+'
                            | b'-'
                            | b'*'
                            | b'/'
                            | b'%'
                            | b'&'
                            | b'|'
                            | b'^'
                            | b'@'
                    )
                {
                    i += 1;
                    continue;
                }
                eq_idx = Some(i);
            }
            _ => {}
        }
        i += 1;
    }
    let eq = eq_idx?;
    let (code, comment) = match comment_idx {
        Some(c) => (&tail[..c], &tail[c..]),
        None => (tail, ""),
    };
    let lhs = code[..eq].trim_end();
    let rhs = code[eq + 1..].trim();
    if rhs.is_empty() {
        return None;
    }
    // Recompute the residual depth using only the RHS (the LHS is plain
    // names + type annotations and is balanced by construction).
    let residual = bracket_delta_simple(rhs);
    if residual < 0 {
        // RHS closed more brackets than it opened — malformed; let
        // downstream parsing surface the error verbatim.
        return None;
    }
    if residual > 0 && !comment.is_empty() {
        // Multi-line wrap can't carry a trailing comment on the open
        // line (the `#` would consume the wrap's continuation). Fall
        // through; let the user move the comment.
        return None;
    }
    let suffix = if comment.is_empty() {
        String::new()
    } else {
        format!("  {}", comment.trim_end())
    };
    Some(FreezeLetWrap {
        head: format!("{} = __typhon_freeze__({}", lhs, rhs),
        tail: suffix,
        residual,
    })
}

/// Net bracket delta of `s`, ignoring brackets inside string literals.
/// String literals use plain `'`/`"` quoting; backslash-escaped quotes
/// are honoured. Triple-quoted strings span multiple lines — those are
/// not handled here because the wider preprocess loop already tracks
/// `in_string` separately.
fn bracket_delta_simple(s: &str) -> i32 {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'#' => break,
            b'\'' | b'"' => in_str = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth
}

/// Net bracket delta of `line`, threading multi-line triple-string
/// state through `in_string`. Mirrors `bracket_delta_simple` but defers
/// to the wider preprocess loop's string tracker so brackets inside an
/// active triple-quoted string don't count.
fn bracket_delta_outside_strings(line: &str, in_string: &mut Option<StringMode>) -> i32 {
    let bytes = line.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        if in_string.is_some() {
            // Conservatively skip — we don't try to count brackets inside
            // a triple-quoted string. The wider loop handles entering /
            // leaving these strings via the existing `update_string_state`
            // path; here we just don't disturb depth counts.
            i += 1;
            continue;
        }
        let b = bytes[i];
        match b {
            b'#' => break,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth
}

/// Parse the tail of a `newtype` line: `Name = Base`.
///
/// Returns `(name, base)` on success, `None` if the syntax is malformed.
/// `Name` must be a valid Python identifier; `Base` is forwarded verbatim
/// as the inner type expression so generic forms (`list[int]`,
/// `dict[str, int]`, `Result[int, str]`) are supported.
fn parse_newtype_decl(tail: &str) -> Option<(String, String)> {
    let code = tail.split('#').next().unwrap_or("").trim();
    let eq = code.find('=')?;
    let name = code[..eq].trim().to_owned();
    let base = code[eq + 1..].trim().to_owned();
    if !is_python_ident(&name) || base.is_empty() {
        return None;
    }
    Some((name, base))
}

/// Reverse [`parse_newtype_decl`]'s emission: take the preprocessed
/// `Name = NewType("Name", Base)` form and restore `newtype Name = Base`.
/// Returns `None` if the line doesn't match the expected shape (in which
/// case the postprocessor leaves the line unchanged).
fn restore_newtype_decl(content: &str) -> Option<String> {
    let eq = content.find('=')?;
    let name = content[..eq].trim();
    let rhs = content[eq + 1..].trim();
    let rhs = rhs.strip_prefix("NewType(")?;
    let rhs = rhs.strip_suffix(')')?;
    let comma = rhs.find(',')?;
    let quoted = rhs[..comma].trim();
    let base = rhs[comma + 1..].trim();
    let qname = quoted.strip_prefix('"')?.strip_suffix('"')?;
    if qname != name || !is_python_ident(name) || base.is_empty() {
        return None;
    }
    Some(format!("newtype {} = {}", name, base))
}

/// Parse the tail of a `lazy import` line: `ALIAS = MODULE`.
///
/// Returns `(alias, module)` on success, `None` if the syntax is malformed.
fn parse_lazy_import(tail: &str) -> Option<(String, String)> {
    // Strip a trailing `# comment` so that `lazy import np = numpy  # noqa`
    // is handled correctly rather than failing the identifier check.
    let code = tail.split('#').next().unwrap_or("").trim();
    let eq = code.find('=')?;
    let alias = code[..eq].trim().to_owned();
    let module = code[eq + 1..].trim().to_owned();
    if !is_python_ident(&alias) || !is_dotted_python_ident(&module) {
        return None;
    }
    Some((alias, module))
}

/// True iff `s` is a valid Python identifier (starts with alpha or `_`,
/// followed by alphanumerics or `_`).
fn is_python_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// True iff `s` is a dotted Python module path (each component is a valid
/// Python identifier, e.g. `numpy` or `numpy.random`).
fn is_dotted_python_ident(s: &str) -> bool {
    !s.is_empty()
        && s.split('.')
            .all(|part| !part.is_empty() && is_python_ident(part))
}

/// Build the class-header portion of an `impl` line, converting
/// `ClassName:\n` into `__typhon_impl_ClassName(object):\n`.
///
/// `impl` blocks do not accept base-class lists; any `(...)` suffix on the
/// class name is stripped rather than forwarded, preventing a Python syntax
/// error from a doubly-parenthesised expression like
/// `class __typhon_impl_User(Base)(object):`.
///
/// Returns `None` when the line doesn't look like a class header (no `:`).
///
/// Handles two forms:
/// - `Name:` / `Name(Bases):` — plain `impl Name:`.
/// - `[T1, ...] Name[T1, ...]:` — generic `impl[T] Name[T]:`. The
///   leading bracket list is the impl's PEP 695 type parameters; it is
///   forwarded onto the pseudo class so the methods inside resolve
///   `T`/`U`. The trailing bracket list on the class name is dropped
///   from the pseudo class header (PEP 695 introduces type params on
///   the class header itself; we don't need to repeat them as bases).
fn make_impl_class_line(after_impl: &str) -> Option<String> {
    // Optionally consume a leading `[...]` type-param list. Track bracket
    // depth so commas inside nested brackets don't fool the scanner.
    let (impl_type_params, after_tps) = if after_impl.starts_with('[') {
        let mut depth = 0i32;
        let mut end = None;
        for (i, c) in after_impl.char_indices() {
            match c {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;
        (Some(&after_impl[..end]), after_impl[end..].trim_start())
    } else {
        (None, after_impl)
    };

    let mut depth = 0i32;
    let mut colon_pos = None;
    for (i, c) in after_tps.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ':' if depth == 0 => {
                colon_pos = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon_pos = colon_pos?;
    let raw = after_tps[..colon_pos].trim_end();
    // Strip any base-class list — `impl` blocks don't support inheritance.
    // Also strip the `[T, U]` trailing type-param application on the class
    // name; the pseudo class carries its own type params on the header.
    let name = if let Some(paren) = raw.find('(') {
        raw[..paren].trim_end()
    } else if let Some(bracket) = raw.find('[') {
        raw[..bracket].trim_end()
    } else {
        raw
    };
    let tail = &after_tps[colon_pos..]; // ":\n" or ":"
    let tp = impl_type_params.unwrap_or("");
    Some(format!("__typhon_impl_{}{}(object){}", name, tp, tail))
}

/// Expand a single-line `guard NAME = EXPR else: BODY` into a
/// None-check + early-return + immutable binding. The caller has
/// already verified the line starts with `guard ` and provides the
/// leading-whitespace `indent` so the rewrite preserves indentation.
///
/// Returns `None` when the line doesn't match the single-line guard
/// shape (multi-line `guard …\n    return` still hits a parse error;
/// that case is a separate, larger lowering not yet implemented).
fn expand_guard_one_liner(rest: &str, indent: &str, line_index: usize) -> Option<String> {
    let after_guard = rest.strip_prefix("guard ")?;
    // Body is everything after the `else:`. Walk the line tracking
    // bracket depth so `else:` inside a parenthesised expression doesn't
    // trip the matcher.
    let body_marker = " else:";
    let mut depth = 0i32;
    let mut idx = 0usize;
    let bytes = after_guard.as_bytes();
    let mut found = None;
    while idx < bytes.len() {
        match bytes[idx] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b' ' if depth == 0
                && idx + body_marker.len() <= bytes.len()
                && &bytes[idx..idx + body_marker.len()] == body_marker.as_bytes() =>
            {
                found = Some(idx);
                break;
            }
            _ => {}
        }
        idx += 1;
    }
    let split = found?;
    let head = &after_guard[..split]; // `NAME = EXPR`
    let tail = &after_guard[split + body_marker.len()..]; // ` BODY\n`

    // Split head on the first `=` outside of brackets.
    let mut depth = 0i32;
    let mut eq_pos = None;
    for (i, b) in head.bytes().enumerate() {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                // Reject `==` / `!=` / `<=` / `>=`.
                let prev = i.checked_sub(1).map(|j| head.as_bytes()[j]).unwrap_or(0);
                let next = head.as_bytes().get(i + 1).copied().unwrap_or(0);
                if matches!(prev, b'=' | b'!' | b'<' | b'>') || next == b'=' {
                    continue;
                }
                eq_pos = Some(i);
                break;
            }
            _ => {}
        }
    }
    let eq_pos = eq_pos?;
    let name = head[..eq_pos].trim();
    let expr = head[eq_pos + 1..].trim();
    if !is_simple_identifier(name) || expr.is_empty() {
        return None;
    }
    let body = tail.trim_end_matches(['\n', '\r']).trim();
    if body.is_empty() {
        return None;
    }
    // Stash EXPR in a unique-per-line temp so flow narrowing can fire
    // — the checker narrows Name expressions, not arbitrary call
    // results, so referencing the same `find_user(t)` twice in the
    // post-`if` `let` would yield `T?` again.
    let tmp = format!("__typhon_guard_{}", line_index);
    Some(format!(
        "{indent}let {tmp} = ({expr})\n{indent}if {tmp} is None: {body}\n{indent}let {name} = {tmp}\n"
    ))
}

/// Cheap identifier test for the `guard` name binding: ASCII letter or
/// underscore start, ASCII alphanumeric / underscore body.
fn is_simple_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// If `line` is a `def NAME(...) -> TYPE` declaration with no body
/// (no trailing `:` after the return type), return a rewritten copy
/// with `: ...` appended so the Python parser accepts it. Returns
/// `None` when the line doesn't match the bodiless-def shape.
///
/// This makes interface-body declarations like
/// `def draw() -> None` legal source, mirroring the docs and the
/// skill cheat-sheet's syntax for `interface` bodies.
fn append_ellipsis_to_bodiless_def(line: &str) -> Option<String> {
    // Preserve trailing newline / CRLF.
    let (body, terminator) = match line.rfind('\n') {
        Some(idx) => {
            let term_start = if idx > 0 && line.as_bytes()[idx - 1] == b'\r' {
                idx - 1
            } else {
                idx
            };
            (&line[..term_start], &line[term_start..])
        }
        None => (line, ""),
    };
    let indent_len = body.find(|c: char| !c.is_whitespace())?;
    let rest = &body[indent_len..];
    if !rest.starts_with("def ") {
        return None;
    }
    // Confirm balanced parens before checking for the bodiless tail. Track
    // bracket depth so `[T, U]`-style annotations in the return type don't
    // throw the scanner off.
    let mut paren_depth = 0i32;
    let mut bracket_depth = 0i32;
    let mut found_close_paren = false;
    for b in rest.as_bytes() {
        match b {
            b'(' => paren_depth += 1,
            b')' => {
                paren_depth -= 1;
                if paren_depth == 0 {
                    found_close_paren = true;
                }
            }
            b'[' => bracket_depth += 1,
            b']' => bracket_depth -= 1,
            _ => {}
        }
    }
    if !found_close_paren || paren_depth != 0 || bracket_depth != 0 {
        return None;
    }
    // Strip a trailing comment (if any) before checking the bodiless tail.
    let no_comment = strip_trailing_comment(rest);
    let trimmed = no_comment.trim_end();
    // Detect the function's header colon — anything at depth 0 *after*
    // the closing `)`. A `:` there means the function has a body (either
    // single-line `def f(): pass` or multi-line `def f():\n …`), so we
    // must NOT rewrite — the line is already valid Python.
    let mut depth = 0i32;
    let mut past_close = false;
    let bytes = trimmed.as_bytes();
    for &b in bytes {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 && b == b')' {
                    past_close = true;
                }
            }
            b':' if depth == 0 && past_close => return None,
            _ => {}
        }
    }
    // Require a `-> TYPE` return annotation so we don't accidentally
    // rewrite a syntactically invalid `def f()` (no return type, no
    // body) into something that masks a real user error.
    if !trimmed.contains("->") {
        return None;
    }
    Some(format!("{}: ...{}", body.trim_end(), terminator))
}

/// Strip the `frozen` modifier from a `class NAME frozen:` or
/// `class NAME frozen(BASES):` header so the Python parser accepts it.
///
/// Returns the rewritten line (including the trailing newline if one was
/// present), or `None` if the input does not match the expected shape.
/// The leading `class ` is preserved so callers can simply prepend the
/// captured indent.
fn strip_frozen_modifier(rest: &str) -> Option<String> {
    // Walk after `class ` to find the end of the class name (first non-id
    // character). Everything up through the name is the prefix we keep.
    let after_class = rest.strip_prefix("class ")?;
    let name_end = after_class
        .bytes()
        .position(|b| !(b.is_ascii_alphanumeric() || b == b'_'))?;
    if name_end == 0 {
        return None;
    }
    let name = &after_class[..name_end];
    let after_name = &after_class[name_end..];
    // The modifier must follow whitespace and be the bare token `frozen`,
    // terminated by `:` or `(` (with whatever whitespace sits between).
    let trimmed = after_name.trim_start();
    let leading_ws = after_name.len() - trimmed.len();
    if leading_ws == 0 {
        return None;
    }
    let rest_after_mod = trimmed.strip_prefix("frozen")?;
    // Reject `frozen` followed by an identifier character (e.g. `frozenset`)
    // so the modifier match is unambiguous.
    if let Some(next) = rest_after_mod.bytes().next() {
        if next.is_ascii_alphanumeric() || next == b'_' {
            return None;
        }
    }
    // Anything after the modifier (bases list, colon, trailing newline) is
    // preserved verbatim — the Python parser handles `(Base):` natively.
    let tail = rest_after_mod.trim_start();
    Some(format!("class {}{}", name, tail))
}

/// Same as [`make_impl_class_line`] but uses the `__typhon_builtin_ext_`
/// prefix so the analyse pass can find these classes, extract their
/// methods to free functions, and drive the call-site rewrite.  Kept
/// separate from the user-class lowering because the desugar paths
/// diverge: builtin extensions never merge into the original type.
fn make_builtin_extend_class_line(after_extend: &str) -> Option<String> {
    let mut depth = 0i32;
    let mut colon_pos = None;
    for (i, c) in after_extend.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ':' if depth == 0 => {
                colon_pos = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon_pos = colon_pos?;
    let raw = after_extend[..colon_pos].trim_end();
    let name = if let Some(paren) = raw.find('(') {
        raw[..paren].trim_end()
    } else {
        raw
    };
    let tail = &after_extend[colon_pos..];
    Some(format!("__typhon_builtin_ext_{}(object){}", name, tail))
}

/// Pull the target type identifier out of an `extend …:` header.
fn extract_extend_target(after_extend: &str) -> Option<&str> {
    let head = after_extend.split([':', '(']).next()?.trim();
    if head.is_empty() {
        None
    } else {
        Some(head)
    }
}

/// True when `target` names a Python built-in type that the extension-on-
/// builtins pass should handle (vs. delegating to the regular user-class
/// `impl`-style merge).  Kept consistent with
/// [`BUILTIN_TYPES_REJECTED_BY_EXTEND`] — that list was the previous
/// hard-error gate and now drives the extraction pass.
fn is_builtin_extend_target(target: &str) -> bool {
    BUILTIN_TYPES_REJECTED_BY_EXTEND.contains(&target)
}

/// Build the class-header portion of an `interface` line, converting
/// `Name:\n` into `Name(Protocol):\n`.
///
/// Interfaces accept an explicit base list `Name(Base):\n` and the existing
/// bases are preserved alongside `Protocol`.
fn make_interface_class_line(after_iface: &str) -> Option<String> {
    let mut depth = 0i32;
    let mut colon_pos = None;
    for (i, c) in after_iface.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ':' if depth == 0 => {
                colon_pos = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon_pos = colon_pos?;
    let head = after_iface[..colon_pos].trim_end();
    let tail = &after_iface[colon_pos..];
    let new_head = if let Some(stripped) = head.strip_suffix(')') {
        let trimmed = stripped.trim_end();
        if trimmed.ends_with('(') {
            format!("{trimmed}Protocol)")
        } else {
            format!("{trimmed}, Protocol)")
        }
    } else {
        format!("{}(Protocol)", head)
    };
    Some(format!("{}{}", new_head, tail))
}

/// Rewrite an `unsafe` header line into a `if True:` block with a marker
/// comment so the desugar pass can find it.
///
/// Accepts both `unsafe:` and `unsafe x:` (where `x` is a future expression
/// form — rejected for now). Returns `None` if the header is malformed and the
/// caller should leave the line for the Python parser to flag.
fn rewrite_unsafe_block_line(rest: &str) -> Option<String> {
    // Strip a trailing newline so we can re-attach exactly one.
    let raw = rest.trim_end_matches(['\n', '\r']);
    let terminator = &rest[raw.len()..];
    let body = raw.strip_prefix("unsafe")?.trim_start();
    if !body.ends_with(':') {
        return None;
    }
    // For v1 we only accept the bare `unsafe:` form.
    if body.trim_end_matches(':').trim() != "" {
        return None;
    }
    Some(format!("if True:  # __typhon_unsafe__{}", terminator))
}

/// Rewrite the body of `lazy import X = expr` into a Python assignment that
/// uses the typhon_runtime lazy-loader helper. Returns `None` if the import
/// body doesn't match the supported shape.
/// Build the class-header portion of a `model` line, converting
/// `ClassName:\n` (or `ClassName :\n`) into `ClassName(BaseModel):\n`.
///
/// Returns `None` when the line doesn't look like a class header (no `:`).
fn make_model_class_line(after_model: &str) -> Option<String> {
    // Find the `:` that terminates the class header at depth 0.
    let mut depth = 0i32;
    let mut colon_pos = None;
    for (i, c) in after_model.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ':' if depth == 0 => {
                colon_pos = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon_pos = colon_pos?;
    let name = after_model[..colon_pos].trim_end();
    let tail = &after_model[colon_pos..]; // ":\n" or ":"
                                          // If `name` already ends with `)` it has existing bases — merge BaseModel
                                          // into the list.  Otherwise wrap with `(BaseModel)`.
    let new_name = if let Some(stripped) = name.strip_suffix(')') {
        // If the class had an empty base list `()`, stripped ends with `(`
        // and we must not emit the separating `, `.
        let trimmed = stripped.trim_end();
        if trimmed.ends_with('(') {
            format!("{trimmed}BaseModel)")
        } else {
            format!("{trimmed}, BaseModel)")
        }
    } else {
        format!("{}(BaseModel)", name)
    };
    Some(format!("{}{}", new_name, tail))
}

/// Active string-literal mode while scanning. `Single` and `Double` cannot
/// span a newline (per Python's grammar) and are reset at end-of-line;
/// triple-quoted forms persist across lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringMode {
    Single,
    Double,
    TripleSingle,
    TripleDouble,
}

/// Replace every `?` outside string literals and comments with ` | None`.
///
/// Returns the rewritten line and a list of 0-based byte columns in the
/// rewritten string where each ` | None` insertion *starts* (i.e. the
/// character right after the type token). The `in_string` argument tracks
/// triple-quoted string state across lines.
///
/// Known limitation: `?` inside an f-string expression (e.g. `f"{x?}"`) is
/// treated as string content and not rewritten — supporting it requires a
/// nested expression scanner, which is deferred.
fn rewrite_optionals(line: &str, in_string: &mut Option<StringMode>) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(line.len());
    let mut marks = Vec::new();
    let mut last_char: Option<char> = None;
    let mut chars = line.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        // Comment outside any string — copy the remainder verbatim.
        if in_string.is_none() && c == '#' {
            out.push(c);
            for (_, c2) in chars.by_ref() {
                out.push(c2);
            }
            break;
        }

        // Inside a string literal — copy chars, look for the close.
        if let Some(mode) = *in_string {
            out.push(c);
            // Backslash escape in single/double-quoted strings (but not raw —
            // we don't try to distinguish, since copying the next char
            // through is harmless either way).
            if matches!(mode, StringMode::Single | StringMode::Double) && c == '\\' {
                if let Some((_, esc)) = chars.next() {
                    out.push(esc);
                    last_char = Some(esc);
                    continue;
                }
            }
            match mode {
                StringMode::Single if c == '\'' => *in_string = None,
                StringMode::Double if c == '"' => *in_string = None,
                StringMode::TripleSingle if c == '\'' && line[i..].starts_with("'''") => {
                    out.push(chars.next().unwrap().1);
                    out.push(chars.next().unwrap().1);
                    *in_string = None;
                }
                StringMode::TripleDouble if c == '"' && line[i..].starts_with("\"\"\"") => {
                    out.push(chars.next().unwrap().1);
                    out.push(chars.next().unwrap().1);
                    *in_string = None;
                }
                _ => {}
            }
            last_char = Some(c);
            continue;
        }

        // Outside any string — detect a string opening.
        if c == '"' || c == '\'' {
            let triple = (c == '"' && line[i..].starts_with("\"\"\""))
                || (c == '\'' && line[i..].starts_with("'''"));
            out.push(c);
            if triple {
                out.push(chars.next().unwrap().1);
                out.push(chars.next().unwrap().1);
                *in_string = Some(if c == '"' {
                    StringMode::TripleDouble
                } else {
                    StringMode::TripleSingle
                });
            } else {
                *in_string = Some(if c == '"' {
                    StringMode::Double
                } else {
                    StringMode::Single
                });
            }
            last_char = Some(c);
            continue;
        }

        if c == '?' {
            let is_type_tail = matches!(
                last_char,
                Some(ch) if ch.is_alphanumeric() || ch == '_' || ch == ']'
            );
            if is_type_tail {
                marks.push(out.len());
                out.push_str(" | None");
                last_char = Some('e'); // last char of " | None"
                continue;
            }
            // Pass through unchanged; parser will surface a syntax error.
            out.push('?');
            last_char = Some('?');
            continue;
        }

        out.push(c);
        last_char = Some(c);
    }

    // Single/double-quoted strings cannot span a newline.
    if matches!(
        *in_string,
        Some(StringMode::Single) | Some(StringMode::Double)
    ) {
        *in_string = None;
    }

    (out, marks)
}

/// Restore stripped keywords, `?` sugar, and `lazy import` declarations into
/// a normalised Python source string.
///
/// `normalised` is the Python source after whitespace normalisation.
pub fn postprocess(
    normalised: &str,
    stripped: &[StrippedKeyword],
    optionals: &[StrippedOptional],
) -> String {
    postprocess_full(normalised, stripped, optionals, &[])
}

/// Like [`postprocess`] but also restores `lazy import` lines.
pub fn postprocess_full(
    normalised: &str,
    stripped: &[StrippedKeyword],
    optionals: &[StrippedOptional],
    lazy_imports: &[LazyImport],
) -> String {
    if stripped.is_empty() && optionals.is_empty() && lazy_imports.is_empty() {
        return normalised.to_owned();
    }

    let mut lines: Vec<String> = normalised.lines().map(|l| l.to_owned()).collect();
    let has_trailing_newline = normalised.ends_with('\n');

    // Restore `?` first (right-to-left within each line), then val/var, so
    // column offsets stay valid.
    let mut per_line: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for opt in optionals {
        per_line
            .entry(opt.line_index)
            .or_default()
            .push(opt.python_col);
    }
    for (line_idx, mut cols) in per_line {
        if line_idx >= lines.len() {
            continue;
        }
        cols.sort_unstable_by(|a, b| b.cmp(a));
        let mut line = std::mem::take(&mut lines[line_idx]);
        for col in cols {
            // Defensive bounds checks — normalisation can move columns
            // around (it doesn't in Phase 0, but stay safe).
            const REWRITE: &str = " | None";
            if col <= line.len() && line[col..].starts_with(REWRITE) {
                line.replace_range(col..col + REWRITE.len(), "?");
            }
        }
        lines[line_idx] = line;
    }

    // Restore Typhon keywords, processing from the last line to the first so
    // that line indices stay valid.
    let mut insertions: Vec<(usize, TyphonKeyword)> = stripped
        .iter()
        .map(|sk| (sk.line_index, sk.keyword))
        .collect();
    // Sort descending by line_index; for identical line_index values preserve
    // original order (stable sort) so that `val` is restored before `comptime`
    // on the same line, allowing `comptime` to be prepended on top.
    insertions.sort_by(|a, b| b.0.cmp(&a.0));
    for (line_idx, kw) in insertions {
        if line_idx >= lines.len() {
            continue;
        }
        match kw {
            TyphonKeyword::Impl => {
                // Restore `class __typhon_impl_X(object):` → `impl X:`.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let content = &line[indent_len..];
                let restored = if let Some(tail) = content.strip_prefix("class __typhon_impl_") {
                    let tail = tail.replacen("(object)", "", 1);
                    format!("impl {}", tail)
                } else {
                    format!("impl {}", content)
                };
                lines[line_idx] = format!("{}{}", &line[..indent_len], restored);
            }
            TyphonKeyword::Model => {
                // Restore `class X(BaseModel):` → `model X:`.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let content = &line[indent_len..];
                let restored = if let Some(tail) = content.strip_prefix("class ") {
                    // Remove "(BaseModel)" inserted by preprocessing.
                    let tail = tail
                        .replacen("(BaseModel)", "", 1)
                        .replacen("(BaseModel, ", "(", 1);
                    format!("model {}", tail)
                } else {
                    format!("model {}", content)
                };
                lines[line_idx] = format!("{}{}", &line[..indent_len], restored);
            }
            TyphonKeyword::Let | TyphonKeyword::Mut | TyphonKeyword::Comptime => {
                // Prepend the keyword (and a space) before the non-whitespace content.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let new_line = format!(
                    "{}{} {}",
                    &line[..indent_len],
                    kw.as_str(),
                    &line[indent_len..]
                );
                lines[line_idx] = new_line;
            }
            TyphonKeyword::Extend => {
                // Restore `class __typhon_impl_X(object):` → `extend X:`.
                // (`extend` shares the impl stub prefix.)
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let content = &line[indent_len..];
                let restored = if let Some(tail) = content.strip_prefix("class __typhon_impl_") {
                    let tail = tail.replacen("(object)", "", 1);
                    format!("extend {}", tail)
                } else {
                    format!("extend {}", content)
                };
                lines[line_idx] = format!("{}{}", &line[..indent_len], restored);
            }
            TyphonKeyword::Interface => {
                // Restore `class Name(Protocol):` → `interface Name:`.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let content = &line[indent_len..];
                let restored = if let Some(tail) = content.strip_prefix("class ") {
                    let tail = tail
                        .replacen("(Protocol)", "", 1)
                        .replacen("(Protocol, ", "(", 1)
                        .replacen(", Protocol)", ")", 1);
                    format!("interface {}", tail)
                } else {
                    format!("interface {}", content)
                };
                lines[line_idx] = format!("{}{}", &line[..indent_len], restored);
            }
            TyphonKeyword::Unsafe => {
                // Restore `if True:  # __typhon_unsafe__` → `unsafe:`.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                // Match either with or without trailing whitespace before the comment.
                let content = &line[indent_len..];
                let restored = if content.starts_with("if True:") {
                    // Find the marker; replace the whole `if True: …` head with `unsafe:`.
                    "unsafe:".to_owned()
                } else {
                    format!("unsafe: {}", content)
                };
                lines[line_idx] = format!("{}{}", &line[..indent_len], restored);
            }
            // `lazy import` restoration is handled separately below via the
            // `lazy_imports` vector.  `lazy let` round-trips through the
            // `stripped` mechanism: the inner `val`/`var` is restored first,
            // and this arm prepends the `lazy ` prefix on top.
            TyphonKeyword::Lazy => {
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let new_line = format!("{}lazy {}", &line[..indent_len], &line[indent_len..]);
                lines[line_idx] = new_line;
            }
            TyphonKeyword::Gather | TyphonKeyword::Go => {
                // These constructs are expanded by separate passes
                // (`expand_gather_blocks`, `expand_go_calls`); they never
                // end up in the `stripped` keyword list.
            }
            TyphonKeyword::RawClass => {
                // Restore `class Foo(...):` → `class! Foo(...):` by
                // rewriting just the leading `class` keyword.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let content = &line[indent_len..];
                let restored = if let Some(tail) = content.strip_prefix("class ") {
                    format!("class! {}", tail)
                } else {
                    content.to_owned()
                };
                lines[line_idx] = format!("{}{}", &line[..indent_len], restored);
            }
            TyphonKeyword::PlainClass => {
                // Restore `class Foo(...):` → `plain class Foo(...):` by
                // prepending the stripped `plain ` modifier.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let content = &line[indent_len..];
                let restored = if content.starts_with("class ") {
                    format!("plain {}", content)
                } else {
                    content.to_owned()
                };
                lines[line_idx] = format!("{}{}", &line[..indent_len], restored);
            }
            TyphonKeyword::Newtype => {
                // Restore `Name = NewType("Name", Base)` → `newtype Name = Base`.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let content = &line[indent_len..];
                let restored = restore_newtype_decl(content).unwrap_or_else(|| content.to_owned());
                lines[line_idx] = format!("{}{}", &line[..indent_len], restored);
            }
            TyphonKeyword::Pub => {
                // Prepend `pub ` to whatever the line currently starts
                // with. `pub` lives at module level (zero-indent) by
                // construction, so the prefix slot is empty.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                lines[line_idx] = format!("{}pub {}", &line[..indent_len], &line[indent_len..]);
            }
            TyphonKeyword::Freeze => {
                // Restore `let NAME = __typhon_freeze__(EXPR)` to
                // `freeze let NAME = EXPR`. The `let` keyword (or its
                // annotation form) sits unchanged in the middle; we
                // strip the wrapper around the RHS and prepend `freeze `.
                let line = &lines[line_idx];
                let indent_len = line
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(line.len());
                let content = &line[indent_len..];
                let restored = unwrap_freeze_let(content).unwrap_or_else(|| content.to_owned());
                lines[line_idx] = format!("{}freeze {}", &line[..indent_len], restored);
            }
        }
    }

    // Restore `lazy import ALIAS = MODULE` lines.  The preprocessor emitted
    // `import MODULE as ALIAS` in their place; replace that with the original
    // Typhon syntax.
    for li in lazy_imports {
        if li.line_index >= lines.len() {
            continue;
        }
        lines[li.line_index] = format!("lazy import {} = {}", li.alias, li.module);
    }

    let mut result = lines.join("\n");
    if has_trailing_newline {
        result.push('\n');
    }
    result
}

// ── `lazy` usage validation ───────────────────────────────────────────────────

/// An error produced when `lazy` is used in an unsupported form.
#[derive(Debug, Clone)]
pub struct LazyUsageError {
    /// 0-based line index in the original Typhon source.
    pub line_index: usize,
    /// Byte offset where the violating `lazy` token starts.
    pub offset: usize,
    /// Human-readable error message.
    pub message: String,
}

/// Scan `source` for `lazy` constructs that Typhon rejects.
///
/// Currently flagged:
/// - `lazy from x import a, b` — `from` imports defeat deferral because the
///   names must be bound eagerly; reject in favour of `lazy import x = x` and
///   member access through the lazy module proxy.
pub fn validate_lazy_usage(source: &str) -> Vec<LazyUsageError> {
    let mut errors = Vec::new();
    let mut byte_offset: usize = 0;
    let mut in_string: Option<StringMode> = None;
    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        let raw = line.trim_end_matches(['\n', '\r']);
        let pre = in_string;
        let _ = scan_line_code_end(raw, &mut in_string);
        if pre.is_none() {
            let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
            let body = &raw[indent_len..];
            if let Some(rest) = body.strip_prefix("lazy ") {
                if rest.starts_with("from ") {
                    errors.push(LazyUsageError {
                        line_index,
                        offset: byte_offset + indent_len,
                        message: "`lazy from … import …` is not supported; \
                                  Typhon's lazy imports defer module loading, \
                                  which is incompatible with eagerly binding \
                                  specific names. Use `lazy import x = x` and \
                                  access members through the lazy proxy."
                            .to_owned(),
                    });
                }
            }
        }
        byte_offset += line.len();
    }
    errors
}

// ── `extend` usage validation ────────────────────────────────────────────────

/// An error produced when `extend` is used in an unsupported form.
#[derive(Debug, Clone)]
pub struct ExtendUsageError {
    /// 0-based line index in the original Typhon source.
    pub line_index: usize,
    /// Byte offset where the violating `extend` token starts.
    pub offset: usize,
    /// Human-readable error message.
    pub message: String,
}

/// Built-in Python type names that cannot be the target of `extend`.
///
/// Adding methods to built-in types in pure Python is not possible because
/// `str`, `int`, etc. are immutable from Python code.  Typhon v1 therefore
/// rejects `extend BUILTIN:` rather than silently lowering it to an unused
/// pseudo-class that would drop methods on the floor.  Phase 4 plans an
/// alternative form via a call-site rewriter — see the roadmap.
const BUILTIN_TYPES_REJECTED_BY_EXTEND: &[&str] = &[
    "bool",
    "bytearray",
    "bytes",
    "complex",
    "dict",
    "float",
    "frozenset",
    "int",
    "list",
    "object",
    "range",
    "set",
    "str",
    "tuple",
    "type",
];

/// Scan `source` for `extend NAME:` declarations whose target carries a
/// generic argument list (`extend list[int]:`, `extend dict[str, int]:`).
///
/// In Typhon ≥ 0.2, `extend BUILTIN:` itself is permitted — the
/// preprocess pass lowers each block to a sentinel
/// `class __typhon_builtin_ext_BUILTIN:` stub that an analyse pass
/// extracts to module-level free functions plus a call-site rewriter.
/// Parametric extends, however, would need per-element-type method
/// dispatch the call-site rewriter doesn't yet support, so the
/// preprocessor silently treats `extend list[int]:` as `impl list:` and
/// the user gets a confusing downstream `tyc::impl_unknown_class`. This
/// validator surfaces a dedicated message earlier in the pipeline so
/// the user knows to drop the type-parameter list — or, with the
/// generic-aware rewriter in place, that the feature isn't yet
/// supported (FINDINGS O23).
pub fn validate_extend_usage(source: &str) -> Vec<ExtendUsageError> {
    let mut out = Vec::new();
    let mut byte_offset: usize = 0;
    let mut in_string: Option<StringMode> = None;
    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        let raw = line.trim_end_matches(['\n', '\r']);
        let pre_string = in_string;
        let code_end = scan_line_code_end(raw, &mut in_string);
        if pre_string.is_some() {
            byte_offset += line.len();
            continue;
        }
        let code = &raw[..code_end];
        let trimmed_start = code.find(|c: char| !c.is_whitespace());
        if let Some(start) = trimmed_start {
            let trimmed = &code[start..];
            if let Some(after) = trimmed.strip_prefix("extend ") {
                // The first `[` before any `:` or `(` marks a generic
                // parameter list. We surface the diagnostic on the
                // `extend` token itself so the highlight matches what
                // the user wrote rather than the synthesised
                // pseudo-class name downstream passes would use.
                let mut depth = 0i32;
                let mut found_bracket = false;
                for c in after.chars() {
                    match c {
                        '[' if depth == 0 => {
                            found_bracket = true;
                            break;
                        }
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        ':' if depth == 0 => break,
                        _ => {}
                    }
                }
                if found_bracket {
                    out.push(ExtendUsageError {
                        line_index,
                        offset: byte_offset + start,
                        message: "`extend NAME[T, …]:` (parameterised target) is \
                             not yet supported. Drop the `[…]` to extend the \
                             unparameterised type (the methods see the \
                             concrete element type at the call site through \
                             the receiver's annotation), or wait for the \
                             per-element-type dispatch the rewriter is \
                             tracked to gain in a later release."
                            .to_owned(),
                    });
                }
            }
        }
        byte_offset += line.len();
    }
    out
}

// ── `?` operator validation ───────────────────────────────────────────────────

/// An error produced when the `?` operator is used in an invalid context.
#[derive(Debug, Clone)]
pub struct QuestionOpError {
    /// 0-based line index in the original Typhon source.
    pub line_index: usize,
    /// Byte offset of the `?` character in the original source.
    pub offset: usize,
    /// Human-readable error message.
    pub message: String,
}

/// Validate that every `?` error-propagation operator (`expr)?`) in `source`
/// appears inside a function whose return-type annotation is `Result[T, E]`.
///
/// Returns a list of errors for each `)?` found at module level or inside a
/// function whose return-type annotation is known and does not begin with the
/// `Result` identifier.
///
/// Multi-line signatures where `->` appears on the `) -> RetType:` closing
/// line (rather than the `def` line itself) are handled: the validator detects
/// the `) -> RetType:` pattern and records the return type.  Signatures where
/// `->` appears on neither the `def` line nor the closing `)` line are treated
/// as having an unknown return type and the `?` check is skipped for them to
/// avoid false positives.
///
/// Call this on the **original Typhon source** before [`expand_question_ops`].
pub fn validate_question_ops(source: &str) -> Vec<QuestionOpError> {
    let mut errors = Vec::new();
    // Stack of (def_indent_len, return_type_text):
    //   def_indent_len   — indentation column of the `def` keyword itself.
    //   return_type_text — Some(text) when `->` was found; None means unknown.
    let mut fn_stack: Vec<(usize, Option<String>)> = Vec::new();
    let mut in_string: Option<StringMode> = None;
    // Running byte offset of the start of the current line in `source`.
    let mut byte_offset: usize = 0;

    for (line_index, line) in source.split_inclusive('\n').enumerate() {
        let raw = line.trim_end_matches(['\n', '\r']);

        let pre_string = in_string;
        let code_end = scan_line_code_end(raw, &mut in_string);

        // Lines inside a triple-quoted string are pure string content — skip.
        if pre_string.is_some() {
            byte_offset += line.len();
            continue;
        }

        let code = raw[..code_end].trim_end();

        // Blank and comment-only lines don't affect scope tracking.
        if code.trim().is_empty() {
            byte_offset += line.len();
            continue;
        }

        let indent_len = code.find(|c: char| !c.is_whitespace()).unwrap_or(0);
        let trimmed = &code[indent_len..];

        // A line that begins with `)` is the continuation/close of a multi-line
        // expression — most commonly `) -> RetType:` closing a multi-line
        // parameter list.  Suppress scope-pop for these lines; if `->` is
        // present, update the most-recent function's return type.  Unlike the
        // previous approach, we do NOT skip the `?` detection below so that
        // `)?` on the same line as a closing paren is still validated.
        if trimmed.starts_with(')') {
            if trimmed.contains("->") {
                if let Some(entry) = fn_stack.last_mut() {
                    if entry.1.is_none() {
                        entry.1 = extract_return_type_text(code);
                    }
                }
            }
        } else {
            // Pop functions we've exited: a non-blank line at indent ≤ fn_indent
            // means we have left that function's body.
            while let Some(&(fn_indent, _)) = fn_stack.last() {
                if indent_len <= fn_indent {
                    fn_stack.pop();
                } else {
                    break;
                }
            }

            // Detect a function definition and push its return type onto the stack.
            //
            // `pub` is a visibility modifier that stacks with `def` / `async def`;
            // strip it so `pub def f() -> Result[..]: ... x?` is recognised as
            // being inside a function (R2-1). `trim_start` absorbs any extra
            // whitespace between the modifier and the keyword (`pub  def`)
            // so the detector doesn't depend on exact single-space spelling
            // (PR #129 gemini review).
            let after_pub = trimmed.strip_prefix("pub ").unwrap_or(trimmed).trim_start();
            if after_pub.starts_with("def ") || after_pub.starts_with("async def ") {
                let ret_type = extract_return_type_text(code);
                fn_stack.push((indent_len, ret_type));
            }
        }

        // Detect mid-expression `?` — `expand_inline_question_ops` now
        // lifts `Ok(step(x)?)` shapes into temps automatically, so the
        // syntactic carve-out is no longer needed. Validate the same
        // scope rules as the end-of-line case (`?` only valid inside a
        // function returning `Result[T, E]`). O17 / FINDINGS #66.
        for offset in find_mid_expression_questionmarks(code) {
            let q_offset = byte_offset + offset;
            // N2 (2026-05-22): `?` inside a comprehension body would lift
            // the call out to a temp *before* the comprehension's `for`
            // binding came into scope, which produced a misleading
            // "name not in scope" error against rewritten code the user
            // didn't write. Reject up front with a targeted message.
            if questionmark_is_in_comprehension(code, offset) {
                errors.push(QuestionOpError {
                    line_index,
                    offset: q_offset,
                    message: "`?` operator cannot appear inside a list / dict / set / \
                             generator comprehension; rewrite the comprehension as an explicit \
                             `for` loop that threads the `Err` short-circuit through"
                        .to_owned(),
                });
                continue;
            }
            match fn_stack.last() {
                None => {
                    errors.push(QuestionOpError {
                        line_index,
                        offset: q_offset,
                        message: "`?` operator used at module level; \
                                 it is only valid inside a function returning `Result[T, E]`"
                            .to_owned(),
                    });
                }
                Some((_, Some(ret))) if !is_result_type(ret) => {
                    errors.push(QuestionOpError {
                        line_index,
                        offset: q_offset,
                        message: format!(
                            "`?` operator used in a function returning `{ret}`; \
                             it is only valid in functions returning `Result[T, E]`"
                        ),
                    });
                }
                // Return type is Result-family (valid) or unknown (skip FP).
                Some(_) => {}
            }
        }
        // Detect `)?` — the `?` error-propagation operator.  The same pattern
        // `expand_question_ops` uses: last code char is `?`, char before is `)`.
        // This check runs for ALL lines, including `)…` continuation lines.
        if let Some(before_q) = code.strip_suffix('?') {
            if before_q.ends_with(')') {
                // Byte offset of the `?` in the original source.
                let q_offset = byte_offset + code.len() - 1;
                match fn_stack.last() {
                    None => {
                        errors.push(QuestionOpError {
                            line_index,
                            offset: q_offset,
                            message: "`?` operator used at module level; \
                                     it is only valid inside a function returning `Result[T, E]`"
                                .to_owned(),
                        });
                    }
                    Some((_, Some(ret))) if !is_result_type(ret) => {
                        errors.push(QuestionOpError {
                            line_index,
                            offset: q_offset,
                            message: format!(
                                "`?` operator used in a function returning `{ret}`; \
                                 it is only valid in functions returning `Result[T, E]`"
                            ),
                        });
                    }
                    // Return type is Result-family (valid) or unknown (skip FP).
                    Some(_) => {}
                }
            }
        }

        byte_offset += line.len();
    }

    errors
}

/// Find byte offsets of `)?` patterns that are *not* at the end of `code`.
/// These are mid-expression uses of the `?` propagation operator — the
/// current desugar pass only handles end-of-statement `?`, so anything
/// else produces a confusing parse error against the lowered Python.
/// FINDINGS #66.
fn find_mid_expression_questionmarks(code: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let bytes = code.as_bytes();
    let trimmed_end = code.trim_end_matches([' ', '\t']).len();
    // Skip the *last* `?` if it ends the code — that's the supported form.
    //
    // Also skip the `?` that immediately precedes a trailing `,` or `:` —
    // those are the `with`-chain terminators `expr?,` and `expr?:` which
    // `expand_with_chains` recognises later in the pipeline.  Without this
    // carve-out, the bindings on every line of a `with ... = ...?,
    // ... = ...?:` chain would be spuriously flagged as mid-expression `?`.
    let scan_end = if trimmed_end > 0 && bytes[trimmed_end - 1] == b'?' {
        trimmed_end - 1
    } else if trimmed_end >= 2
        && (bytes[trimmed_end - 1] == b',' || bytes[trimmed_end - 1] == b':')
        && bytes[trimmed_end - 2] == b'?'
    {
        trimmed_end - 2
    } else {
        trimmed_end
    };
    // Inline `with`-chains can carry multiple `expr?,` bindings and one
    // trailing `expr?:` on the same line. Those are recognised later by
    // `expand_with_chains` and must not be flagged here. Strip trailing
    // comments (string-aware) before the `?:` end-check so a chain with
    // `# comment` after it is still recognised.
    let is_with_chain_line = {
        let trimmed_start = code.trim_start();
        trimmed_start.starts_with("with ")
            && strip_trailing_comment(code)
                .trim_end_matches([' ', '\t'])
                .ends_with("?:")
    };
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < scan_end {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            in_str = Some(b);
            i += 1;
            continue;
        }
        if b == b'?' && i > 0 && bytes[i - 1] == b')' {
            // Inside a single-line `with`-chain, `)?,` and `)?:` are
            // binding terminators handled by `expand_with_chains`.
            if is_with_chain_line
                && i + 1 < bytes.len()
                && (bytes[i + 1] == b',' || bytes[i + 1] == b':')
            {
                i += 1;
                continue;
            }
            out.push(i);
        }
        i += 1;
    }
    out
}

/// Return `true` when the `?` at byte position `q_idx` in `code` sits
/// inside the *expression* portion of a comprehension — i.e. there is
/// an unbalanced opening bracket `[` / `(` / `{` to its left, and the
/// next `for` keyword at that bracket depth appears to its right
/// before the closing bracket.
///
/// Used by [`validate_question_ops`] to surface a targeted error for
/// `[parse(s)? for s in items]` (N2). The `?` lifter would otherwise
/// hoist the call out *above* the `for s in items` binding and produce
/// a misleading "name `s` not in scope" against text the user didn't
/// write. We could rewrite the comprehension into a manual loop, but
/// the resulting code lays out very differently from what the user
/// wrote, so rejecting up-front with a clear message is the cleaner
/// behaviour.
fn questionmark_is_in_comprehension(code: &str, q_idx: usize) -> bool {
    let bytes = code.as_bytes();
    if q_idx >= bytes.len() {
        return false;
    }
    // Track bracket depth from the start of the line up to the `?`.
    // We record the (depth, position) of the most recent unbalanced
    // opening bracket so we can know which group to scan for `for`.
    let mut stack: Vec<usize> = Vec::new();
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < q_idx {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'#' => return false, // `?` after a comment makes no sense, but be safe.
            b'\'' | b'"' => in_str = Some(b),
            b'(' | b'[' | b'{' => stack.push(i),
            b')' | b']' | b'}' => {
                stack.pop();
            }
            _ => {}
        }
        i += 1;
    }
    // No enclosing open bracket → can't be in a comprehension.
    let open_pos = match stack.last().copied() {
        Some(p) => p,
        None => return false,
    };
    let open_ch = bytes[open_pos];
    let close_ch = match open_ch {
        b'(' => b')',
        b'[' => b']',
        b'{' => b'}',
        _ => return false,
    };
    // Now scan forward from `?` looking for the matching close. While
    // we're at the *outermost* bracket level we entered, we look for
    // the `for ` token: a whole word `for` (with a space / paren on
    // either side). The first `for` we see at that depth flags the
    // enclosing bracket pair as a comprehension.
    let mut depth: i32 = 0;
    let mut in_str: Option<u8> = None;
    let mut j = q_idx + 1;
    while j < bytes.len() {
        let b = bytes[j];
        if let Some(q) = in_str {
            if b == b'\\' && j + 1 < bytes.len() {
                j += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            j += 1;
            continue;
        }
        match b {
            b'#' => break,
            b'\'' | b'"' => in_str = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    // We've left the enclosing bracket without seeing
                    // a `for` keyword — not a comprehension. The byte
                    // *should* match `close_ch` for a well-formed
                    // expression, but mismatched brackets would already
                    // have failed parsing upstream, so we just bail.
                    let _ = close_ch;
                    return false;
                }
                depth -= 1;
            }
            // `for` token at our outer depth (depth == 0 inside the
            // enclosing bracket means we're at the body level of the
            // bracket group, not inside a nested call).
            b'f' if depth == 0 && is_word_for(bytes, j) => return true,
            _ => {}
        }
        j += 1;
    }
    false
}

/// `true` when bytes `[i..i+3]` spells `for` *and* `for` is a whole
/// word (the byte before is non-identifier-continuation, the byte after
/// is non-identifier-continuation). Used by
/// [`questionmark_is_in_comprehension`] so we don't misfire on
/// substrings like `format(...)` or `before` inside a bracket group.
fn is_word_for(bytes: &[u8], i: usize) -> bool {
    if i + 3 > bytes.len() {
        return false;
    }
    if &bytes[i..i + 3] != b"for" {
        return false;
    }
    let prev_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
    let next_ok = i + 3 >= bytes.len() || !is_ident_byte(bytes[i + 3]);
    prev_ok && next_ok
}

fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric()
}

/// Return `true` when `ret` is the `Result` identifier (bare or subscripted).
///
/// Checks for a whole-word match so that `MyResult` / `NotAResult` are not
/// accepted.  The canonical Typhon form is `Result[T, E]`.
fn is_result_type(ret: &str) -> bool {
    let ret = ret.trim();
    if !ret.starts_with("Result") {
        return false;
    }
    // The character after "Result" (if any) must not be an identifier char.
    matches!(
        ret.as_bytes().get("Result".len()),
        None | Some(b'[') | Some(b' ') | Some(b'\t')
    )
}

/// Extract the return-type annotation text from a single `def` line.
///
/// Returns `Some(text)` when `->` appears outside string literals,
/// parentheses, and brackets on this line and the header-closing `:` is also
/// on the same line; `None` otherwise (multi-line signatures, missing
/// annotation, etc.).
///
/// String literal tracking prevents `->` or `:` inside a default-value string
/// (e.g. `def f(x: str = "->") -> Result:`) from confusing the parser.
fn extract_return_type_text(def_line: &str) -> Option<String> {
    let bytes = def_line.as_bytes();
    let mut depth = 0i32;
    let mut in_str: Option<u8> = None; // Some(b'"') or Some(b'\'')
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // Inside a single-/double-quoted string: look for the closing quote,
        // handling backslash escapes.  (Triple-quoted strings are unlikely in
        // a `def` parameter list and are not handled here.)
        if let Some(q) = in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => in_str = Some(b),
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = (depth - 1).max(0),
            b'-' if depth == 0 && i + 1 < bytes.len() && bytes[i + 1] == b'>' => {
                let after_arrow = def_line[i + 2..].trim_start();
                // Scan the return-type text for the colon that closes the
                // function header, respecting nested brackets and strings.
                let mut depth2 = 0i32;
                let mut in_str2: Option<u8> = None;
                let mut colon_pos = None;
                let abytes = after_arrow.as_bytes();
                let mut j = 0;
                while j < abytes.len() {
                    let c = abytes[j];
                    if let Some(q) = in_str2 {
                        if c == b'\\' {
                            j += 2;
                            continue;
                        }
                        if c == q {
                            in_str2 = None;
                        }
                        j += 1;
                        continue;
                    }
                    match c {
                        b'"' | b'\'' => in_str2 = Some(c),
                        b'(' | b'[' => depth2 += 1,
                        b')' | b']' => depth2 -= 1,
                        b':' if depth2 == 0 => {
                            colon_pos = Some(j);
                            break;
                        }
                        _ => {}
                    }
                    j += 1;
                }
                return colon_pos.map(|pos| after_arrow[..pos].trim().to_owned());
            }
            _ => {}
        }
        i += 1;
    }
    None
}

// ── `?` operator expansion ────────────────────────────────────────────────────

/// Expand `lazy import ALIAS = MODULE` declarations into thread-safe,
/// on-first-access loader code.
///
/// Each `lazy import` line at module level (indent = 0) is replaced by a
/// single call to the `typhon_runtime.lazy.lazy_import` helper, which
/// builds on the stdlib's `importlib.util.LazyLoader` to defer the real
/// import until the first attribute access:
///
/// ```text
/// # Input (Typhon)
/// lazy import np = numpy
///
/// # Output (valid Python)
/// from typhon_runtime.lazy import lazy_import as __typhon_lazy_import
/// np = __typhon_lazy_import("numpy")
/// ```
///
/// An earlier emission inlined a ~30-line bespoke proxy class per import
/// — `__TyphonLazy_<alias>_` with `__slots__`, double-checked-locking
/// `__getattr__`, custom `__dir__` and `__repr__`. That paid a heavy
/// per-import cost for behaviour the stdlib provides in five lines via
/// `LazyLoader`, and produced N copies of the same boilerplate when a
/// project lazily imported multiple modules. The runtime helper is
/// strictly better: the value returned is a real `types.ModuleType`
/// (so `isinstance(np, ModuleType)` is True), submodule loading works
/// out of the box, and there is one helper, not N.
///
/// `lazy from x import a, b` is not supported and is left unchanged so that
/// the Python parser produces a diagnostic at the offending line.
///
/// This function is called **before** [`preprocess`] in the build pipeline.
/// It is deliberately *not* called by `tyc fmt` or the check pipeline (which
/// use [`preprocess`]'s simpler `import MODULE as ALIAS` conversion instead).
/// Like [`expand_lazy_imports`] but only rewrites `lazy let` bindings; `lazy
/// import` lines are left untouched so that downstream passes (notably
/// [`preprocess`]) can still recognise them and populate the lazy-import
/// metadata used by the unused-import diagnostic.
pub fn expand_lazy_lets(source: &str) -> String {
    expand_lazy_imports_with(source, false)
}

/// Rewrite typed tuple-unpacking `let` declarations into a temp +
/// per-element sequence. N4 (2026-05-22).
///
/// Input shape: `let (NAME: TYPE, NAME: TYPE[, …]) = EXPR` — the
/// parenthesised LHS must contain at least one `:`-annotated capture
/// (otherwise the existing tuple-unpacking path handles it).
///
/// Output:
///
/// ```text
///     let __typhon_unpack_N__ = EXPR
///     let A: TA = __typhon_unpack_N__[0]
///     let B: TB = __typhon_unpack_N__[1]
/// ```
///
/// The pass is line-based and idempotent: it scans only lines whose
/// `code` portion (string- and comment-aware) starts with `let (`
/// and contains the `: TYPE` pattern. Other forms (untyped tuple let,
/// `let NAME = …`, regular Python statements) are emitted verbatim.
///
/// One untyped capture mixed with typed ones is allowed —
/// `let (a: int, b) = pair()` emits `let b = __typhon_unpack_N__[1]`
/// with the inferred type.
///
/// Multi-line RHS expressions (the literal spans multiple physical
/// lines) are *not* supported in v1 — Typhon source convention is to
/// keep the unpacking call on one line. The pass leaves multi-line
/// shapes untouched so the parser surfaces a clean error.
pub fn expand_typed_let_unpack(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut counter: usize = 0;
    let mut in_string: Option<StringMode> = None;

    for line in source.split_inclusive('\n') {
        let pre_string = in_string;
        let raw = line.trim_end_matches(['\n', '\r']);
        let _code_end = scan_line_code_end(raw, &mut in_string);

        if pre_string.is_some() {
            out.push_str(line);
            continue;
        }

        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        let indent = &raw[..indent_len];
        let body = &raw[indent_len..];

        let nl = if line.ends_with("\r\n") {
            "\r\n"
        } else if line.ends_with('\n') {
            "\n"
        } else {
            ""
        };

        match parse_typed_let_unpack(body) {
            Some(rewrite) => {
                let tmp = format!("__typhon_unpack_{}__", counter);
                counter += 1;
                out.push_str(indent);
                out.push_str("let ");
                out.push_str(&tmp);
                out.push_str(" = ");
                out.push_str(&rewrite.rhs);
                out.push_str(nl);
                for (i, capture) in rewrite.captures.iter().enumerate() {
                    out.push_str(indent);
                    out.push_str("let ");
                    out.push_str(&capture.name);
                    if let Some(ty) = &capture.annotation {
                        out.push_str(": ");
                        out.push_str(ty);
                    }
                    out.push_str(" = ");
                    out.push_str(&tmp);
                    out.push('[');
                    out.push_str(&i.to_string());
                    out.push(']');
                    out.push_str(nl);
                }
            }
            None => {
                out.push_str(line);
            }
        }
    }

    out
}

struct TypedLetUnpack {
    captures: Vec<TypedLetCapture>,
    rhs: String,
}

struct TypedLetCapture {
    name: String,
    annotation: Option<String>,
}

/// Recognise `let (a: int, b: str) = expr` on a single physical line.
///
/// Returns `None` for plain untyped destructuring (`let (a, b) = expr`)
/// so that pattern keeps flowing through `preprocess`'s existing
/// `let `-stripping path. We only intercept when at least one capture
/// has a `:` annotation — that's the case the existing path can't
/// handle.
fn parse_typed_let_unpack(body: &str) -> Option<TypedLetUnpack> {
    let after_let = body.strip_prefix("let ")?;
    let after_paren = after_let.strip_prefix('(')?;
    let bytes = after_paren.as_bytes();
    // Find the matching `)` at depth 0, ignoring brackets inside
    // annotations (`list[int]`, `tuple[int, str]`, …) and string
    // literals.
    let mut depth: i32 = 0;
    let mut close: Option<usize> = None;
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_str = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 && b == b')' {
                    close = Some(i);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
        i += 1;
    }
    let close = close?;
    let inside = &after_paren[..close];
    let after_close = after_paren[close + 1..].trim_start();
    let rhs = after_close.strip_prefix('=')?.trim();
    if rhs.is_empty() {
        return None;
    }
    // Split captures by top-level commas (commas inside `[...]` /
    // `(...)` stay grouped so `dict[str, int]` survives).
    let raw_captures = split_top_level_commas_lite(inside);
    if raw_captures.is_empty() {
        return None;
    }
    let mut captures: Vec<TypedLetCapture> = Vec::with_capacity(raw_captures.len());
    let mut saw_annotation = false;
    for cap in raw_captures {
        let cap = cap.trim();
        if cap.is_empty() {
            return None;
        }
        let (name, annotation) = if let Some(colon) = find_top_level_colon(cap) {
            saw_annotation = true;
            (cap[..colon].trim(), Some(cap[colon + 1..].trim()))
        } else {
            (cap, None)
        };
        if !is_python_ident(name) {
            return None;
        }
        captures.push(TypedLetCapture {
            name: name.to_owned(),
            annotation: annotation.map(|s| s.to_owned()),
        });
    }
    if !saw_annotation {
        // Pure untyped destructuring — let the existing path handle
        // it so we don't introduce gratuitous temps.
        return None;
    }
    Some(TypedLetUnpack {
        captures,
        rhs: rhs.to_owned(),
    })
}

/// Top-level comma split that ignores brackets and string literals.
/// A trimmed local copy so we don't have to wire up the larger
/// `split_top_level_commas` helper in the LSP / migrate crate.
fn split_top_level_commas_lite(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0usize;
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_str = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

/// Find the byte position of the first top-level `:` in `s`, ignoring
/// `:` characters inside bracket groups (`dict[str, int]`) and string
/// literals. Returns `None` if `s` has no top-level `:`.
fn find_top_level_colon(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_str = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b':' if depth == 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

pub fn expand_lazy_imports(source: &str) -> String {
    expand_lazy_imports_with(source, true)
}

fn expand_lazy_imports_with(source: &str, rewrite_imports: bool) -> String {
    let mut result = String::with_capacity(source.len() + 256);
    // Track triple-quoted string state so that a `lazy import` that appears
    // inside a docstring or multiline string is never mistakenly rewritten.
    let mut in_string: Option<StringMode> = None;
    // Stack of enclosing `class:` / `def:` block indents so we can decide
    // whether a `lazy let` is module-level (→ `lazy_let(lambda: …)`) or
    // class-body (→ `@cached_property`).  Each entry is `(indent_of_header,
    // is_class_body)`.
    let mut block_stack: Vec<(usize, bool)> = Vec::new();
    // Track whether we have already injected the runtime imports so we don't
    // emit duplicates.
    let mut needs_lazy_let_import = false;
    let mut needs_lazy_import_import = false;
    let mut needs_cached_property_import = false;
    let mut emitted_lines: Vec<String> = Vec::new();

    for line in source.split_inclusive('\n') {
        let pre_string = in_string;
        let raw = line.trim_end_matches(['\n', '\r']);
        let _code_end = scan_line_code_end(raw, &mut in_string);

        // Lines that begin inside a triple-quoted string are pure string
        // content — emit verbatim.
        if pre_string.is_some() {
            emitted_lines.push(line.to_owned());
            continue;
        }

        let trimmed = raw.trim_start();
        let indent_len = raw.len() - trimmed.len();

        // Pop block stack entries whose header indent is deeper than (or
        // equal to) the current code indent — unless the line is blank.
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            while let Some(&(top_indent, _)) = block_stack.last() {
                if indent_len <= top_indent {
                    block_stack.pop();
                } else {
                    break;
                }
            }
        }

        // Push a new entry when we enter a `class …:` or `def …:` block.
        // `impl …:` and `extend …:` are Typhon-only forms that lower to a
        // class body later in the pipeline; treat them as class-bodies here
        // so a `lazy let` inside is recognised as a class-body binding.
        // The body of the block sits at a deeper indent than `indent_len`,
        // so subsequent lines compare against `indent_len`.
        if let Some(rest) = trimmed.strip_prefix("class ") {
            if rest.contains(':') {
                block_stack.push((indent_len, true));
            }
        } else if let Some(rest) = trimmed.strip_prefix("impl ") {
            if rest.contains(':') {
                block_stack.push((indent_len, true));
            }
        } else if let Some(rest) = trimmed.strip_prefix("extend ") {
            if rest.contains(':') {
                block_stack.push((indent_len, true));
            }
        } else if let Some(rest) = trimmed.strip_prefix("async def ") {
            if rest.contains(':') {
                block_stack.push((indent_len, false));
            }
        } else if let Some(rest) = trimmed.strip_prefix("def ") {
            if rest.contains(':') {
                block_stack.push((indent_len, false));
            }
        }

        // ── `lazy import ALIAS = MODULE` ───────────────────────────────────
        if rewrite_imports && indent_len == 0 {
            if let Some(after) = trimmed.strip_prefix("lazy import ") {
                if let Some((alias, module)) = parse_lazy_import(after) {
                    let mut proxy = String::new();
                    emit_lazy_proxy(&mut proxy, &alias, &module);
                    needs_lazy_import_import = true;
                    emitted_lines.push(proxy);
                    continue;
                }
            }
        }

        // ── `lazy let NAME: T = expr` ──────────────────────────────────────
        if let Some(after) = trimmed.strip_prefix("lazy let ") {
            // Are we directly inside a class body?
            let inside_class = block_stack
                .last()
                .map(|&(header_indent, is_class)| is_class && indent_len > header_indent)
                .unwrap_or(false);
            let inside_function = block_stack
                .iter()
                .rev()
                .find(|&&(header_indent, _)| indent_len > header_indent)
                .map(|&(_, is_class)| !is_class)
                .unwrap_or(false);

            if let Some(binding) = parse_lazy_let_binding(after) {
                let indent = &raw[..indent_len];
                if inside_function && !inside_class {
                    // Function-local lazy let is not supported — leave the
                    // line so the parser flags it. A linter pass can later
                    // produce a nicer diagnostic.
                    emitted_lines.push(line.to_owned());
                    continue;
                }
                if inside_class {
                    // Lower to:
                    //   @cached_property
                    //   def NAME(self) -> T:
                    //       return expr
                    needs_cached_property_import = true;
                    let body = render_cached_property(indent, &binding);
                    emitted_lines.push(body);
                    continue;
                }
                if indent_len == 0 {
                    // Module-level: lower to
                    //   NAME: T = __typhon_lazy_let(lambda: expr)
                    needs_lazy_let_import = true;
                    let typed = if let Some(ty) = &binding.annotation {
                        format!("{}: {}", binding.name, ty)
                    } else {
                        binding.name.clone()
                    };
                    emitted_lines.push(format!(
                        "{}{} = __typhon_lazy_let(lambda: {})\n",
                        indent, typed, binding.expr
                    ));
                    continue;
                }
            }
        }

        emitted_lines.push(line.to_owned());
    }

    // Prepend any injected imports.  They go after the `from __future__`
    // imports, which by Python rules must remain at the top; for simplicity
    // we scan and insert after the last `from __future__ import …` line.
    let mut header = String::new();
    if needs_lazy_import_import {
        header.push_str("from typhon_runtime.lazy import lazy_import as __typhon_lazy_import\n");
    }
    if needs_lazy_let_import {
        header.push_str("from typhon_runtime.lazy import lazy_let as __typhon_lazy_let\n");
    }
    if needs_cached_property_import {
        header.push_str("from functools import cached_property as _typhon_cached_property\n");
    }

    if header.is_empty() {
        for line in &emitted_lines {
            result.push_str(line);
        }
        return result;
    }

    // Find the insertion point: after any leading `from __future__ import …`
    // statements (these must remain at the top of the file) and after a
    // module docstring if present. Inserting before the docstring would
    // demote it from `__doc__` to a dead string literal, silently
    // breaking `help(module)` and any tooling that reads `__doc__`.
    let mut insert_at = 0usize;
    let mut i = 0usize;
    while i < emitted_lines.len() {
        let line = &emitted_lines[i];
        let trimmed = line.trim_start();
        if trimmed.starts_with("from __future__ import")
            || trimmed.is_empty()
            || trimmed.starts_with('#')
        {
            i += 1;
            insert_at = i;
            continue;
        }
        // Module docstring detection: a triple-quoted string as the next
        // logical statement. May be single-line (`"""one-liner"""`) or
        // span multiple lines.
        if let Some(quote) = docstring_open_quote(trimmed) {
            // Check whether the opening triple quote also closes on the
            // same line (after the opener). Strip the leading triple
            // quote and look for a second occurrence.
            let rest = &trimmed[3..];
            if rest.contains(quote) {
                // Single-line docstring — consumed by this one line.
                i += 1;
                insert_at = i;
                break;
            }
            // Multi-line docstring — scan forward until we find the
            // closing triple quote.
            i += 1;
            while i < emitted_lines.len() {
                let body = &emitted_lines[i];
                if body.contains(quote) {
                    i += 1;
                    break;
                }
                i += 1;
            }
            insert_at = i;
            break;
        }
        break;
    }
    for line in &emitted_lines[..insert_at] {
        result.push_str(line);
    }
    result.push_str(&header);
    for line in &emitted_lines[insert_at..] {
        result.push_str(line);
    }

    result
}

/// Parsed `lazy let NAME [: T] = expr` body.
#[derive(Debug)]
struct LazyLetBinding {
    name: String,
    annotation: Option<String>,
    expr: String,
}

/// Parse the tail of a `lazy let ` line (everything after the keyword).  The
/// expected shapes are `NAME = expr` or `NAME: TYPE = expr`.
fn parse_lazy_let_binding(tail: &str) -> Option<LazyLetBinding> {
    let tail = tail.trim_end_matches(['\n', '\r']);
    // Strip a trailing comment so `lazy let x = 1  # note` works.
    let code = strip_trailing_comment(tail);
    let eq = find_top_level_assign_eq(&code)?;
    let head = code[..eq].trim();
    let expr = code[eq + 1..].trim().to_owned();
    if expr.is_empty() {
        return None;
    }
    let (name, annotation) = if let Some(colon) = head.find(':') {
        let name = head[..colon].trim().to_owned();
        let ann = head[colon + 1..].trim().to_owned();
        (name, Some(ann))
    } else {
        (head.to_owned(), None)
    };
    if !is_python_ident(&name) {
        return None;
    }
    Some(LazyLetBinding {
        name,
        annotation,
        expr,
    })
}

/// Find the `=` that opens the RHS of an assignment, skipping `=` inside
/// brackets or string literals.  Returns `None` for compound operators like
/// `==`, `!=`, `<=`, `>=`, `+=`, etc.
fn find_top_level_assign_eq(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string: Option<StringMode> = None;
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if let Some(mode) = in_string {
            match mode {
                StringMode::Single if c == '\'' => in_string = None,
                StringMode::Double if c == '"' => in_string = None,
                StringMode::Single | StringMode::Double if c == '\\' => {
                    i += 2;
                    continue;
                }
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => in_string = Some(StringMode::Single),
            '"' => in_string = Some(StringMode::Double),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0 => {
                // Skip compound operators: `==`, `<=`, `>=`, `!=`, `+=`, `-=`,
                // `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `>>=`, `<<=`, `:=`.
                let prev = if i == 0 { b' ' } else { bytes[i - 1] };
                let next = if i + 1 < bytes.len() {
                    bytes[i + 1]
                } else {
                    b' '
                };
                if next == b'=' {
                    i += 2;
                    continue;
                }
                if matches!(
                    prev,
                    b'=' | b'<'
                        | b'>'
                        | b'!'
                        | b'+'
                        | b'-'
                        | b'*'
                        | b'/'
                        | b'%'
                        | b'&'
                        | b'|'
                        | b'^'
                        | b':'
                ) {
                    i += 1;
                    continue;
                }
                return Some(i);
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Strip a trailing `# comment` from a single line, respecting string
/// literals.  Returns the comment-free prefix as an owned string.
fn strip_trailing_comment(line: &str) -> String {
    let mut in_string: Option<StringMode> = None;
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if let Some(mode) = in_string {
            match mode {
                StringMode::Single if c == '\'' => in_string = None,
                StringMode::Double if c == '"' => in_string = None,
                StringMode::Single | StringMode::Double if c == '\\' => {
                    i += 2;
                    continue;
                }
                _ => {}
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => in_string = Some(StringMode::Single),
            '"' => in_string = Some(StringMode::Double),
            '#' => return line[..i].to_owned(),
            _ => {}
        }
        i += 1;
    }
    line.to_owned()
}

/// Emit a `@cached_property`-decorated method for a class-body `lazy let`.
fn render_cached_property(indent: &str, binding: &LazyLetBinding) -> String {
    let ret = binding
        .annotation
        .as_deref()
        .map(|t| format!(" -> {}", t))
        .unwrap_or_default();
    format!(
        "{indent}@_typhon_cached_property\n{indent}def {name}(self){ret}:\n{indent}    return {expr}\n",
        indent = indent,
        name = binding.name,
        ret = ret,
        expr = binding.expr,
    )
}

/// If `line` starts with a Python triple-quote (`"""` or `'''`),
/// optionally preceded by an `r`/`b`/`u`/`rb`/`br` string prefix,
/// return the quote characters. Used by the header-insertion logic
/// to recognise a leading module docstring so injected imports don't
/// land above it (which would demote it from `__doc__` to a no-op
/// expression statement).
fn docstring_open_quote(line: &str) -> Option<&'static str> {
    // Strip an optional Python string prefix (one or two ASCII letters
    // from the b/r/u/f set). Module docstrings won't use `f`, but it
    // costs nothing to accept the wider set.
    let rest = line.trim_start_matches(['r', 'R', 'b', 'B', 'u', 'U']);
    if rest.starts_with("\"\"\"") {
        Some("\"\"\"")
    } else if rest.starts_with("'''") {
        Some("'''")
    } else {
        None
    }
}

/// Emit the single-line lowering for `lazy import ALIAS = MODULE`:
///
/// ```text
/// ALIAS = __typhon_lazy_import("MODULE")
/// ```
///
/// The `__typhon_lazy_import` symbol is brought into scope by an
/// injected `from typhon_runtime.lazy import lazy_import as
/// __typhon_lazy_import` at the top of the file (handled by the
/// caller, which sets `needs_lazy_import_import = true` when at least
/// one `lazy import` line is rewritten).
fn emit_lazy_proxy(out: &mut String, alias: &str, module: &str) {
    out.push_str(&format!("{alias} = __typhon_lazy_import(\"{module}\")\n"));
}

/// Expand the `?` error-propagation operator into equivalent Python guard code.
///
/// A line ending with `)?` in the code portion (before any comment) is treated
/// as the propagation operator. It is expanded in-place into the
/// three-statement guard form:
///
/// ```text
/// # Input (Typhon)
/// val x = f()?
///
/// # Output (valid Python, before further preprocessing)
/// __typhon_q_0__ = f()
/// if isinstance(__typhon_q_0__, Err):
///     return __typhon_q_0__
/// val x = __typhon_q_0__.value
/// ```
///
/// If there is no assignment target (bare `f()?`), the final assignment is
/// omitted and only the call + guard are emitted.
///
/// Lines inside triple-quoted strings and comment text are not expanded.
/// Trailing comments (`f()? # note`) are stripped before the check so the
/// comment does not interfere with operator detection.
///
/// This function is called **before** [`preprocess`] in the build and check
/// pipelines. It is deliberately *not* called by `tyc fmt` so that the
/// source formatter preserves the `?` syntax unchanged.
///
/// # Limitations
///
/// - Only handles `?` that directly follows `)`.  A `?` after `]` or an
///   identifier is treated as nullable-type sugar (`T?`) by the regular
///   preprocessor.
/// - Nested `?` operators on a single line are not supported.  Break them
///   across multiple `val` bindings instead.
pub fn expand_question_ops(source: &str) -> String {
    let mut result = String::with_capacity(source.len() + 64);
    let mut counter = 0usize;
    let mut in_string: Option<StringMode> = None;
    let mut needs_err_alias_import = false;

    for line in source.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\n', '\r']);

        // Record string state at the start of this line.
        let pre_string = in_string;

        // Scan the line to update string state and find the code end position
        // (where a comment begins, or line.len() if no comment).
        let code_end = scan_line_code_end(raw, &mut in_string);

        // Lines that start inside a triple-quoted string are pure string
        // content — emit verbatim.  (Single-line strings can't span lines.)
        if pre_string.is_some() {
            result.push_str(line);
            continue;
        }

        // The effective code (before any comment), trimmed of trailing whitespace.
        let content = raw[..code_end].trim_end();

        // A `?` operator must be the very last non-whitespace character in the
        // code portion.
        if !content.ends_with('?') || content.is_empty() {
            result.push_str(line);
            continue;
        }

        // Compute indentation to reproduce the correct nesting.
        let indent_len = content.find(|c: char| !c.is_whitespace()).unwrap_or(0);
        let indent = &content[..indent_len];

        // `expr_part` is the code content without the trailing `?`.
        let expr_part = &content[indent_len..content.len() - 1]; // e.g. "val x = f()"

        // Split into optional assignment LHS and the RHS expression.
        let (lhs, rhs) = match find_assignment_eq(expr_part) {
            Some(eq_pos) => {
                let l = expr_part[..eq_pos].trim();
                let r = expr_part[eq_pos + 1..].trim();
                (Some(l.to_owned()), r.to_owned())
            }
            None => (None, expr_part.to_owned()),
        };

        // The character before the `?` must be `)` for it to be unambiguously
        // the propagation operator (`f()?`). `T?` (nullable type sugar)
        // follows an alphanumeric / `_` / `]` char. Disambiguate the
        // alphanumeric / `]` case by RHS context:
        //
        // - `let x: int = a?`     — `a?` is in value-position (after `=`),
        //                           so the trailing `?` is propagation.
        // - `let x: int? = a`     — `int?` is in the annotation (before `=`),
        //                           so the trailing `?` would never reach here
        //                           (it's not at end of line in that shape).
        // - `let x: list[int]?`   — no `=`, ends with `]?`, treated as nullable.
        // - `type X = T?`         — `T?` is type-position despite the `=`;
        //                           the keyword introduces a type alias, not
        //                           a value binding. Same for `newtype X = T?`.
        // - `return f()?` / `Ok(f()?)` — handled by the inline-`?` pass
        //                           (expand_inline_question_ops) running
        //                           earlier in the pipeline; this pass only
        //                           processes the end-of-line statement form.
        //
        // Keep `T?` annotations as a no-op when there is no assignment, or
        // when the assignment is a type-alias declaration.
        let before_q = &content[..content.len() - 1];
        let last_ch = before_q.chars().last();
        let trailing_is_close_paren = matches!(last_ch, Some(')'));
        if !trailing_is_close_paren {
            // We're at `<expr>?` where <expr> ends with an identifier, `]`,
            // or `_`. Decide whether this is value-position propagation or
            // type-position nullable sugar.
            let first_word = |s: &str| s.split_whitespace().next().map(|w| w.to_owned());
            let is_value_position = match &lhs {
                // assignment: `a = X?` → X is value position, EXCEPT when the
                // LHS starts with `type` or `newtype` — those introduce a
                // type alias / nominal alias and the RHS is a type expression
                // where `?` is nullable-type sugar (`type Maybe = int?` ⇒
                // `type Maybe = int | None`).
                Some(l) => {
                    let first = first_word(l);
                    first.as_deref() != Some("type") && first.as_deref() != Some("newtype")
                }
                // No assignment → either a type annotation (`let x: T?`) on
                // its own line, or a bare statement form the inline-`?` pass
                // should have handled. Leave the line untouched.
                None => false,
            };
            if !is_value_position {
                // No assignment AND no value-position keyword → leave alone
                // (annotation form like `let x: list[int]?` or a bare type
                // alias position).
                result.push_str(line);
                continue;
            }
        }

        // Generate a unique temporary variable name.
        let tmp = format!("__typhon_q_{counter}__");
        counter += 1;

        let nl = if line.ends_with('\n') { "\n" } else { "" };

        // 1. Evaluate the expression into the temporary.
        result.push_str(indent);
        result.push_str(&tmp);
        result.push_str(" = ");
        result.push_str(&rhs);
        result.push('\n');

        // 2. Short-circuit on Err. Use the shadow-resistant
        // `__typhon_Err__` alias so a user-declared `type Err = …`
        // can't redirect the isinstance check away from
        // `typhon_runtime.Err`. FINDINGS #104.
        result.push_str(indent);
        result.push_str("if isinstance(");
        result.push_str(&tmp);
        result.push_str(", __typhon_Err__):\n");
        needs_err_alias_import = true;
        result.push_str(indent);
        result.push_str("    return ");
        result.push_str(&tmp);
        result.push('\n');

        // 3. Bind the Ok value if there is an assignment target.
        if let Some(l) = lhs {
            result.push_str(indent);
            result.push_str(&l);
            result.push_str(" = ");
            result.push_str(&tmp);
            result.push_str(".value");
            result.push_str(nl);
        }
    }

    if needs_err_alias_import {
        prepend_typhon_err_alias_import(result)
    } else {
        result
    }
}

/// Lift inline `?` propagation operators (`)?` appearing inside a larger
/// expression on the same line) into temp bindings that
/// [`expand_question_ops`] can then process as ordinary top-of-statement
/// `?` operators. Used to support the natural Rust-style form
/// `Ok(add(parse(s)?, parse(t)?))` — O17 / FINDINGS #66 / R3.13 / E9.
///
/// # Example
///
/// Input (Typhon):
/// ```text
///     return Ok(add(parse(s)?, parse(t)?))
/// ```
///
/// Output (valid Python after this pass; `expand_question_ops` is then a no-op):
/// ```text
///     __typhon_qi_0__ = parse(s)
///     if isinstance(__typhon_qi_0__, __typhon_Err__):
///         return __typhon_qi_0__
///     __typhon_qi_1__ = parse(t)
///     if isinstance(__typhon_qi_1__, __typhon_Err__):
///         return __typhon_qi_1__
///     return Ok(add(__typhon_qi_0__.value, __typhon_qi_1__.value))
/// ```
///
/// This pass runs **before** [`expand_question_ops`], which handles the
/// end-of-line case (`let x = f()?`). After this pass, every remaining
/// `?` on a line is either at the line-end position or is nullable-type
/// sugar (`T?` after an identifier or `]`), so the existing logic in
/// `expand_question_ops` handles it unchanged.
///
/// # Limitations
///
/// - Only handles callables whose receiver is a bare identifier, a
///   dotted path (`mod.f`, `obj.attr.f`), or a method chain ending in
///   `(...)` or `[...]` (`obj.f().g`, `obj[i].g`). Calls with a
///   parenthesised receiver like `(a + b)()?` are skipped.
/// - The `?` inside a string literal on the line is preserved verbatim.
pub fn expand_inline_question_ops(source: &str) -> String {
    let mut out = String::with_capacity(source.len() + 64);
    let mut counter = 0usize;
    let mut in_string: Option<StringMode> = None;
    let mut needs_err_alias_import = false;

    for line in source.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\n', '\r']);
        let pre_string = in_string;
        let code_end = scan_line_code_end(raw, &mut in_string);

        // Lines that begin inside a triple-quoted string have no
        // executable code on this row — emit verbatim.
        if pre_string.is_some() {
            out.push_str(line);
            continue;
        }

        let content = &raw[..code_end];
        let comment = &raw[code_end..];
        let nl = if line.ends_with('\n') { "\n" } else { "" };

        match rewrite_inline_question_ops_one_line(content, &mut counter) {
            Some((rewritten, lifted)) => {
                for l in lifted {
                    out.push_str(&l);
                    out.push('\n');
                }
                out.push_str(&rewritten);
                out.push_str(comment);
                out.push_str(nl);
                needs_err_alias_import = true;
            }
            None => {
                out.push_str(line);
            }
        }
    }

    if needs_err_alias_import {
        prepend_typhon_err_alias_import(out)
    } else {
        out
    }
}

/// Lift every inline `)?` on a single line. Returns `None` when the line
/// contains no inline propagation operator (so the caller can emit it
/// verbatim and avoid the overhead of building a new buffer). When `Some`,
/// the returned tuple is `(rewritten_line_without_newline, lifted_lines)`.
fn rewrite_inline_question_ops_one_line(
    content: &str,
    counter: &mut usize,
) -> Option<(String, Vec<String>)> {
    let indent_len = content
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(content.len());
    let indent = content[..indent_len].to_owned();

    let mut lifted: Vec<String> = Vec::new();
    let mut current = content.to_owned();

    while let Some(q_pos) = find_first_inline_propagation_q(&current) {
        let close_paren = q_pos - 1;
        let open = match find_matching_open_paren(&current, close_paren) {
            Some(o) => o,
            None => break,
        };
        let call_start = find_callable_start(&current, open);
        if call_start == open {
            // The `(` has no callable to its left — this is a
            // parenthesised expression like `(a + b)?`, not a call.
            // Skipping keeps the diagnostic crisp instead of emitting
            // a `(a + b).value` that runs but means the wrong thing.
            break;
        }
        let call_text = current[call_start..q_pos].to_owned();

        // Use a distinct prefix from the end-of-line pass's
        // `__typhon_q_N__` so the two rewrites can compose without
        // counter collisions when a single line carries both shapes
        // (e.g. `let x = Ok(f()?)?`).
        let tmp = format!("__typhon_qi_{}__", *counter);
        *counter += 1;

        lifted.push(format!("{indent}{tmp} = {call_text}"));
        lifted.push(format!("{indent}if isinstance({tmp}, __typhon_Err__):"));
        lifted.push(format!("{indent}    return {tmp}"));

        let mut new_content = String::with_capacity(current.len() + tmp.len());
        new_content.push_str(&current[..call_start]);
        new_content.push_str(&tmp);
        new_content.push_str(".value");
        new_content.push_str(&current[q_pos + 1..]);
        current = new_content;
    }

    if lifted.is_empty() {
        return None;
    }
    Some((current, lifted))
}

/// Find the position of the first `?` in `s` that is an inline
/// propagation operator (`)?` *not* at the end-of-code position).
/// Skips chars inside string literals. The end-of-code case is owned by
/// [`expand_question_ops`], so we deliberately skip it here.
fn find_first_inline_propagation_q(s: &str) -> Option<usize> {
    let trimmed_end = s.trim_end().len();
    let bytes = s.as_bytes();
    // Reuse the central in-string mask so triple-quoted strings (`"""…"""`,
    // `'''…'''`) inside the line don't accidentally toggle the in-string
    // state on each individual quote and unmask a `?` that lives inside
    // the literal. Gemini review on PR #96.
    let in_str_mask = compute_in_string_mask(s);
    for i in 0..bytes.len() {
        if in_str_mask[i] {
            continue;
        }
        if bytes[i] == b'?'
            && i > 0
            && bytes[i - 1] == b')'
            && !in_str_mask[i - 1]
            && i + 1 < trimmed_end
        {
            return Some(i);
        }
    }
    None
}

/// Walk backwards from `close_pos` (position of `)`) to find the matching
/// `(`. Skips parens inside string literals (scanned forwards once to
/// build a string-range mask).
fn find_matching_open_paren(s: &str, close_pos: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    // Mark every byte that's inside a string literal so the depth
    // counter doesn't count parens inside `"f(("`.
    let in_str_mask = compute_in_string_mask(s);
    let mut depth: i32 = 0;
    let mut i = close_pos;
    loop {
        if !in_str_mask[i] {
            match bytes[i] {
                b')' => depth += 1,
                b'(' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

/// Walk backwards from `(`'s position (exclusive) to find the start of
/// the callable expression. Accepts identifier characters, `.` (for
/// dotted paths), and balanced `(...)` / `[...]` segments (for method
/// chains like `obj.f().g(x)` or `obj[i].g(x)`). Returns the position
/// where the callable starts; if no callable precedes the `(`, returns
/// `open_pos` itself.
fn find_callable_start(s: &str, open_pos: usize) -> usize {
    let bytes = s.as_bytes();
    let in_str_mask = compute_in_string_mask(s);
    let mut i = open_pos;
    while i > 0 {
        let b = bytes[i - 1];
        if in_str_mask[i - 1] {
            break;
        }
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'.' {
            i -= 1;
            continue;
        }
        if b == b')' || b == b']' {
            // Walk back through a balanced bracket pair.
            let (open_byte, close_byte) = if b == b')' {
                (b'(', b')')
            } else {
                (b'[', b']')
            };
            let mut depth: i32 = 1;
            let mut j = i - 1;
            while j > 0 && depth > 0 {
                j -= 1;
                if in_str_mask[j] {
                    continue;
                }
                let c = bytes[j];
                if c == close_byte {
                    depth += 1;
                } else if c == open_byte {
                    depth -= 1;
                }
            }
            if depth != 0 {
                // Unmatched bracket to the left — abort the walk and
                // return the position immediately after the bracket so
                // the lift doesn't grab a half-balanced fragment.
                return i;
            }
            i = j;
            continue;
        }
        break;
    }
    i
}

/// Build a parallel byte-mask `mask[i] = true` iff `s.as_bytes()[i]` is
/// inside a `"..."` / `'...'` string literal. Triple-quoted strings on a
/// single line are treated as ordinary literals (they're rare on a
/// single line and the only safety property we need is "don't match
/// parens inside any string").
fn compute_in_string_mask(s: &str) -> Vec<bool> {
    let bytes = s.as_bytes();
    let mut mask = vec![false; bytes.len()];
    // `None` outside any string; `Some((quote, triple))` inside one. A
    // triple-quoted region (`"""` / `'''`) only ends on a matching
    // triple; bare quotes inside it are ordinary content. Single-line
    // strings continue to honour `\`-escapes; triple-quoted strings
    // do too (Python allows `\` continuation inside them).
    let mut in_str: Option<(u8, bool)> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some((q, triple)) = in_str {
            mask[i] = true;
            if b == b'\\' && i + 1 < bytes.len() {
                mask[i + 1] = true;
                i += 2;
                continue;
            }
            if b == q {
                if triple {
                    if i + 2 < bytes.len() && bytes[i + 1] == q && bytes[i + 2] == q {
                        mask[i + 1] = true;
                        mask[i + 2] = true;
                        in_str = None;
                        i += 3;
                        continue;
                    }
                } else {
                    in_str = None;
                }
            }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            // Detect `"""` / `'''` triple openers and consume all three
            // quotes at once so the in-string state for a triple region
            // isn't toggled off by the second quote of the opener.
            // Gemini review on PR #96.
            if i + 2 < bytes.len() && bytes[i + 1] == b && bytes[i + 2] == b {
                mask[i] = true;
                mask[i + 1] = true;
                mask[i + 2] = true;
                in_str = Some((b, true));
                i += 3;
                continue;
            }
            in_str = Some((b, false));
            mask[i] = true;
        }
        i += 1;
    }
    mask
}

/// Prepend `from typhon_runtime import Err as __typhon_Err__` to `source`
/// in a way that preserves Python's "future imports must be first" rule
/// and that doesn't demote a module docstring. Used by the `?` and
/// `with`-chain lowerings when they introduced a `__typhon_Err__`
/// reference. FINDINGS #104.
fn prepend_typhon_err_alias_import(body: String) -> String {
    const IMPORT_LINE: &str = "from typhon_runtime import Err as __typhon_Err__\n";

    let mut out = String::with_capacity(body.len() + IMPORT_LINE.len());
    let mut inserted = false;
    let mut idx = 0usize;
    let mut in_docstring: Option<&'static str> = None;
    while idx < body.len() {
        let line_end = body[idx..]
            .find('\n')
            .map(|n| idx + n + 1)
            .unwrap_or(body.len());
        let line = &body[idx..line_end];
        let trimmed = line.trim_start();

        let is_blank = trimmed.is_empty() || trimmed == "\n" || trimmed == "\r\n";
        let is_comment = trimmed.starts_with('#');
        let is_future = trimmed.starts_with("from __future__ import");
        let is_typhon_alias_import =
            trimmed.starts_with("from typhon_runtime") && trimmed.contains(" as __typhon_");

        if in_docstring.is_none() && (is_blank || is_comment || is_future || is_typhon_alias_import)
        {
            // If the import we're about to inject is already present in
            // the header, mark `inserted` so the trailing fallback
            // doesn't append a duplicate. This keeps the generated
            // Python tidy when both `expand_question_ops` and
            // `expand_with_chains` produce `__typhon_Err__` references
            // (each pipeline pass independently calls this helper).
            // `trimmed` is the line from `trim_start()`, which leaves the
            // trailing newline intact.  Strip both sides before comparing
            // against the import-line literal, otherwise a duplicate
            // injected by an earlier pass slips through and produces back-
            // to-back identical `from typhon_runtime import Err as ...`
            // lines in the emitted Python (caught by PR #120 review).
            if is_typhon_alias_import && trimmed.trim_end() == IMPORT_LINE.trim_end() {
                inserted = true;
            }
            out.push_str(line);
            idx = line_end;
            continue;
        }
        if in_docstring.is_none() {
            if let Some(q) = docstring_open_quote(trimmed) {
                let rest = &trimmed[3..];
                if rest.contains(q) {
                    out.push_str(line);
                    idx = line_end;
                    continue;
                }
                in_docstring = Some(q);
                out.push_str(line);
                idx = line_end;
                continue;
            }
        } else if let Some(q) = in_docstring {
            out.push_str(line);
            idx = line_end;
            if trimmed.contains(q) {
                in_docstring = None;
            }
            continue;
        }

        // Found the first real code line. Inject the import here
        // unless an identical line was already seen during the header
        // scan (in which case `inserted` is already `true`).
        if !inserted {
            out.push_str(IMPORT_LINE);
            inserted = true;
        }
        out.push_str(&body[idx..]);
        break;
    }

    if !inserted {
        out.push_str(IMPORT_LINE);
    }
    out
}

// ── multi-line `guard` expansion ───────────────────────────────────────────────

/// Expand the multi-line `guard NAME = EXPR else:` form into the same
/// shape the in-preprocess single-line handler produces. Runs as a
/// pre-pass so the rest of the pipeline (single-line guards included)
/// only sees one of two shapes: either the lowered form, or a still-
/// unhandled guard line that the single-line path then matches.
///
/// Source form:
///
/// ```text
///     guard w = weight else:
///         log("missing")
///         return 0
/// ```
///
/// Lowered to:
///
/// ```text
///     let __typhon_mguard_<N> = (weight)
///     if __typhon_mguard_<N> is None:
///         log("missing")
///         return 0
///     let w = __typhon_mguard_<N>
/// ```
///
/// The `mguard` prefix (rather than the single-line form's `guard`) is
/// deliberate: the single-line handler uses the source `line_index`
/// directly, while this multi-line pass uses a per-call counter. Using
/// distinct prefixes guarantees a multi-line and single-line guard on
/// the same line index can never share a temp name.
///
/// Behaviour:
/// - The body must be indented strictly deeper than the `guard` header.
///   The body ends at the first line whose code (skipping blank and
///   comment-only lines) indents at or above the header's indent.
/// - An empty body (no indented lines after `else:`) is left unrewritten
///   — the rest of the pipeline will produce a normal parse error.
/// - Lines inside triple-quoted strings are passed through untouched.
/// - This pass produces line-count drift: 1 header line lowers to 2
///   non-body lines plus a trailing `let NAME = …` line, matching the
///   single-line form's drift. Downstream span fidelity is the same
///   as for single-line guards.
pub fn expand_multiline_guards(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut in_string: Option<StringMode> = None;
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut i = 0;
    let mut counter: usize = 0;

    while i < lines.len() {
        let line = lines[i];
        let pre = in_string;
        let raw = line.trim_end_matches(['\n', '\r']);
        let _ = scan_line_code_end(raw, &mut in_string);

        if pre.is_some() {
            out.push_str(line);
            i += 1;
            continue;
        }

        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        let header_indent = &raw[..indent_len];
        let body = &raw[indent_len..];

        // Match `guard NAME = EXPR else:` with nothing after the colon
        // (single-line form is handled inside `preprocess`). The
        // header must end with `else:` and have no body suffix.
        let Some(after_guard) = body.strip_prefix("guard ") else {
            out.push_str(line);
            i += 1;
            continue;
        };
        let trimmed = after_guard.trim_end();
        let Some(head) = trimmed.strip_suffix("else:") else {
            // Either single-line form or unrecognised — leave it alone.
            out.push_str(line);
            i += 1;
            continue;
        };
        // Require a space before `else:` so we don't match `selse:` or
        // `relse:` inside an identifier.
        if !head.ends_with(' ') {
            out.push_str(line);
            i += 1;
            continue;
        }
        let assign_part = head.trim_end();
        // Split NAME = EXPR on the first `=` outside brackets.
        let Some((name, expr)) = split_guard_name_equals_expr(assign_part) else {
            out.push_str(line);
            i += 1;
            continue;
        };

        // Collect the indented body. End at the first non-blank,
        // non-comment-only line whose code indent is <= header indent.
        //
        // Three special cases bypass the indent-based termination:
        //   1. Lines whose start sits inside an unterminated triple-
        //      quoted string opened in an earlier body line — the
        //      content's leading-column-0 text isn't real indentation.
        //   2. Blank lines — the Python tokenizer skips them for
        //      indentation purposes, and a body author might place
        //      one before the first indented statement for clarity.
        //   3. Comment-only lines — same reasoning; a `# comment` at
        //      column 0 between two body statements doesn't dedent
        //      the surrounding block.
        // The body ends only when we hit a real code line at indent
        // <= header indent (outside a string).
        let mut body_lines: Vec<&str> = Vec::new();
        let mut body_state = in_string;
        let mut j = i + 1;
        while j < lines.len() {
            let candidate = lines[j];
            let raw_c = candidate.trim_end_matches(['\n', '\r']);
            // Case 1: inside a triple-quoted string opened in an
            // earlier body line. Push and keep scanning string state.
            if body_state.is_some() {
                let _ = scan_line_code_end(raw_c, &mut body_state);
                body_lines.push(candidate);
                j += 1;
                continue;
            }
            // Case 2/3: blank or comment-only line.
            let stripped = raw_c.trim_start();
            let is_blank_like = stripped.is_empty() || stripped.starts_with('#');
            if is_blank_like {
                body_lines.push(candidate);
                j += 1;
                continue;
            }
            let c_indent = raw_c
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(raw_c.len());
            if c_indent <= indent_len {
                break;
            }
            // Track string state so a triple-quoted string opened
            // inside the body is recognised on the next iteration.
            let _ = scan_line_code_end(raw_c, &mut body_state);
            body_lines.push(candidate);
            j += 1;
        }
        // Drop trailing blank/comment-only lines from the body: they
        // belong to the *surrounding* scope (the trailing `let NAME = …`
        // we're about to emit must come before them so the binding is
        // visible to subsequent statements). Without this, a guard
        // followed by a blank-line gap before unrelated code would
        // swallow the gap into the lowered `if` body and bury the
        // binding too deeply.
        while body_lines
            .last()
            .map(|l| {
                let raw = l.trim_end_matches(['\n', '\r']);
                let trimmed = raw.trim_start();
                trimmed.is_empty() || trimmed.starts_with('#')
            })
            .unwrap_or(false)
        {
            body_lines.pop();
            j -= 1;
        }
        // Body is "empty" if every collected line was blank/comment-only
        // (now all popped). Bail and let the parser surface its own
        // indent error rather than emitting an `if … is None:` with
        // nothing inside it.
        let has_code = body_lines.iter().any(|l| {
            let raw = l.trim_end_matches(['\n', '\r']);
            let trimmed = raw.trim_start();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        });
        if !has_code {
            out.push_str(line);
            i += 1;
            continue;
        }
        if body_lines.is_empty() {
            // Empty body — leave the source alone and let the parser
            // surface its own indent error.
            out.push_str(line);
            i += 1;
            continue;
        }

        // Emit the lowered form. Use a counter rather than the line
        // index so each guard in a multi-line file gets a unique temp
        // even when two headers share a column.
        let tmp = format!("__typhon_mguard_{}", counter);
        counter += 1;
        out.push_str(header_indent);
        out.push_str("let ");
        out.push_str(&tmp);
        out.push_str(" = (");
        out.push_str(expr);
        out.push_str(")\n");
        out.push_str(header_indent);
        out.push_str("if ");
        out.push_str(&tmp);
        out.push_str(" is None:\n");
        for b in &body_lines {
            out.push_str(b);
        }
        out.push_str(header_indent);
        out.push_str("let ");
        out.push_str(name);
        out.push_str(" = ");
        out.push_str(&tmp);
        out.push('\n');
        // Advance past the consumed lines + body.
        in_string = body_state;
        i = j;
    }

    out
}

/// Split `NAME = EXPR` on the first `=` outside brackets, returning
/// `(NAME, EXPR)` trimmed. Returns `None` for malformed input (missing
/// `=`, empty name, name that isn't a simple identifier, empty expr).
fn split_guard_name_equals_expr(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                // Reject `==` / `!=` / `<=` / `>=`.
                let prev = i.checked_sub(1).map(|j| bytes[j]).unwrap_or(0);
                let next = bytes.get(i + 1).copied().unwrap_or(0);
                if matches!(prev, b'=' | b'!' | b'<' | b'>') || next == b'=' {
                    continue;
                }
                let name = s[..i].trim();
                let expr = s[i + 1..].trim();
                if !is_simple_identifier(name) || expr.is_empty() {
                    return None;
                }
                return Some((name, expr));
            }
            _ => {}
        }
    }
    None
}

// ── `with`-chain expansion ────────────────────────────────────────────────────

/// Expand a `with`-chain into a flat sequence of guarded `Result` unwraps.
///
/// Source form:
///
/// ```text
/// with user   = db.find_user(id)?,
///      perms  = check_perms(user)?,
///      report = build_report(user, perms)?:
///     return Ok(report)
/// else err:
///     log.warn(err)
///     return Err(err)
/// ```
///
/// Each binding evaluates its RHS, returns the value when `Ok`, and runs the
/// `else` block (with `err` bound to the unwrapped error value) on the first
/// `Err`. When no `else` clause is provided the chain falls back to the
/// default propagation form — `return <tmp>` — matching the `?` operator.
///
/// This rewrite runs **before** [`expand_question_ops`] and [`expand_pipes`]
/// so that the rest of the pipeline sees only plain Python.
///
/// # Limitations
///
/// - The `with`-chain must sit at the top of its line and not be nested
///   inside another expression.
/// - Bindings continue on subsequent lines only; nesting another control
///   structure between bindings is not supported.
/// - The `else` body must be indented strictly deeper than the `with`
///   header. A bare `else:` (no binding name) defaults the error variable
///   name to `_err`.
pub fn expand_with_chains(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut counter: usize = 0;
    let mut in_string: Option<StringMode> = None;
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let pre_string = in_string;
        let raw = line.trim_end_matches(['\n', '\r']);
        let _code_end = scan_line_code_end(raw, &mut in_string);

        // Lines that start inside a triple-quoted string are pure content.
        if pre_string.is_some() {
            out.push_str(line);
            i += 1;
            continue;
        }

        // Look for a `with`-chain opener on this line.
        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        let chain_indent = &raw[..indent_len];
        let body = &raw[indent_len..];

        if let Some(rest) = body.strip_prefix("with ") {
            if let Some((inline_bindings, first_term)) = parse_inline_with_bindings(rest) {
                // Re-scan the binding line's string state so the continuation
                // scanner picks up unfinished triple-quoted strings correctly.
                let mut state_for_chain = pre_string;
                let _ = scan_line_code_end(raw, &mut state_for_chain);

                if let Some((chain, consumed, end_state)) = collect_chain(
                    &lines,
                    i,
                    chain_indent,
                    inline_bindings,
                    first_term,
                    state_for_chain,
                ) {
                    let rendered = render_chain(&chain, chain_indent, &mut counter);
                    out.push_str(&rendered);
                    in_string = end_state;
                    i += consumed;
                    continue;
                }
            }
        }

        out.push_str(line);
        i += 1;
    }

    // `with`-chain lowering uses the shadow-resistant `__typhon_Err__`
    // alias for the isinstance check (FINDINGS #104). Inject the
    // aliasing import once if any rewrite was made.
    if out.contains("__typhon_Err__") {
        prepend_typhon_err_alias_import(out)
    } else {
        out
    }
}

/// One unwrap step inside a `with`-chain.
#[derive(Debug)]
struct WithBinding {
    /// The variable name on the left-hand side of `=`.
    target: String,
    /// The Result-typed expression on the right (without the trailing `?`).
    expr: String,
}

#[derive(Debug)]
struct WithChain {
    bindings: Vec<WithBinding>,
    /// Body lines that run after every binding has unwrapped to its `Ok` value.
    /// Each entry is a verbatim line including its trailing newline.
    body: Vec<String>,
    /// The error-binding identifier supplied in `else <name>:`, or `None` for
    /// a default-propagating chain. A bare `else:` records `Some("_err")`.
    err_var: Option<String>,
    /// Body lines for the else branch, with the chain's base indentation
    /// removed (so each line starts at indent zero relative to the chain).
    else_body: Vec<String>,
}

/// Parse one or more inline `name = expr?,` bindings followed by a final
/// `name = expr?:` binding on a single `with` line. Returns the collected
/// bindings and the terminator character of the *last* binding, which will
/// be `:` for a complete inline chain and `,` when the chain continues onto
/// the next line.
fn parse_inline_with_bindings(s: &str) -> Option<(Vec<WithBinding>, char)> {
    // Strip any trailing `# comment` (string-aware) before slicing so a
    // chain like `with a = f()?: # ok` parses the same as without the
    // comment.
    let without_comment = strip_trailing_comment(s);
    let trimmed = without_comment.trim_end();
    // Walk top-level (`?,` or `?:`) terminators, slicing each binding segment.
    let bytes = trimmed.as_bytes();
    let mut bindings = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    let mut in_str: Option<u8> = None;
    let mut i = 0usize;
    let mut last_term: Option<char> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' | b'"' => in_str = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'?' if depth == 0 && i + 1 < bytes.len() => {
                let next = bytes[i + 1];
                if next == b',' || next == b':' {
                    let term = if next == b',' { ',' } else { ':' };
                    let segment = &trimmed[start..=i + 1];
                    let (binding, t) = parse_with_binding(segment)?;
                    if t != term {
                        return None;
                    }
                    bindings.push(binding);
                    last_term = Some(term);
                    i += 2;
                    // Skip whitespace before the next binding.
                    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                        i += 1;
                    }
                    start = i;
                    if term == ':' {
                        // Inline head ended; anything trailing means malformed.
                        if i != bytes.len() {
                            return None;
                        }
                        break;
                    }
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if bindings.is_empty() {
        return None;
    }
    // If we exited without seeing a final `?:`, the chain continues on the
    // next line — require the trailing terminator on the last binding to be
    // `,` so the multi-line scanner can resume.
    let term = last_term?;
    if term == ',' && start != bytes.len() {
        // There's trailing junk after the last `?,` on this line.
        return None;
    }
    Some((bindings, term))
}

/// Parse one `name = expr?(,|:)` segment from the tail of a binding line.
///
/// Returns the binding plus the terminator character (`,` or `:`).
fn parse_with_binding(s: &str) -> Option<(WithBinding, char)> {
    let s = s.trim_end();
    let term = if s.ends_with("?,") {
        ','
    } else if s.ends_with("?:") {
        ':'
    } else {
        return None;
    };
    let head = &s[..s.len() - 2];
    // Split on the first `=` at depth 0.
    let eq = find_assignment_eq(head)?;
    let target = head[..eq].trim();
    let expr = head[eq + 1..].trim();
    if target.is_empty() || expr.is_empty() {
        return None;
    }
    Some((
        WithBinding {
            target: target.to_owned(),
            expr: expr.to_owned(),
        },
        term,
    ))
}

/// Collect a `with`-chain that starts at `lines[start]`. Returns the chain,
/// the number of consumed lines, and the resulting triple-quoted-string state.
///
/// Returns `None` (and consumes no lines) if the construct is malformed —
/// the caller will then emit the source unchanged so the underlying parser
/// can surface the syntax error with full context.
fn collect_chain(
    lines: &[&str],
    start: usize,
    chain_indent: &str,
    inline_bindings: Vec<WithBinding>,
    first_term: char,
    initial_string_state: Option<StringMode>,
) -> Option<(WithChain, usize, Option<StringMode>)> {
    let mut bindings = inline_bindings;
    let mut idx = start + 1;
    let mut term = first_term;
    let mut in_string = initial_string_state;

    // Collect continuation binding lines while the previous terminator was `,`.
    while term == ',' && idx < lines.len() {
        let line = lines[idx];
        let raw = line.trim_end_matches(['\n', '\r']);
        let _ = scan_line_code_end(raw, &mut in_string);

        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        let line_indent = &raw[..indent_len];
        // Continuation lines must be indented strictly past the `with`.
        if line_indent.len() <= chain_indent.len() {
            return None;
        }
        let body = &raw[indent_len..];
        let (binding, t) = parse_with_binding(body)?;
        bindings.push(binding);
        term = t;
        idx += 1;
    }
    // The terminating `:` must have been seen by now.
    if term != ':' {
        return None;
    }

    // The success body: every line whose indent exceeds the chain's. Strip the
    // chain indent from each line so the caller can re-indent uniformly.
    let mut body = Vec::new();
    while idx < lines.len() {
        let line = lines[idx];
        let raw = line.trim_end_matches(['\n', '\r']);
        if raw.trim().is_empty() {
            // Pass blank lines through verbatim.
            let _ = scan_line_code_end(raw, &mut in_string);
            body.push(line.to_string());
            idx += 1;
            continue;
        }
        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        if indent_len <= chain_indent.len() {
            break;
        }
        let _ = scan_line_code_end(raw, &mut in_string);
        body.push(line.to_string());
        idx += 1;
    }
    if body.is_empty() {
        return None;
    }

    // Optional `else <name>:` continuation at the chain indent. The header
    // must end with `:`; a malformed `else err` (no colon) is *not* silently
    // accepted — the whole chain is rejected and emitted verbatim so the
    // underlying Python parser surfaces a syntax error pointing at the
    // mistake.
    let mut err_var = None;
    let mut else_body = Vec::new();
    if idx < lines.len() {
        let line = lines[idx];
        let raw = line.trim_end_matches(['\n', '\r']);
        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        let header = raw[indent_len..].trim_end();
        let is_else_header =
            indent_len == chain_indent.len() && (header == "else:" || header.starts_with("else "));
        if is_else_header {
            // Require a trailing colon on the header. `else err` (no colon)
            // is rejected so the user sees a Python-level syntax error.
            if !header.ends_with(':') {
                return None;
            }
            err_var = Some(if header == "else:" {
                "_err".to_owned()
            } else {
                parse_else_var(header)?
            });
            let _ = scan_line_code_end(raw, &mut in_string);
            idx += 1;
            while idx < lines.len() {
                let l = lines[idx];
                let r = l.trim_end_matches(['\n', '\r']);
                if r.trim().is_empty() {
                    let _ = scan_line_code_end(r, &mut in_string);
                    else_body.push(l.to_string());
                    idx += 1;
                    continue;
                }
                let ind = r.find(|c: char| !c.is_whitespace()).unwrap_or(r.len());
                if ind <= chain_indent.len() {
                    break;
                }
                let _ = scan_line_code_end(r, &mut in_string);
                else_body.push(l.to_string());
                idx += 1;
            }
            if else_body.is_empty() {
                return None;
            }
        }
    }

    let consumed = idx - start;
    Some((
        WithChain {
            bindings,
            body,
            err_var,
            else_body,
        },
        consumed,
        in_string,
    ))
}

fn parse_else_var(header: &str) -> Option<String> {
    let rest = header.strip_prefix("else")?.trim_start();
    if let Some(name) = rest.strip_suffix(':') {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        if !name
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
        {
            return None;
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(name.to_owned())
    } else {
        None
    }
}

/// Render a `with`-chain back into Python. The output is a flat sequence of
/// guarded `if isinstance(tmp, Err): ...` checks followed by the success body.
///
/// The success body lines originated one indent level inside the `with`
/// header; after flattening they become siblings of the unwrap statements and
/// must shed exactly that level. Else-body lines stay inside the new `if`
/// block, so their indent relative to the chain is preserved by emitting
/// them verbatim. The unit-indent is detected from the source itself (the
/// first non-blank body line) rather than assumed to be four spaces, so
/// tab-indented or two-space code round-trips correctly.
/// Replace every whole-word occurrence of `from` with `to` in `line`,
/// respecting Python identifier boundaries (a leading or trailing
/// alphanumeric / underscore character disqualifies the match) **and**
/// skipping over content inside regular string literals and `#`
/// comments so the substitution can't corrupt user-visible text like
/// `log.error("err occurred")` when renaming a synthesised `err`
/// binding.
///
/// F-string interpolations (`f"... {EXPR} ..."`) are walked as code:
/// the literal portions are left alone, but `{ ... }` expressions are
/// rescanned so the identifier `err` inside `f"{err.field}"` is
/// renamed exactly as it would be at top level. The scanner only
/// handles single-line forms (matching the call site, which runs on
/// already-line-split text from the `else err:` body).
fn rename_whole_word(line: &str, from: &str, to: &str) -> String {
    if from.is_empty() || !line.contains(from) {
        return line.to_owned();
    }
    let bytes = line.as_bytes();
    let from_bytes = from.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;
    /// Per-active-string state used to flip between literal-text and
    /// interpolation modes correctly for f-strings.
    #[derive(Clone, Copy)]
    struct StrState {
        quote: u8,
        /// `true` when this string opened with `f"` / `f'` (or any
        /// case-insensitive prefix containing `f`). Drives the `{` →
        /// interpolation transition.
        is_fstring: bool,
        /// `true` once we've stepped into a `{ ... }` block. While
        /// set, the scanner treats characters as code, recursing on
        /// nested strings as needed.
        in_interp: bool,
    }
    let mut stack: Vec<StrState> = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(top) = stack.last().copied() {
            if top.in_interp {
                // Inside `f"... { CODE }"` we treat characters as code,
                // recursing on nested string opens, and closing the
                // interpolation on the matching `}` at depth 0 of the
                // current interpolation. Track a local paren-style
                // depth for `{ }` so dict literals inside the
                // interpolation don't terminate it early.
                match b {
                    b'}' => {
                        // Close the interpolation, fall back to string
                        // literal mode.
                        out.push('}');
                        i += 1;
                        if let Some(last) = stack.last_mut() {
                            last.in_interp = false;
                        }
                        continue;
                    }
                    b'"' | b'\'' => {
                        // Nested string literal inside the
                        // interpolation. Push a fresh frame; the
                        // outer f-string stays on the stack so we
                        // return to it when this one closes.
                        let is_fstring = is_fstring_prefix(bytes, i);
                        stack.push(StrState {
                            quote: b,
                            is_fstring,
                            in_interp: false,
                        });
                        out.push(b as char);
                        i += 1;
                        continue;
                    }
                    _ => {
                        // Code byte — fall through to the bottom
                        // matcher so identifier renaming applies.
                    }
                }
            } else {
                // Inside a literal portion of a string. Honour escapes
                // and watch for the closing quote / f-string `{`.
                if b == b'\\' && i + 1 < bytes.len() {
                    let end = (i + 2).min(bytes.len());
                    out.push_str(&line[i..end]);
                    i = end;
                    continue;
                }
                if b == top.quote {
                    stack.pop();
                    out.push(b as char);
                    i += 1;
                    continue;
                }
                if top.is_fstring && b == b'{' {
                    // f-string interpolation opens. `{{` is a literal
                    // brace — handle by checking the next byte.
                    if bytes.get(i + 1) == Some(&b'{') {
                        out.push_str("{{");
                        i += 2;
                        continue;
                    }
                    out.push('{');
                    i += 1;
                    if let Some(last) = stack.last_mut() {
                        last.in_interp = true;
                    }
                    continue;
                }
                // Plain literal byte — preserve UTF-8.
                let ch_len = utf8_char_len(bytes, i);
                out.push_str(&line[i..i + ch_len]);
                i += ch_len;
                continue;
            }
        }
        // Top-level (or interpolation-code) byte.
        if b == b'#' {
            // Comment to end of line — copy verbatim.
            out.push_str(&line[i..]);
            break;
        }
        if b == b'"' || b == b'\'' {
            let is_fstring = is_fstring_prefix(bytes, i);
            stack.push(StrState {
                quote: b,
                is_fstring,
                in_interp: false,
            });
            out.push(b as char);
            i += 1;
            continue;
        }
        // Word match in code position.
        if i + from_bytes.len() <= bytes.len() && &bytes[i..i + from_bytes.len()] == from_bytes {
            let before_ok = i == 0 || !is_ident_continuation(bytes[i - 1]);
            let after_ok = i + from_bytes.len() == bytes.len()
                || !is_ident_continuation(bytes[i + from_bytes.len()]);
            if before_ok && after_ok {
                out.push_str(to);
                i += from_bytes.len();
                continue;
            }
        }
        // Copy one UTF-8 character — pushing `bytes[i] as char` would
        // mojibake non-ASCII identifiers / string-literal continuation
        // bytes.
        let ch_len = utf8_char_len(bytes, i);
        out.push_str(&line[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Return `true` when the quote byte at `bytes[i]` is preceded by a
/// string-prefix that includes `f` / `F` (`f"..."`, `Rf"..."`, etc.).
/// We scan backwards over the legal Python prefix bytes (`f`, `F`, `r`,
/// `R`, `b`, `B`, `u`, `U`) — order doesn't matter, but `b`/`B` makes
/// the string non-f. We only require that an `f`/`F` is present.
fn is_fstring_prefix(bytes: &[u8], quote_idx: usize) -> bool {
    let mut j = quote_idx;
    let mut has_f = false;
    let mut has_b = false;
    while j > 0 {
        let prev = bytes[j - 1];
        if prev == b'f' || prev == b'F' {
            has_f = true;
            j -= 1;
            continue;
        }
        if prev == b'r' || prev == b'R' || prev == b'u' || prev == b'U' {
            j -= 1;
            continue;
        }
        if prev == b'b' || prev == b'B' {
            has_b = true;
            j -= 1;
            continue;
        }
        break;
    }
    has_f && !has_b
}

/// Length in bytes of the UTF-8 character starting at `bytes[i]`.
/// Used by the byte-cursor scanners in this module to keep multi-byte
/// sequences intact while still allowing constant-time byte indexing.
fn utf8_char_len(bytes: &[u8], i: usize) -> usize {
    let b = bytes[i];
    if b < 0x80 {
        1
    } else if b < 0xC0 {
        // Stray continuation byte — copy one byte to make progress.
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
    .min(bytes.len() - i)
}

fn is_ident_continuation(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn render_chain(chain: &WithChain, chain_indent: &str, counter: &mut usize) -> String {
    let mut out = String::new();

    // Detect the body's own indent unit (the leading whitespace beyond the
    // chain indent on the first non-blank body line). Fall back to four
    // spaces only when the body is entirely blank, which is unreachable in
    // valid Python but defensive against the corner case.
    let unit_indent: String = chain
        .body
        .iter()
        .find_map(|line| {
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if trimmed.trim().is_empty() {
                return None;
            }
            let leading_len = trimmed
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(trimmed.len());
            let leading = &trimmed[..leading_len];
            leading
                .strip_prefix(chain_indent)
                .map(|extra| extra.to_owned())
        })
        .unwrap_or_else(|| "    ".to_owned());
    let inner_indent = format!("{}{}", chain_indent, unit_indent);

    for binding in &chain.bindings {
        let tmp = format!("__typhon_with_{}__", *counter);
        *counter += 1;

        out.push_str(chain_indent);
        out.push_str(&tmp);
        out.push_str(" = ");
        out.push_str(&binding.expr);
        out.push('\n');

        out.push_str(chain_indent);
        out.push_str("if isinstance(");
        out.push_str(&tmp);
        // Shadow-resistant alias — see FINDINGS #104.
        out.push_str(", __typhon_Err__):\n");

        match (&chain.err_var, !chain.else_body.is_empty()) {
            (Some(name), true) => {
                // Uniquify the user-visible `err` name so multiple
                // `with`-chains in the same function don't trip the
                // resolver's `tyc::immutable_assign` re-declaration
                // check on a shared `let err = ...`. The synthesised
                // name keys off the current tmp counter (which has
                // already been bumped for this binding) so it's
                // guaranteed unique without interleaving with the
                // chain's own `__typhon_with_N__` numbering.
                let unique_err = format!("__typhon_with_err_{}__", *counter - 1);
                out.push_str(&inner_indent);
                out.push_str("let ");
                out.push_str(&unique_err);
                out.push_str(" = ");
                out.push_str(&tmp);
                out.push_str(".error\n");
                for line in &chain.else_body {
                    out.push_str(&rename_whole_word(line, name, &unique_err));
                }
            }
            _ => {
                out.push_str(&inner_indent);
                out.push_str("return ");
                out.push_str(&tmp);
                out.push('\n');
            }
        }

        // Emit the user-visible unwrap as `let NAME = tmp.value` so the
        // resolver sees an explicit binding-kind keyword. Without `let`
        // here, Rule-2's `tyc::missing_binding_kind` would fire on the
        // lowered statement (and the diagnostic would point at the
        // synthesised line rather than the user's `with` source).
        out.push_str(chain_indent);
        out.push_str("let ");
        out.push_str(&binding.target);
        out.push_str(" = ");
        out.push_str(&tmp);
        out.push_str(".value\n");
    }

    // Success body: strip the unit-indent the `with` header imposed on each
    // line so the statements become siblings of the unwraps.
    for line in &chain.body {
        if line.trim().is_empty() {
            out.push_str(line);
        } else if let Some(stripped) = line.strip_prefix(inner_indent.as_str()) {
            out.push_str(chain_indent);
            out.push_str(stripped);
        } else {
            // Indent didn't match — fall back to emitting verbatim so the
            // user sees a precise Python indentation error rather than a
            // silently mangled body.
            out.push_str(line);
        }
    }

    out
}

// ── `gather` block expansion ──────────────────────────────────────────────────

/// Expand a `gather` block into a concurrent task pattern.
///
/// `gather:` and `gather(strategy="…")` block forms are accepted. The inner
/// bindings are simple `name = expr` lines that must all be `await`able. The
/// default form lowers to `asyncio.TaskGroup`, which cancels siblings on the
/// first failure:
///
/// ```text
/// # Typhon
/// gather:
///     user  = fetch_user(id)
///     posts = fetch_posts(id)
///
/// # Lowered Python
/// async with asyncio.TaskGroup() as __typhon_tg__:
///     __typhon_gather_0__ = __typhon_tg__.create_task(fetch_user(id))
///     __typhon_gather_1__ = __typhon_tg__.create_task(fetch_posts(id))
/// user  = __typhon_gather_0__.result()
/// posts = __typhon_gather_1__.result()
/// ```
///
/// The `gather(strategy="best-effort"):` form lowers to a plain
/// `asyncio.gather(..., return_exceptions=True)` await so callers can inspect
/// per-task failures:
///
/// ```text
/// __typhon_gather_results__ = await asyncio.gather(
///     fetch_user(id), fetch_posts(id), return_exceptions=True,
/// )
/// user, posts = __typhon_gather_results__
/// ```
///
/// Lines inside triple-quoted strings are passed through verbatim. Malformed
/// blocks (no bindings, mixed body lines, etc.) are emitted unchanged so the
/// Python parser surfaces a precise error.
pub fn expand_gather_blocks(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut counter: usize = 0;
    let mut in_string: Option<StringMode> = None;
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let pre = in_string;
        let raw = line.trim_end_matches(['\n', '\r']);
        let _ = scan_line_code_end(raw, &mut in_string);

        if pre.is_some() {
            out.push_str(line);
            i += 1;
            continue;
        }

        let indent_len = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        let header_indent = &raw[..indent_len];
        let body = &raw[indent_len..];

        let parsed = parse_gather_header(body);
        if let Some(strategy) = parsed {
            let mut block_state = in_string;
            if let Some((bindings, consumed, end_state)) =
                collect_gather_bindings(&lines, i, header_indent, &mut block_state)
            {
                let rendered =
                    render_gather_block(&bindings, header_indent, strategy, &mut counter);
                out.push_str(&rendered);
                in_string = end_state;
                i += consumed;
                continue;
            }
        }

        out.push_str(line);
        i += 1;
    }

    out
}

/// Gather strategy chosen at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatherStrategy {
    /// Default — lower to `asyncio.TaskGroup` (cancels siblings on first failure).
    TaskGroup,
    /// `strategy="best-effort"` — lower to `asyncio.gather(..., return_exceptions=True)`.
    BestEffort,
}

/// Parse a `gather:` or `gather(strategy="..."):` header line, returning the
/// chosen strategy when the syntax is well-formed.
fn parse_gather_header(body: &str) -> Option<GatherStrategy> {
    let trimmed = body.trim_end();
    if trimmed == "gather:" {
        return Some(GatherStrategy::TaskGroup);
    }
    // gather(strategy="..."):
    let inner = trimmed.strip_prefix("gather(")?.strip_suffix("):")?.trim();
    if !inner.starts_with("strategy") {
        return None;
    }
    let after = inner["strategy".len()..].trim_start();
    let value = after.strip_prefix('=')?.trim();
    // Accept either single- or double-quoted string literal.
    let stripped = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))?;
    match stripped {
        "task-group" | "taskgroup" | "default" => Some(GatherStrategy::TaskGroup),
        "best-effort" | "best_effort" | "besteffort" => Some(GatherStrategy::BestEffort),
        _ => None,
    }
}

/// One `name = expr` binding inside a `gather:` block.
#[derive(Debug)]
struct GatherBinding {
    name: String,
    expr: String,
}

/// Collect indented `name = expr` bindings under a `gather:` header.
/// Returns the bindings, the number of lines consumed (header + bindings),
/// and the resulting triple-quoted-string state.
fn collect_gather_bindings(
    lines: &[&str],
    start: usize,
    header_indent: &str,
    in_string: &mut Option<StringMode>,
) -> Option<(Vec<GatherBinding>, usize, Option<StringMode>)> {
    let mut bindings = Vec::new();
    let mut idx = start + 1;
    let mut block_state = *in_string;

    while idx < lines.len() {
        let line = lines[idx];
        let raw = line.trim_end_matches(['\n', '\r']);
        if raw.trim().is_empty() {
            // Allow blank lines inside the block; they don't add bindings.
            let _ = scan_line_code_end(raw, &mut block_state);
            idx += 1;
            continue;
        }
        let line_indent = raw.find(|c: char| !c.is_whitespace()).unwrap_or(raw.len());
        if line_indent <= header_indent.len() {
            break;
        }
        let body = &raw[line_indent..];
        // Use scan_line_code_end (which handles triple-quoted strings and raw
        // strings) to find where code ends and a comment begins.  Only then
        // run find_assignment_eq on the comment-free slice so that `=` inside
        // a comment (e.g. `# note: x = 1`) can never be mistaken for the
        // assignment operator.  FINDINGS #93 — hardened per review.
        let mut snap = block_state;
        let code_end = scan_line_code_end(body, &mut snap);
        let code = body[..code_end].trim_end();
        let eq = find_assignment_eq(code)?;
        let name = code[..eq].trim().to_owned();
        let expr = code[eq + 1..].trim().to_owned();
        if name.is_empty() || expr.is_empty() {
            return None;
        }
        if !is_python_ident(&name) {
            return None;
        }
        bindings.push(GatherBinding { name, expr });
        let _ = scan_line_code_end(raw, &mut block_state);
        idx += 1;
    }

    if bindings.is_empty() {
        return None;
    }
    let consumed = idx - start;
    Some((bindings, consumed, block_state))
}

/// True when binding `b` at position `idx` references the name of any
/// earlier binding in the same gather block. Used to demote dependent
/// gather blocks to sequential awaits — concurrent lowering would
/// reference an undefined name inside `create_task(...)` and crash at
/// runtime with `UnboundLocalError`. FINDINGS #60.
fn gather_binding_depends_on_earlier(bindings: &[GatherBinding], idx: usize) -> bool {
    if idx == 0 {
        return false;
    }
    let expr = &bindings[idx].expr;
    for prior in &bindings[..idx] {
        if expr_references_identifier(expr, &prior.name) {
            return true;
        }
    }
    false
}

/// Scan `expr` for a free occurrence of `name`. Word-boundary check using
/// Python identifier-character rules; doesn't honour string literals or
/// nested scopes, but those almost never trigger a false positive on a
/// gather binding name (which is necessarily a `name = expr` shape).
fn expr_references_identifier(expr: &str, name: &str) -> bool {
    let bytes = expr.as_bytes();
    let needle = name.as_bytes();
    let n = needle.len();
    if n == 0 || bytes.len() < n {
        return false;
    }
    let is_id_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i + n <= bytes.len() {
        if &bytes[i..i + n] == needle {
            let prev_ok = i == 0 || !is_id_char(bytes[i - 1]);
            let next_ok = i + n == bytes.len() || !is_id_char(bytes[i + n]);
            if prev_ok && next_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// True when any binding in this gather block references the name of an
/// earlier binding — the lowering must demote to sequential awaits.
fn gather_has_dependent_bindings(bindings: &[GatherBinding]) -> bool {
    (0..bindings.len()).any(|i| gather_binding_depends_on_earlier(bindings, i))
}

/// Render a `gather` block into the chosen Python concurrency primitive.
fn render_gather_block(
    bindings: &[GatherBinding],
    header_indent: &str,
    strategy: GatherStrategy,
    counter: &mut usize,
) -> String {
    let mut out = String::new();
    // Dependent bindings (b's expr references a's name) cannot be concurrent.
    // Demote to sequential `let x = await expr` so the lowering is at least
    // correct; a future diagnostic could warn that the gather intent was
    // demoted. FINDINGS #60.
    if gather_has_dependent_bindings(bindings) {
        for b in bindings {
            out.push_str(header_indent);
            out.push_str("let ");
            out.push_str(&b.name);
            out.push_str(" = await ");
            out.push_str(&b.expr);
            out.push('\n');
        }
        return out;
    }
    match strategy {
        GatherStrategy::TaskGroup => {
            let tg = format!("__typhon_tg_{}__", *counter);
            *counter += 1;
            out.push_str(header_indent);
            out.push_str("async with asyncio.TaskGroup() as ");
            out.push_str(&tg);
            out.push_str(":\n");
            let inner = format!("{}    ", header_indent);
            let mut task_names = Vec::with_capacity(bindings.len());
            for b in bindings {
                let task_name = format!("__typhon_gather_{}__", *counter);
                *counter += 1;
                out.push_str(&inner);
                out.push_str(&task_name);
                out.push_str(" = ");
                out.push_str(&tg);
                out.push_str(".create_task(");
                out.push_str(&b.expr);
                out.push_str(")\n");
                task_names.push(task_name);
            }
            // Emit `let` on each user-named binding so the resolver records
            // them as explicit immutable bindings. The `gather:` keyword
            // already advertises single-assignment semantics, so `let` is
            // always the right mutability; `mut` would be wrong. Without
            // the `let` here, the `tyc::missing_binding_kind` Rule-2
            // enforcement would fire on the lowered assignment.
            for (b, task) in bindings.iter().zip(task_names.iter()) {
                out.push_str(header_indent);
                out.push_str("let ");
                out.push_str(&b.name);
                out.push_str(" = ");
                out.push_str(task);
                out.push_str(".result()\n");
            }
        }
        GatherStrategy::BestEffort => {
            let results = format!("__typhon_gather_{}__", *counter);
            *counter += 1;
            out.push_str(header_indent);
            out.push_str(&results);
            out.push_str(" = await asyncio.gather(");
            for (idx, b) in bindings.iter().enumerate() {
                if idx > 0 {
                    out.push_str(", ");
                }
                out.push_str(&b.expr);
            }
            if !bindings.is_empty() {
                out.push_str(", ");
            }
            out.push_str("return_exceptions=True)\n");
            // Index each result into a `let NAME = __typhon_gather_N__[i]`
            // line so every user-visible binding carries an explicit
            // binding-kind keyword. We deliberately avoid emitting the
            // earlier tuple-destructure form (`a, b = results`) because
            // Rule-2's `tyc::missing_binding_kind` enforcement would fire
            // on the synthesised target without a way for the user to
            // add `let` themselves (the `gather:` block keyword is the
            // closest surface form, and that exception only applies to
            // the strict TaskGroup lowering today).
            for (idx, b) in bindings.iter().enumerate() {
                out.push_str(header_indent);
                out.push_str("let ");
                out.push_str(&b.name);
                out.push_str(" = ");
                out.push_str(&results);
                out.push('[');
                out.push_str(&idx.to_string());
                out.push_str("]\n");
            }
        }
    }
    out
}

// ── `go` spawn expansion ──────────────────────────────────────────────────────

/// Rewrite every `go <call>` and `go <call> -> name` statement into a call to
/// the `typhon_runtime.tasks.spawn` helper.
///
/// `go fetch(x)` becomes `typhon_runtime.tasks.spawn(fetch(x))`.
/// `go fetch(x) -> fut` becomes `fut = typhon_runtime.tasks.spawn(fetch(x))`.
///
/// The pass leaves any other `go`-prefixed line alone so identifiers like
/// `goto` aren't accidentally rewritten.
pub fn expand_go_calls(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut in_string: Option<StringMode> = None;
    for line in source.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\n', '\r']);
        let terminator = &line[raw.len()..];
        let pre = in_string;
        let code_end = scan_line_code_end(raw, &mut in_string);
        if pre.is_some() {
            out.push_str(line);
            continue;
        }
        let code = &raw[..code_end];
        let comment = &raw[code_end..];
        let indent_len = code
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(code.len());
        let indent = &code[..indent_len];
        let body = code[indent_len..].trim_end();
        if let Some(rest) = body.strip_prefix("go ") {
            if let Some((call_expr, handle)) = parse_go_call(rest) {
                if let Some(handle) = handle {
                    // Emit `let handle = …` so the user-visible task
                    // identifier carries an explicit binding-kind keyword
                    // and Rule-2's `tyc::missing_binding_kind` doesn't
                    // fire on the synthesised assignment.
                    out.push_str(indent);
                    out.push_str("let ");
                    out.push_str(&handle);
                    out.push_str(" = typhon_runtime.tasks.spawn(");
                    out.push_str(&call_expr);
                    out.push(')');
                } else {
                    out.push_str(indent);
                    out.push_str("typhon_runtime.tasks.spawn(");
                    out.push_str(&call_expr);
                    out.push(')');
                }
                out.push_str(comment);
                out.push_str(terminator);
                continue;
            }
        }
        out.push_str(line);
    }
    out
}

/// Parse the tail of a `go` line into `(call_expr, Option<handle_name>)`.
///
/// Accepts:
/// - `f(x)`               → `("f(x)", None)`
/// - `mod.f(x) -> fut`    → `("mod.f(x)", Some("fut"))`
///
/// Returns `None` for shapes we don't recognise (so the caller leaves the line
/// alone and the parser surfaces a precise error).
fn parse_go_call(rest: &str) -> Option<(String, Option<String>)> {
    let rest = rest.trim();
    // Split on `->` at depth 0, with string-literal and backslash-escape
    // awareness so `go f("\"->")` is not mistakenly split at the inner `->`.
    let mut depth: i32 = 0;
    let bytes = rest.as_bytes();
    let mut arrow_at: Option<usize> = None;
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' {
                i += 1; // skip escaped character
            } else if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => in_str = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'-' if depth == 0 && i + 1 < bytes.len() && bytes[i + 1] == b'>' => {
                arrow_at = Some(i);
                break;
            }
            _ => {}
        }
        i += 1;
    }

    let (call_part, handle_part) = match arrow_at {
        Some(pos) => (rest[..pos].trim(), Some(rest[pos + 2..].trim())),
        None => (rest, None),
    };
    if call_part.is_empty() {
        return None;
    }
    // The call portion must look like a function call (ends with `)`).
    if !call_part.ends_with(')') {
        return None;
    }
    let handle = match handle_part {
        Some(h) if is_python_ident(h) => Some(h.to_owned()),
        Some(_) => return None,
        None => None,
    };
    Some((call_part.to_owned(), handle))
}

// ── `|>` pipe operator expansion ──────────────────────────────────────────────

/// Expand the `|>` pipe operator into nested function calls.
///
/// `x |> f` rewrites to `f(x)`. `x |> f(a, b)` rewrites to `f(x, a, b)`.
/// Chained pipes `x |> f |> g` rewrite left-to-right as `g(f(x))`. The
/// transformation runs on the textual source line-by-line before the regular
/// preprocessor.
///
/// Pipes are only rewritten when:
///
/// - They appear outside any string literal or comment.
/// - They appear at parenthesis depth 0 within the line (pipes inside a
///   parenthesised sub-expression must be broken out into their own binding).
/// - The right-hand side is a callable name (`f`, `mod.f`) optionally
///   followed by a parenthesised argument list.
///
/// Lines that begin inside a triple-quoted string are passed through
/// verbatim. A line that contains no top-level `|>` is unchanged.
pub fn expand_pipes(source: &str) -> String {
    // First, fold multi-line pipe segments back onto their preceding
    // line so the rest of the pass can stay line-oriented. Python
    // allows operators at the start of a continuation line inside a
    // parenthesised expression (black/ruff format `+`, `and`, `|`
    // that way); `|>` follows the same convention. FINDINGS #52.
    let joined = join_pipe_continuations(source);
    expand_pipes_line_by_line(&joined)
}

fn expand_pipes_line_by_line(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut in_string: Option<StringMode> = None;

    for line in source.split_inclusive('\n') {
        let pre_string = in_string;
        let raw = line.trim_end_matches(['\n', '\r']);
        // Preserve the original line terminator (LF, CRLF, or bare-EOF) so
        // mixed-newline files round-trip unchanged.
        let terminator = &line[raw.len()..];
        let code_end = scan_line_code_end(raw, &mut in_string);

        if pre_string.is_some() {
            result.push_str(line);
            continue;
        }

        let code = &raw[..code_end];
        // First, expand any `|>` that lives inside a parenthesised
        // sub-expression — the original line-only pass only looked at
        // depth-0 pipes, which left shapes like
        // `(1 |> add(2)) |> add(3)` un-rewritten on the inner pipe
        // and produced a parser error against the still-Typhon-only
        // `|>` token. The nested helper recurses into each balanced
        // `(...)` group so inner pipes are rewritten before the
        // outer pipe pass sees them. O28 / FINDINGS #117–#119.
        let nested_expanded = expand_pipes_in_subexpressions(code);
        let code_for_top: &str = &nested_expanded;
        let pipes = find_top_level_pipes(code_for_top);
        if pipes.is_empty() {
            // The nested pass may still have rewritten parenthesised
            // sub-expressions even when no top-level pipe remained.
            // Stream the rewritten code (plus the original trailing
            // comment) into the buffer so that progress is preserved.
            if nested_expanded != code {
                result.push_str(&nested_expanded);
                result.push_str(&raw[code_end..]);
                result.push_str(terminator);
            } else {
                result.push_str(line);
            }
            continue;
        }

        let rewritten = match rewrite_pipe_line(code_for_top, &pipes) {
            Some(s) => s,
            None => {
                // Bail out — pass the line through unchanged so the regular
                // parser produces a coherent diagnostic at the `|>` token.
                result.push_str(line);
                continue;
            }
        };

        result.push_str(&rewritten);
        // Preserve any trailing comment, then re-attach the original
        // newline bytes (which may be `\r\n` on Windows-authored files).
        result.push_str(&raw[code_end..]);
        result.push_str(terminator);
    }

    result
}

/// Recursively expand `|>` operators inside balanced `(...)` groups.
/// Walks `code` left-to-right, treating top-level `(` / `)` pairs as
/// independent sub-expressions; each pair's body is processed first
/// (so nested pipes are rewritten innermost-first), then any pipes
/// still present at the body's top level are rewritten via the same
/// machinery the line-level pass uses. Triple-quoted and ordinary
/// string literals are passed through verbatim.
///
/// This pass is a no-op when the input contains no `|>` token at all.
/// O28 / FINDINGS #119.
fn expand_pipes_in_subexpressions(code: &str) -> String {
    if !code.contains("|>") {
        return code.to_owned();
    }
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len());
    let mut in_str: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            out.push(b as char);
            if b == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if b == b'"' || b == b'\'' {
            in_str = Some(b);
            out.push(b as char);
            i += 1;
            continue;
        }
        if b == b'(' {
            // Find the matching `)`, tracking string state inside.
            let mut depth: i32 = 1;
            let mut j = i + 1;
            let mut local_str: Option<u8> = None;
            while j < bytes.len() && depth > 0 {
                let c = bytes[j];
                if let Some(q) = local_str {
                    if c == b'\\' && j + 1 < bytes.len() {
                        j += 2;
                        continue;
                    }
                    if c == q {
                        local_str = None;
                    }
                    j += 1;
                    continue;
                }
                match c {
                    b'"' | b'\'' => local_str = Some(c),
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            if depth != 0 {
                // Unmatched paren — give up and emit verbatim. The
                // downstream parser will produce a coherent diagnostic.
                out.push('(');
                i += 1;
                continue;
            }
            let inner_bytes = &bytes[i + 1..j - 1];
            let inner = std::str::from_utf8(inner_bytes).unwrap_or("");
            // Recurse: rewrite any deeper-nested parens first, then
            // run the top-level pass on the result.
            let processed_inner = {
                let nested = expand_pipes_in_subexpressions(inner);
                let pipes = find_top_level_pipes(&nested);
                if pipes.is_empty() {
                    nested
                } else {
                    rewrite_pipe_line(&nested, &pipes).unwrap_or(nested)
                }
            };
            out.push('(');
            out.push_str(&processed_inner);
            out.push(')');
            i = j;
            continue;
        }
        out.push(b as char);
        i += 1;
    }
    out
}

/// Fold multi-line pipe segments back onto their preceding line so the
/// line-based [`expand_pipes`] pass can rewrite the chain. When a line
/// inside an unclosed parenthesised expression starts (after whitespace)
/// with `|>`, we treat it as a continuation of the previous logical
/// line and join them with a single space. Outside parentheses the
/// pass is a no-op — Python doesn't permit operator-at-line-start
/// there anyway.
///
/// Lines that begin mid-string (e.g. triple-quoted continuation) are
/// passed through verbatim so we don't perturb string contents.
fn join_pipe_continuations(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut buffered: Option<(String, String)> = None; // (line_without_terminator, terminator)
    let mut paren_depth: i32 = 0;
    let mut in_string: Option<StringMode> = None;

    let flush = |buffered: &mut Option<(String, String)>, out: &mut String| {
        if let Some((line, term)) = buffered.take() {
            out.push_str(&line);
            out.push_str(&term);
        }
    };

    for line in source.split_inclusive('\n') {
        let raw = line.trim_end_matches(['\n', '\r']);
        let terminator = &line[raw.len()..];

        // Snapshot the string state at the start of this line.
        let pre_string = in_string;
        let _ = scan_line_code_end(raw, &mut in_string);

        // If we entered this line inside a string literal, just emit
        // the buffer-then-line as-is — we can't safely join across
        // string boundaries.
        if pre_string.is_some() {
            flush(&mut buffered, &mut out);
            out.push_str(line);
            continue;
        }

        // Look for a leading `|>` (after any whitespace), but only
        // accept it as a continuation if we are inside an unclosed
        // parenthesised expression.
        let trimmed = raw.trim_start();
        let is_pipe_continuation =
            paren_depth > 0 && trimmed.starts_with("|>") && buffered.is_some();

        if is_pipe_continuation {
            // Merge with the buffered previous line.
            if let Some((prev_line, prev_term)) = buffered.take() {
                // Drop the terminator on the previous line and the
                // leading whitespace on this one, joining with a single
                // space so existing tokenization keeps working.
                let joined = format!("{} {}", prev_line, trimmed);
                buffered = Some((joined, prev_term));
            }
        } else {
            // Different shape — flush whatever was buffered and start
            // buffering this line in its place.
            flush(&mut buffered, &mut out);
            buffered = Some((raw.to_owned(), terminator.to_owned()));
        }

        // Track parenthesis depth based on the original raw text (the
        // join preserves bracket balance because it just glues two
        // halves together). Skip bytes inside string literals and
        // after `#` comments so a line like `s = "(((" ` doesn't
        // mistakenly inflate `paren_depth` and trigger a bogus
        // continuation fold on a later `|>` at module level.
        let bytes = raw.as_bytes();
        let mut local_str: Option<u8> = None;
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(quote) = local_str {
                if b == b'\\' && i + 1 < bytes.len() {
                    // Escape skips the next byte (`\"`, `\'`, `\\`, etc).
                    i += 2;
                    continue;
                }
                if b == quote {
                    local_str = None;
                }
                i += 1;
                continue;
            }
            match b {
                b'#' => break,
                b'"' | b'\'' => local_str = Some(b),
                b'(' | b'[' | b'{' => paren_depth += 1,
                b')' | b']' | b'}' => paren_depth -= 1,
                _ => {}
            }
            i += 1;
        }
    }
    flush(&mut buffered, &mut out);
    out
}

/// Locate every `|>` token in `code` that sits at parenthesis depth 0 and
/// outside any string literal (single-, double-, or triple-quoted).
/// Returns the byte offset of the `|` character in each occurrence.
fn find_top_level_pipes(code: &str) -> Vec<usize> {
    let mut positions = Vec::new();
    let bytes = code.as_bytes();
    scan_inline_code(code, |i, depth, _in_string| {
        if depth == 0
            && bytes[i] == b'|'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'>'
            // Reject `||>` and `|>=` shapes (neither is valid Python today,
            // but be defensive).
            && (i == 0 || bytes[i - 1] != b'|')
            && (i + 2 >= bytes.len() || bytes[i + 2] != b'=')
        {
            positions.push(i);
            // Tell the scanner to skip the `>` so we don't re-enter this
            // branch on the next byte.
            return Some(2);
        }
        None
    });
    positions
}

/// Walk `code` byte-by-byte, tracking parenthesis depth and string state
/// (including triple-quoted forms). Calls `step(i, depth, in_string)` for
/// every byte position outside a string literal, and inside strings only
/// to allow `step` to inspect the state. `step` may return `Some(skip)` to
/// advance the cursor by `skip` extra bytes (used by token-aware callers
/// like `find_top_level_pipes` once they've matched a multi-byte operator).
///
/// The scanner is byte-level and ASCII-safe: it only inspects bytes that
/// are themselves ASCII (quotes, brackets, the backslash escape, and the
/// matching closing quote), and otherwise transparently advances by one
/// byte. UTF-8 continuation bytes never match any of these so multi-byte
/// characters are passed through correctly.
fn scan_inline_code<F>(code: &str, mut step: F)
where
    F: FnMut(usize, i32, Option<StringMode>) -> Option<usize>,
{
    let bytes = code.as_bytes();
    let mut depth: i32 = 0;
    let mut in_string: Option<StringMode> = None;
    let mut i = 0;
    while i < bytes.len() {
        if let Some(extra) = step(i, depth, in_string) {
            i += extra;
            continue;
        }
        let c = bytes[i];
        match in_string {
            Some(StringMode::Single) | Some(StringMode::Double) => {
                // Single-line strings: honour backslash escapes, look for
                // the matching closing quote.
                let close = match in_string {
                    Some(StringMode::Single) => b'\'',
                    _ => b'"',
                };
                if c == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if c == close {
                    in_string = None;
                }
                i += 1;
            }
            Some(StringMode::TripleSingle) | Some(StringMode::TripleDouble) => {
                let triple: &[u8] = match in_string {
                    Some(StringMode::TripleSingle) => b"'''",
                    _ => b"\"\"\"",
                };
                if i + 3 <= bytes.len() && &bytes[i..i + 3] == triple {
                    in_string = None;
                    i += 3;
                } else {
                    i += 1;
                }
            }
            None => match c {
                b'\'' | b'"' => {
                    let triple: &[u8] = if c == b'"' { b"\"\"\"" } else { b"'''" };
                    if i + 3 <= bytes.len() && &bytes[i..i + 3] == triple {
                        in_string = Some(if c == b'"' {
                            StringMode::TripleDouble
                        } else {
                            StringMode::TripleSingle
                        });
                        i += 3;
                    } else {
                        in_string = Some(if c == b'"' {
                            StringMode::Double
                        } else {
                            StringMode::Single
                        });
                        i += 1;
                    }
                }
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    i += 1;
                }
                b')' | b']' | b'}' => {
                    depth -= 1;
                    i += 1;
                }
                _ => {
                    i += 1;
                }
            },
        }
    }
}

/// Rewrite a single line `code` containing top-level pipe operators (at the
/// byte positions in `pipes`) into the equivalent nested-call form.
///
/// Returns `None` if any pipe right-hand-side does not match the supported
/// callable shape — the caller is expected to pass the line through unchanged.
fn rewrite_pipe_line(code: &str, pipes: &[usize]) -> Option<String> {
    // Split into segments delimited by `|>`. Leading/trailing whitespace on
    // each segment is preserved on the first segment (for indent) but trimmed
    // on intermediate ones.
    let mut segments: Vec<&str> = Vec::with_capacity(pipes.len() + 1);
    let mut last = 0;
    for &pos in pipes {
        segments.push(&code[last..pos]);
        last = pos + 2;
    }
    segments.push(&code[last..]);

    let first = segments[0];
    let indent_end = first
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(first.len());
    let indent = &first[..indent_end];
    let first_body = first[indent_end..].trim_end();

    // Identify any assignment / return prefix on the first segment so the
    // chain only consumes the right-hand expression.
    let (prefix, lhs_expr) = split_pipe_prefix(first_body);
    let lhs_expr = lhs_expr.trim();
    if lhs_expr.is_empty() {
        return None;
    }

    let mut acc = lhs_expr.to_string();
    for seg in &segments[1..] {
        let rhs = seg.trim();
        acc = apply_pipe_call(&acc, rhs)?;
    }

    Some(format!("{}{}{}", indent, prefix, acc))
}

/// Strip an optional `return ` or `LHS [op]= ` prefix from the head of a
/// pipe chain so the chain only consumes the right-hand expression.
///
/// Supports both bare and augmented assignment (`+=`, `-=`, `*=`, `/=`,
/// `%=`, `**=`, `//=`, `<<=`, `>>=`, `&=`, `|=`, `^=`, `@=`). The walrus
/// `:=` is an *expression* operator, not a statement-level assignment, so
/// it is treated as part of the chain LHS rather than a prefix.
fn split_pipe_prefix(s: &str) -> (&str, &str) {
    // `return ` or `return\t`.
    if let Some(rest) = s.strip_prefix("return ") {
        return (&s[..s.len() - rest.len()], rest);
    }
    if let Some(rest) = s.strip_prefix("return\t") {
        return (&s[..s.len() - rest.len()], rest);
    }
    // `yield ` (single-arg pipe yield is unusual but legal).
    if let Some(rest) = s.strip_prefix("yield ") {
        return (&s[..s.len() - rest.len()], rest);
    }
    // Otherwise look for an assignment-statement operator at depth 0.
    if let Some(eq_end) = find_statement_assignment_end(s) {
        let after = &s[eq_end..];
        let trim_start = after.len() - after.trim_start().len();
        return (&s[..eq_end + trim_start], &after[trim_start..]);
    }
    ("", s)
}

/// Locate the byte offset immediately after a statement-level assignment
/// operator at parenthesis depth 0, outside any string literal. Recognises
/// the bare `=` and every augmented form (`+=`, `-=`, `*=`, `/=`, `%=`,
/// `**=`, `//=`, `<<=`, `>>=`, `&=`, `|=`, `^=`, `@=`). Returns `None` for
/// the comparison operators (`==`, `!=`, `<=`, `>=`) and for the walrus
/// `:=` so callers don't mistake them for assignments.
fn find_statement_assignment_end(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    let mut prev_char: Option<char> = None;
    let mut prev_prev_char: Option<char> = None;
    let bytes = s.as_bytes();
    for (i, c) in s.char_indices() {
        if let Some(q) = in_str {
            if c == q {
                in_str = None;
            }
            prev_prev_char = prev_char;
            prev_char = Some(c);
            continue;
        }
        match c {
            '\'' | '"' => in_str = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0 => {
                let next = bytes.get(i + 1).copied();
                if next == Some(b'=') {
                    // `==` — comparison, skip.
                } else if prev_char == Some(':') || prev_char == Some('=') {
                    // `:=` (walrus, not a statement-level assignment) or a
                    // trailing `=` inside a longer `==`-style sequence.
                } else if prev_char == Some('!') {
                    // `!=` — comparison.
                } else if prev_char == Some('<') {
                    if prev_prev_char == Some('<') {
                        // `<<=` — augmented assignment.
                        return Some(i + 1);
                    }
                    // `<=` — comparison.
                } else if prev_char == Some('>') {
                    if prev_prev_char == Some('>') {
                        // `>>=` — augmented assignment.
                        return Some(i + 1);
                    }
                    // `>=` — comparison.
                } else {
                    // Bare `=` or augmented (`+=`, `-=`, `*=`, `**=`, `/=`,
                    // `//=`, `%=`, `&=`, `|=`, `^=`, `@=`). All of these are
                    // valid statement-level assignments.
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        prev_prev_char = prev_char;
        prev_char = Some(c);
    }
    None
}

/// Combine an accumulated LHS expression with the next pipe segment.
///
/// - `acc |> name`            → `name(acc)`
/// - `acc |> name(args)`      → `name(acc, args)`  (or `name(acc)` when empty)
/// - `acc |> module.name(..)` → `module.name(acc, ..)`
///
/// Returns `None` for shapes the rewriter does not understand (e.g. lambda or
/// non-call expressions on the RHS).
fn apply_pipe_call(acc: &str, rhs: &str) -> Option<String> {
    let rhs = rhs.trim();
    if rhs.is_empty() {
        return None;
    }

    // The RHS is either a bare callable (no parens) or a call shape
    // `<callable>(<args>)`. When parens are present we identify the
    // call by walking back from the trailing `)` to its matching
    // `(`, NOT by taking the first `(` — that lets a parenthesised
    // callable like `(lambda x: x * 2)()` be recognised. The
    // `<callable>` text before the matching `(` may itself be:
    //   * a dotted identifier chain (`mod.f.g`),
    //   * a literal-receiver method head (`", ".join`,
    //     `(a + b).method`), or
    //   * a parenthesised expression (`(lambda x: x * 2)`,
    //     `(f if c else g)`) — i.e. anything starting with `(` and
    //     balanced.
    // O28 / FINDINGS #117.
    let bytes = rhs.as_bytes();
    let mut paren_at: Option<usize> = None;
    if rhs.ends_with(')') {
        // Walk back from the trailing `)` to find its match. A forward
        // scan tags every byte as in-string-or-not; the reverse walk
        // then ignores parens that landed inside a string literal.
        // Without this mask, an RHS like `f(")")` would corrupt the
        // depth counter on the embedded `)` and either pick the wrong
        // matching `(` or fail to find one — leaving `|>` in the
        // emitted source for the downstream parser to choke on.
        // Codex / Copilot review on PR #96.
        let in_str_mask = compute_in_string_mask(rhs);
        let mut depth: i32 = 0;
        let mut k = bytes.len();
        while k > 0 {
            k -= 1;
            if in_str_mask[k] {
                continue;
            }
            match bytes[k] {
                b')' => depth += 1,
                b'(' => {
                    depth -= 1;
                    if depth == 0 {
                        paren_at = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    match paren_at {
        None => {
            // Bare callable. Validate it looks like an identifier or dotted
            // chain so we don't generate nonsense.
            if !is_dotted_callable(rhs) {
                return None;
            }
            Some(format!("{}({})", rhs, acc))
        }
        Some(open) => {
            let func = rhs[..open].trim_end();
            if !is_dotted_callable(func)
                && !is_method_call_head(func)
                && !is_parenthesised_expr(func)
            {
                return None;
            }
            // Inner args (with no surrounding parens). The matching
            // walk above guarantees `func(...)` spans to end of `rhs`,
            // so `inner` is everything between the matching parens.
            let inner = rhs[open + 1..rhs.len() - 1].trim();
            if inner.is_empty() {
                Some(format!("{}({})", func, acc))
            } else {
                Some(format!("{}({}, {})", func, acc, inner))
            }
        }
    }
}

/// `true` when `s` is a parenthesised expression — starts with `(`,
/// ends with `)`, and the two are balanced. Used to accept shapes
/// like `(lambda x: x * 2)` and `(f if c else g)` as the head of a
/// pipe RHS so `5 |> (lambda x: x * 2)()` rewrites to
/// `(lambda x: x * 2)(5)`. O28 / FINDINGS #117.
fn is_parenthesised_expr(s: &str) -> bool {
    let s = s.trim();
    if !s.starts_with('(') || !s.ends_with(')') {
        return false;
    }
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(q) = in_str {
            if b == b'\\' && i + 1 < bytes.len() {
                continue;
            }
            if b == q {
                in_str = None;
            }
            continue;
        }
        match b {
            b'"' | b'\'' => in_str = Some(b),
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 && i + 1 != bytes.len() {
                    // Closing before the end means the outer parens
                    // don't span the whole string.
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}

/// True if `s` looks like a *bound method* call head — anything that
/// ends in `.IDENT` where the preceding text is either a string /
/// bytes literal or a parenthesised expression. Examples:
///
///   `", ".join`            → ok (string literal + `.join`)
///   `b"x".decode`          → ok
///   `(a + b).foo`          → ok
///   `mod.func.helper`      → handled by `is_dotted_callable` instead
fn is_method_call_head(s: &str) -> bool {
    let s = s.trim_end();
    let Some(dot_pos) = s.rfind('.') else {
        return false;
    };
    let receiver = s[..dot_pos].trim_end();
    let method = s[dot_pos + 1..].trim_start();
    if receiver.is_empty() || method.is_empty() {
        return false;
    }
    // Method must be a plain identifier.
    let mut chars = method.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    // Receiver: a string/bytes literal (quoted, possibly with a
    // prefix like `b`, `r`, `f`), or a parenthesised expression.
    let bytes = receiver.as_bytes();
    let last = *bytes.last().unwrap();
    matches!(last, b'"' | b'\'' | b')' | b']')
}

/// True if `s` looks like a (possibly dotted) callable identifier suitable as
/// the head of a pipe RHS. Used as a guard so we don't rewrite arbitrary
/// expressions that happen to follow `|>` (e.g. `x |> (a + b)`).
fn is_dotted_callable(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut started_segment = false;
    for c in s.chars() {
        if c == '.' {
            if !started_segment {
                return false;
            }
            started_segment = false;
            continue;
        }
        if started_segment {
            if !(c.is_ascii_alphanumeric() || c == '_') {
                return false;
            }
        } else if !(c.is_ascii_alphabetic() || c == '_') {
            return false;
        } else {
            started_segment = true;
        }
    }
    started_segment
}

/// Scan `line` while updating `in_string` for string-literal state.
/// Returns the byte index where the "code" portion ends — the start of a
/// line comment (`#`) or `line.len()` if there is no comment.
///
/// Single- and double-quoted strings that do not close by the end of the
/// physical line are reset to `None` (Python syntax error territory anyway).
fn scan_line_code_end(line: &str, in_string: &mut Option<StringMode>) -> usize {
    let mut chars = line.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if let Some(mode) = *in_string {
            match mode {
                // Single-line strings: handle backslash escapes and closing quote.
                StringMode::Single | StringMode::Double => {
                    if c == '\\' {
                        chars.next(); // skip escaped character
                        continue;
                    }
                    match mode {
                        StringMode::Single if c == '\'' => *in_string = None,
                        StringMode::Double if c == '"' => *in_string = None,
                        _ => {}
                    }
                }
                // Triple-quoted strings: look for the three-char closing sequence.
                StringMode::TripleSingle | StringMode::TripleDouble => {
                    let (q, triple) = if matches!(mode, StringMode::TripleSingle) {
                        ('\'', "'''")
                    } else {
                        ('"', "\"\"\"")
                    };
                    if c == q && line[i..].starts_with(triple) {
                        chars.next();
                        chars.next();
                        *in_string = None;
                    }
                }
            }
            continue;
        }

        // Outside any string.
        if c == '#' {
            return i; // rest of line is a comment
        }

        if c == '"' || c == '\'' {
            let triple = (c == '"' && line[i..].starts_with("\"\"\""))
                || (c == '\'' && line[i..].starts_with("'''"));
            if triple {
                chars.next();
                chars.next();
                *in_string = Some(if c == '"' {
                    StringMode::TripleDouble
                } else {
                    StringMode::TripleSingle
                });
            } else {
                *in_string = Some(if c == '"' {
                    StringMode::Double
                } else {
                    StringMode::Single
                });
            }
        }
    }

    // A single/double-quoted string that didn't close before end-of-line is
    // a Python syntax error — reset so subsequent lines parse correctly.
    if matches!(*in_string, Some(StringMode::Single | StringMode::Double)) {
        *in_string = None;
    }

    line.len()
}

/// Find the position of the `=` assignment operator in `s`, ignoring `=`
/// characters inside parentheses/brackets/strings, comparison operators
/// (`==`, `!=`, `<=`, `>=`), augmented assignments (`+=`, `-=`, `*=`, `/=`,
/// `%=`, `&=`, `|=`, `^=`, `**=`, `//=`, `<<=`, `>>=`, `@=`), and the walrus
/// operator (`:=`).
fn find_assignment_eq(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str: Option<char> = None;
    let chars = s.char_indices();

    for (i, c) in chars {
        if let Some(q) = in_str {
            if c == q {
                in_str = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => in_str = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0 => {
                let prev = if i > 0 { s[..i].chars().last() } else { None };
                let next = s[i + 1..].chars().next();
                // Reject `==`, `!=`, `<=`, `>=` (comparison; `prev == =` also
                // covers `===` chains although Python rejects those), and
                // augmented/walrus operators where `=` is preceded by one of
                // `+ - * / % & | ^ : @`. (`**=`, `//=`, `<<=`, `>>=` are
                // caught because their last char before `=` is one of those
                // single-char operators already.)
                let is_compound = matches!(
                    prev,
                    Some(
                        '!' | '<'
                            | '>'
                            | '='
                            | '+'
                            | '-'
                            | '*'
                            | '/'
                            | '%'
                            | '&'
                            | '|'
                            | '^'
                            | ':'
                            | '@'
                    )
                );
                if !is_compound && !matches!(next, Some('=')) {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepend_err_alias_dedupes_existing_line() {
        // The body already carries a `from typhon_runtime import Err as
        // __typhon_Err__` line in its header (left there by a prior
        // pipeline pass). Calling the helper again must NOT produce a
        // second identical line — PR #120 caught the duplicate in the
        // emitted Python because the comparison forgot to strip the
        // trailing newline before matching against IMPORT_LINE.
        let body = "from __future__ import annotations\n\
             from typhon_runtime import Err as __typhon_Err__\n\
             import dataclasses\n\
             pass\n";
        let out = prepend_typhon_err_alias_import(body.to_owned());
        assert_eq!(
            out.matches("from typhon_runtime import Err as __typhon_Err__")
                .count(),
            1,
            "duplicate alias import must be deduped:\n{out}"
        );
    }

    #[test]
    fn preserves_let_keyword() {
        // `let` / `mut` are recognised by the Ruff parser directly, so the
        // preprocessor leaves them in place.
        let result = preprocess("let x: int = 1\n");
        assert_eq!(result.python_source, "let x: int = 1\n");
        assert!(result.stripped.is_empty());
    }

    #[test]
    fn preserves_mut_keyword() {
        let result = preprocess("mut count: int = 0\n");
        assert_eq!(result.python_source, "mut count: int = 0\n");
        assert!(result.stripped.is_empty());
    }

    #[test]
    fn preserves_indented_let() {
        let result = preprocess("    let x: int = 1\n");
        assert_eq!(result.python_source, "    let x: int = 1\n");
    }

    #[test]
    fn rewrites_optional_in_annotation() {
        let result = preprocess("let email: str? = None\n");
        assert_eq!(result.python_source, "let email: str | None = None\n");
        assert_eq!(result.optionals.len(), 1);
        // The optional starts at column 14 ("let email: str|").
        assert_eq!(result.optionals[0].python_col, 14);
    }

    #[test]
    fn rewrites_optional_after_subscript() {
        let result = preprocess("x: list[int]? = None\n");
        assert_eq!(result.python_source, "x: list[int] | None = None\n");
        assert_eq!(result.optionals.len(), 1);
    }

    #[test]
    fn does_not_rewrite_question_mark_inside_string() {
        let result = preprocess("let s: str = \"is this ok?\"\n");
        assert_eq!(result.python_source, "let s: str = \"is this ok?\"\n");
        assert!(result.optionals.is_empty());
    }

    #[test]
    fn does_not_rewrite_question_mark_in_triple_quoted_string() {
        let src = "val s: str = \"\"\"line one\nline two with ?\nline three\"\"\"\n";
        let result = preprocess(src);
        // The ? inside the triple-quoted block must be preserved as-is.
        assert!(result.python_source.contains("line two with ?"));
        assert!(result.optionals.is_empty());
    }

    #[test]
    fn handles_non_ascii_source_without_corruption() {
        // Em-dash and accented characters must survive char-aware iteration.
        let src = "# café — entry point\nval name: str? = None\n";
        let result = preprocess(src);
        assert!(result.python_source.contains("café — entry point"));
        assert!(result.python_source.contains("name: str | None"));
    }

    #[test]
    fn does_not_rewrite_question_mark_inside_comment() {
        let result = preprocess("x: int = 1  # really?\n");
        assert_eq!(result.python_source, "x: int = 1  # really?\n");
        assert!(result.optionals.is_empty());
    }

    #[test]
    fn round_trips_optional() {
        let src = "val email: str? = None\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn plain_python_unchanged() {
        let src = "x: int = 1\n";
        let result = preprocess(src);
        assert_eq!(result.python_source, src);
        assert!(result.stripped.is_empty());
        assert!(result.optionals.is_empty());
    }

    // ── model keyword ────────────────────────────────────────────────────────

    #[test]
    fn model_keyword_becomes_basemodel_class() {
        let result = preprocess("model User:\n    id: int\n");
        assert!(
            result.python_source.contains("class User(BaseModel):"),
            "output: {}",
            result.python_source
        );
        assert!(result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::Model)));
    }

    #[test]
    fn model_keyword_round_trips_via_postprocess() {
        let src = "model User:\n    id: int\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    // ── class! (raw class) keyword ──────────────────────────────────────────

    #[test]
    fn raw_class_strips_bang_and_records_line() {
        let result = preprocess("class! MyModel(nn.Module):\n    pass\n");
        assert!(
            result.python_source.contains("class MyModel(nn.Module):"),
            "output: {}",
            result.python_source
        );
        assert!(
            !result.python_source.contains("class!"),
            "bang should be stripped: {}",
            result.python_source
        );
        assert_eq!(result.raw_class_lines, vec![0]);
        assert!(result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::RawClass)));
    }

    #[test]
    fn raw_class_round_trips_via_postprocess() {
        let src = "class! MyModel(nn.Module):\n    pass\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn raw_class_with_no_bases_round_trips() {
        let src = "class! Foo:\n    name: str\n";
        let prep = preprocess(src);
        assert!(prep.python_source.contains("class Foo:"));
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn plain_class_is_not_marked_raw() {
        let result = preprocess("class Foo:\n    pass\n");
        assert!(result.raw_class_lines.is_empty());
        assert!(!result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::RawClass)));
    }

    // ── plain class keyword ─────────────────────────────────────────────────

    #[test]
    fn plain_class_strips_prefix_and_records_line() {
        let result = preprocess("plain class App:\n    pass\n");
        assert!(
            result.python_source.contains("class App:"),
            "output: {}",
            result.python_source
        );
        assert!(
            !result.python_source.contains("plain class"),
            "plain prefix should be stripped: {}",
            result.python_source
        );
        assert_eq!(result.plain_class_lines, vec![0]);
        assert!(result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::PlainClass)));
    }

    #[test]
    fn plain_class_round_trips_via_postprocess() {
        let src = "plain class App:\n    pass\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn plain_class_with_bases() {
        let src = "plain class Widget(textual.App):\n    pass\n";
        let prep = preprocess(src);
        assert!(
            prep.python_source.contains("class Widget(textual.App):"),
            "output: {}",
            prep.python_source
        );
        assert_eq!(prep.plain_class_lines, vec![0]);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn regular_class_is_not_marked_plain() {
        let result = preprocess("class Foo:\n    pass\n");
        assert!(result.plain_class_lines.is_empty());
        assert!(!result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::PlainClass)));
    }

    // ── newtype keyword ─────────────────────────────────────────────────────

    #[test]
    fn newtype_lowers_to_newtype_call() {
        let result = preprocess("newtype UserId = int\n");
        assert_eq!(result.python_source, "UserId = NewType(\"UserId\", int)\n");
        assert!(result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::Newtype)));
    }

    #[test]
    fn newtype_handles_generic_base_type() {
        let result = preprocess("newtype Tags = list[str]\n");
        assert_eq!(
            result.python_source,
            "Tags = NewType(\"Tags\", list[str])\n"
        );
    }

    #[test]
    fn newtype_round_trips_via_postprocess() {
        let src = "newtype UserId = int\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    // ── freeze let (Phase F) ────────────────────────────────────────────

    #[test]
    fn freeze_let_wraps_rhs_in_runtime_call() {
        let result = preprocess("freeze let TAGS = [\"a\", \"b\"]\n");
        assert_eq!(
            result.python_source,
            "let TAGS = __typhon_freeze__([\"a\", \"b\"])\n"
        );
        assert!(result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::Freeze)));
    }

    #[test]
    fn freeze_let_with_annotation_preserves_annotation() {
        let result = preprocess("freeze let CONFIG: dict[str, int] = {\"port\": 8080}\n");
        assert_eq!(
            result.python_source,
            "let CONFIG: dict[str, int] = __typhon_freeze__({\"port\": 8080})\n"
        );
    }

    #[test]
    fn freeze_let_round_trips_via_postprocess() {
        let src = "freeze let TAGS: list[str] = [\"a\", \"b\"]\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn freeze_let_indented_is_left_alone() {
        // `freeze let` is module-level only in v1.
        let result = preprocess("    freeze let X = 1\n");
        assert!(result.python_source.contains("freeze let"));
        assert!(!result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::Freeze)));
    }

    #[test]
    fn freeze_let_preserves_trailing_comment() {
        // PR #95 reviewer feedback: a trailing `# comment` must not be
        // swallowed by the `__typhon_freeze__(...)` call. Verify the
        // wrapper closes BEFORE the comment so the emitted Python is
        // syntactically valid.
        let result = preprocess("freeze let TAGS = [1, 2]  # ids\n");
        assert_eq!(
            result.python_source,
            "let TAGS = __typhon_freeze__([1, 2])  # ids\n"
        );
    }

    #[test]
    fn freeze_let_with_comment_round_trips_via_postprocess() {
        let src = "freeze let TAGS = [1, 2]  # ids\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn typed_let_unpack_rewrites_to_temp_plus_lets() {
        // Regression for N4 (2026-05-22): `let (a: int, b: str) = func()`
        // used to be rejected because the parser doesn't accept typed
        // tuple unpacking patterns. The pre-pass should rewrite it into
        // a single temp + per-element typed lets.
        let src = "def f() -> None:\n    let (a: int, b: str) = pair()\n";
        let out = expand_typed_let_unpack(src);
        assert!(
            out.contains("__typhon_unpack_0__ = pair()"),
            "expected temp + RHS, got:\n{out}"
        );
        assert!(
            out.contains("let a: int = __typhon_unpack_0__[0]"),
            "expected first capture, got:\n{out}"
        );
        assert!(
            out.contains("let b: str = __typhon_unpack_0__[1]"),
            "expected second capture, got:\n{out}"
        );
    }

    #[test]
    fn typed_let_unpack_handles_compound_annotations() {
        // `dict[str, int]` and `tuple[int, ...]` must round-trip
        // intact — the top-level-comma splitter has to ignore commas
        // inside `[]`.
        let src = "def f() -> None:\n    let (xs: list[int], m: dict[str, int]) = build()\n";
        let out = expand_typed_let_unpack(src);
        assert!(
            out.contains("let xs: list[int] = __typhon_unpack_0__[0]"),
            "list[int] annotation got mangled, output:\n{out}"
        );
        assert!(
            out.contains("let m: dict[str, int] = __typhon_unpack_0__[1]"),
            "dict[str, int] annotation got mangled, output:\n{out}"
        );
    }

    #[test]
    fn typed_let_unpack_accepts_mixed_inferred_capture() {
        // One typed + one inferred capture is still rewritten — the
        // inferred slot omits the annotation so the type checker picks
        // it up from the temp's subscript type.
        let src = "def f() -> None:\n    let (a: int, b) = pair()\n";
        let out = expand_typed_let_unpack(src);
        assert!(
            out.contains("let a: int = __typhon_unpack_0__[0]"),
            "got:\n{out}"
        );
        assert!(
            out.contains("let b = __typhon_unpack_0__[1]"),
            "got:\n{out}"
        );
    }

    #[test]
    fn typed_let_unpack_leaves_untyped_destructuring_alone() {
        // Pure untyped destructuring should NOT be intercepted — the
        // existing `let `-stripping path handles `let (a, b) = pair()`
        // directly without a temp.
        let src = "def f() -> None:\n    let (a, b) = pair()\n";
        let out = expand_typed_let_unpack(src);
        assert!(
            !out.contains("__typhon_unpack"),
            "untyped destructuring must not gain a temp: {out}"
        );
        assert!(
            out.contains("let (a, b) = pair()"),
            "original line must be preserved verbatim, got:\n{out}"
        );
    }

    #[test]
    fn questionmark_in_list_comprehension_is_rejected() {
        // Regression for N2 (2026-05-22): the inline `?` lifter used to
        // hoist `parse(s)?` *above* the comprehension's `for s in items`
        // binding, then complained that `s` wasn't in scope. Reject it
        // here with a targeted message instead.
        let src = "def f(xs: list[str]) -> Result[list[int], str]:\n    \
                   let ys: list[int] = [parse(s)? for s in xs]\n    \
                   return Ok(ys)\n";
        let errors = validate_question_ops(src);
        assert_eq!(
            errors.len(),
            1,
            "expected one comprehension error, got {errors:?}"
        );
        assert!(
            errors[0].message.contains("comprehension"),
            "expected comprehension-specific message, got: {}",
            errors[0].message
        );
    }

    #[test]
    fn questionmark_in_dict_comprehension_is_rejected() {
        let src = "def f(xs: list[str]) -> Result[dict[str, int], str]:\n    \
                   let m: dict[str, int] = {s: parse(s)? for s in xs}\n    \
                   return Ok(m)\n";
        let errors = validate_question_ops(src);
        assert!(
            errors.iter().any(|e| e.message.contains("comprehension")),
            "expected comprehension error, got {errors:?}"
        );
    }

    #[test]
    fn questionmark_in_argument_position_still_works() {
        // The argument-position case must NOT be flagged as a
        // comprehension. (Same line shape — `( ... )` brackets — but
        // no `for` keyword inside.)
        let src = "def f(a: str, b: str) -> Result[int, str]:\n    \
                   return Ok(add(parse(a)?, parse(b)?))\n";
        let errors = validate_question_ops(src);
        assert!(
            errors.is_empty(),
            "expected no errors for arg-position `?`, got {errors:?}"
        );
    }

    #[test]
    fn freeze_let_multiline_dict_literal() {
        // Regression for N1 (2026-05-22): a multi-line dict literal on the
        // RHS used to emit `__typhon_freeze__({)` on the open line, breaking
        // Python's parser. The wrap call must stay open until the closing
        // bracket of the RHS expression is reached.
        let src = "freeze let CONFIG = {\n    \"port\": 8080,\n    \"host\": \"a\",\n}\n";
        let out = preprocess(src).python_source;
        // First line opens the call but doesn't close it.
        assert!(
            out.contains("let CONFIG = __typhon_freeze__({\n"),
            "expected open `__typhon_freeze__({{`, got:\n{out}"
        );
        // Closing `)` lands on the line that closes the literal.
        assert!(
            out.contains("})\n") || out.contains("})\r\n"),
            "expected closing brace+paren after the literal, got:\n{out}"
        );
        // Original `{}` body lines are preserved verbatim so f-strings and
        // other literal payloads round-trip unchanged.
        assert!(out.contains("    \"port\": 8080,\n"), "got:\n{out}");
    }

    #[test]
    fn freeze_let_multiline_list_literal() {
        let src = "freeze let TAGS = [\n    \"a\",\n    \"b\",\n]\n";
        let out = preprocess(src).python_source;
        assert!(
            out.contains("let TAGS = __typhon_freeze__([\n"),
            "expected open `__typhon_freeze__([`, got:\n{out}"
        );
        assert!(
            out.contains("])\n") || out.contains("])\r\n"),
            "expected closing `])` after the literal, got:\n{out}"
        );
    }

    #[test]
    fn freeze_let_does_not_match_augmented_assign() {
        // PR #95 reviewer feedback: `%=`, `&=`, `|=`, `^=`, `@=`
        // augmented-assignment operators must not be mistaken for the
        // bare `=` that anchors the RHS. None of these are valid
        // module-level statements after `freeze let`, but the helper
        // still has to fall through cleanly rather than wrap the LHS.
        // (We exercise the helper directly to keep the test focused.)
        assert_eq!(wrap_freeze_let("X %= 1"), None);
        assert_eq!(wrap_freeze_let("X &= 1"), None);
        assert_eq!(wrap_freeze_let("X |= 1"), None);
        assert_eq!(wrap_freeze_let("X ^= 1"), None);
        assert_eq!(wrap_freeze_let("X @= 1"), None);
    }

    // ── pub keyword (Phase D) ───────────────────────────────────────────

    #[test]
    fn pub_def_records_name_and_strips_keyword() {
        let result = preprocess("pub def greet(name: str) -> str:\n    return name\n");
        assert_eq!(result.pub_names, vec!["greet".to_owned()]);
        assert!(!result.python_source.contains("pub def"));
        assert!(result.python_source.contains("def greet"));
    }

    #[test]
    fn pub_class_records_name() {
        let result = preprocess("pub class User:\n    name: str\n");
        assert_eq!(result.pub_names, vec!["User".to_owned()]);
    }

    #[test]
    fn pub_let_records_name() {
        let result = preprocess("pub let API: str = \"v1\"\n");
        assert_eq!(result.pub_names, vec!["API".to_owned()]);
    }

    #[test]
    fn pub_newtype_records_name() {
        let result = preprocess("pub newtype UserId = int\n");
        assert_eq!(result.pub_names, vec!["UserId".to_owned()]);
    }

    #[test]
    fn pub_round_trips_via_postprocess() {
        let src = "pub def greet(name: str) -> str:\n    return name\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn pub_let_round_trips_via_postprocess() {
        let src = "pub let API: str = \"v1\"\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn indented_pub_is_left_alone() {
        // `pub` only applies at module scope. An indented occurrence
        // is treated as a regular identifier (which the type checker
        // will then reject), not as a visibility modifier.
        let result = preprocess("    pub = 1\n");
        assert!(result.pub_names.is_empty());
        assert!(result.python_source.contains("pub = 1"));
    }

    #[test]
    fn pub_with_async_def_records_name() {
        let result = preprocess("pub async def fetch() -> int:\n    return 0\n");
        assert_eq!(result.pub_names, vec!["fetch".to_owned()]);
    }

    #[test]
    fn pub_star_records_line_and_strips_text() {
        // `pub *` is the wildcard re-export marker. The preprocessor
        // must record its line index and leave a blank line behind so
        // line numbers stay aligned for downstream source maps.
        let src = "pub *\npub def greet() -> str:\n    return \"hi\"\n";
        let result = preprocess(src);
        assert_eq!(result.pub_star_lines, vec![0]);
        // The first line should be blank (the marker was stripped).
        let first_line = result
            .python_source
            .lines()
            .next()
            .unwrap_or("");
        assert!(
            first_line.trim().is_empty(),
            "pub * line should be blanked; got {:?}",
            first_line
        );
    }

    #[test]
    fn pub_star_with_trailing_comment_is_recognised() {
        let src = "pub *  # re-export everything\n";
        let result = preprocess(src);
        assert_eq!(result.pub_star_lines, vec![0]);
    }

    #[test]
    fn pub_star_with_extra_text_is_not_recognised() {
        // `pub * from foo` (or any other operand after `*`) is NOT a
        // wildcard marker — keep the existing `pub` machinery unhappy
        // about the form so the user gets a real parse error rather
        // than a silent no-op.
        let src = "pub * from foo\n";
        let result = preprocess(src);
        assert!(
            result.pub_star_lines.is_empty(),
            "`pub * from foo` should not be treated as `pub *`"
        );
    }

    #[test]
    fn indented_pub_star_is_left_alone() {
        // `pub *` only applies at module level; an indented occurrence
        // is not a re-export marker.
        let src = "def f() -> None:\n    pub *\n";
        let result = preprocess(src);
        assert!(result.pub_star_lines.is_empty());
    }

    #[test]
    fn newtype_does_not_match_indented_form() {
        // `newtype` only applies at module level. An indented occurrence
        // should be passed through verbatim (the Python parser will reject
        // it, which is the right behaviour: nominal aliases inside a
        // function body don't compose with type checking).
        let result = preprocess("    newtype UserId = int\n");
        assert!(result.python_source.contains("newtype UserId"));
        assert!(!result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::Newtype)));
    }

    #[test]
    fn indented_model_class_preserved() {
        let src = "    model Inner:\n        x: int\n";
        let result = preprocess(src);
        assert!(
            result.python_source.contains("class Inner(BaseModel):"),
            "output: {}",
            result.python_source
        );
    }

    // ── comptime keyword ────────────────────────────────────────────────────

    #[test]
    fn comptime_let_stripped_to_let_assignment() {
        // Only the `comptime` prefix is stripped — the inner `let` is left
        // for the Ruff parser to consume natively.
        let result = preprocess("comptime let PORT: int = 8080\n");
        assert_eq!(result.python_source, "let PORT: int = 8080\n");
        assert_eq!(result.comptime_bindings.len(), 1);
        assert_eq!(result.comptime_bindings[0].name, "PORT");
    }

    #[test]
    fn comptime_mut_stripped_correctly() {
        let result = preprocess("comptime mut DB_URL: str = \"postgres://localhost\"\n");
        assert_eq!(
            result.python_source,
            "mut DB_URL: str = \"postgres://localhost\"\n"
        );
        assert_eq!(result.comptime_bindings[0].name, "DB_URL");
    }

    #[test]
    fn comptime_let_round_trips_via_postprocess() {
        let src = "comptime let PORT: int = 8080\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    // ── ? operator expansion ────────────────────────────────────────────────

    #[test]
    fn question_op_expands_assignment() {
        let src = "val x = f()?\n";
        let out = expand_question_ops(src);
        assert!(out.contains("__typhon_q_0__ = f()"), "out: {out}");
        assert!(
            out.contains("if isinstance(__typhon_q_0__, __typhon_Err__):"),
            "out: {out}"
        );
        assert!(out.contains("return __typhon_q_0__"), "out: {out}");
        assert!(out.contains("val x = __typhon_q_0__.value"), "out: {out}");
    }

    #[test]
    fn question_op_expands_bare_call() {
        let src = "save(record)?\n";
        let out = expand_question_ops(src);
        assert!(out.contains("__typhon_q_0__ = save(record)"), "out: {out}");
        assert!(
            out.contains("if isinstance(__typhon_q_0__, __typhon_Err__):"),
            "out: {out}"
        );
        // No binding assignment for a bare expression.
        assert!(!out.contains(".value"), "out: {out}");
    }

    #[test]
    fn type_annotation_nullable_not_treated_as_op() {
        // `str?` ends with `?` but last char before `?` is `r` (alphanumeric),
        // so it should NOT be treated as the propagation operator.
        let src = "val x: str? = None\n";
        let out = expand_question_ops(src);
        assert_eq!(out, src, "type sugar must be preserved unchanged");
    }

    #[test]
    fn question_op_preserves_indent() {
        let src = "    let y = compute()?\n";
        let out = expand_question_ops(src);
        assert!(out.contains("    __typhon_q_0__ = compute()"), "out: {out}");
        assert!(out.contains("    if isinstance"), "out: {out}");
        assert!(
            out.contains("    let y = __typhon_q_0__.value"),
            "out: {out}"
        );
    }

    #[test]
    fn question_op_preserves_lhs_with_type_annotation() {
        let src = "val result: int = compute()?\n";
        let out = expand_question_ops(src);
        assert!(
            out.contains("val result: int = __typhon_q_0__.value"),
            "out: {out}"
        );
    }

    #[test]
    fn question_op_ignores_comment_line() {
        // A comment that mentions `)?` must not trigger expansion.
        let src = "# should we call f()?\nx: int = 1\n";
        let out = expand_question_ops(src);
        assert_eq!(out, src, "comment line must be copied verbatim");
    }

    #[test]
    fn question_op_ignores_trailing_comment() {
        // Trailing comment after `)` must not be seen as `)?`.
        let src = "x = f() # returns f()?\n";
        let out = expand_question_ops(src);
        assert_eq!(
            out, src,
            "trailing comment with ')?' must not trigger expansion"
        );
    }

    #[test]
    fn question_op_ignores_triple_string_content() {
        // `)?` inside a triple-quoted string must not be expanded.
        let src = "msg = \"\"\"\ncall f()?\n\"\"\"\n";
        let out = expand_question_ops(src);
        assert_eq!(out, src, "triple-string content must not be expanded");
    }

    #[test]
    fn question_op_preserves_value_position_bare_identifier() {
        // `let x: T = a?` is value-position propagation: the trailing `?`
        // applies to the value `a` and must lower to the standard
        // `__typhon_q_N__` ladder. The old behaviour silently rewrote it
        // to `x: T = a | None` via the nullable-type pass, which crashed
        // at runtime with `TypeError`.
        let src = "    let av: int = a?\n";
        let out = expand_question_ops(src);
        assert!(out.contains("__typhon_q_0__ = a"), "out: {out}");
        assert!(
            out.contains("if isinstance(__typhon_q_0__, __typhon_Err__):"),
            "out: {out}"
        );
        assert!(
            out.contains("let av: int = __typhon_q_0__.value"),
            "out: {out}"
        );
    }

    #[test]
    fn question_op_preserves_type_alias_nullable() {
        // `type X = T?` is a type alias; the trailing `?` is nullable
        // type sugar, not the propagation operator. Must not be rewritten
        // into a `__typhon_q_N__` ladder.
        let src = "type Maybe = int?\n";
        let out = expand_question_ops(src);
        assert_eq!(out, src, "type alias must be copied verbatim");
    }

    #[test]
    fn question_op_preserves_generic_type_alias_nullable() {
        // `type X[T] = T?` is also a type alias even though the RHS is a
        // bare identifier.
        let src = "type Maybe[T] = T?\n";
        let out = expand_question_ops(src);
        assert_eq!(out, src, "generic type alias must be copied verbatim");
    }

    #[test]
    fn question_op_preserves_newtype_nullable() {
        // `newtype X = T?` is a nominal alias; the trailing `?` is
        // nullable type sugar and the RHS is a type expression.
        let src = "newtype MyOpt = int?\n";
        let out = expand_question_ops(src);
        assert_eq!(out, src, "newtype declaration must be copied verbatim");
    }

    // ── inline `?` propagation in sub-expressions (O17) ─────────────────────

    #[test]
    fn inline_question_op_lifts_inner_call_in_ok_wrapper() {
        // The motivating O17 case: `Ok(f(x)?)`. The inner `?` is inside
        // a sub-expression so the end-of-line rewriter doesn't pick it
        // up — the inline pass must lift it to a temp first.
        let src = "    return Ok(parse(s)?)\n";
        let out = expand_inline_question_ops(src);
        assert!(out.contains("__typhon_qi_0__ = parse(s)"), "out: {out}");
        assert!(
            out.contains("if isinstance(__typhon_qi_0__, __typhon_Err__):"),
            "out: {out}"
        );
        assert!(out.contains("return __typhon_qi_0__"), "out: {out}");
        assert!(
            out.contains("return Ok(__typhon_qi_0__.value)"),
            "out: {out}"
        );
        // The auto-injected alias import must appear at module top.
        assert!(
            out.starts_with("from typhon_runtime import Err as __typhon_Err__\n"),
            "out: {out}"
        );
    }

    #[test]
    fn inline_question_op_handles_multiple_inner_calls() {
        // Two `?` ops on the same line — both get lifted, in order,
        // and the final expression substitutes both temps.
        let src = "    return Ok(add(parse(s)?, parse(t)?))\n";
        let out = expand_inline_question_ops(src);
        assert!(out.contains("__typhon_qi_0__ = parse(s)"), "out: {out}");
        assert!(out.contains("__typhon_qi_1__ = parse(t)"), "out: {out}");
        assert!(
            out.contains("return Ok(add(__typhon_qi_0__.value, __typhon_qi_1__.value))"),
            "out: {out}"
        );
    }

    #[test]
    fn inline_question_op_preserves_end_of_line_case() {
        // `let x = f()?` (top-of-statement position) is NOT touched by
        // the inline pass — the existing end-of-line rewriter owns it.
        let src = "    let x = f()?\n";
        let out = expand_inline_question_ops(src);
        assert_eq!(
            out, src,
            "end-of-line `?` must be left for `expand_question_ops`"
        );
    }

    #[test]
    fn inline_question_op_handles_dotted_callable() {
        // The callable receiver is a dotted name (`mod.f`). The lifted
        // expression must include the dotted prefix, not just the
        // final identifier.
        let src = "    return Ok(mod.f(x)?)\n";
        let out = expand_inline_question_ops(src);
        assert!(out.contains("__typhon_qi_0__ = mod.f(x)"), "out: {out}");
        assert!(
            out.contains("return Ok(__typhon_qi_0__.value)"),
            "out: {out}"
        );
    }

    #[test]
    fn inline_question_op_preserves_indent_inside_block() {
        // Lifted temps must inherit the line's leading indent so they
        // remain inside the enclosing block.
        let src = "    if cond:\n        return Ok(parse(s)?)\n";
        let out = expand_inline_question_ops(src);
        assert!(
            out.contains("        __typhon_qi_0__ = parse(s)"),
            "out: {out}"
        );
        assert!(
            out.contains("        return Ok(__typhon_qi_0__.value)"),
            "out: {out}"
        );
    }

    #[test]
    fn inline_question_op_ignores_string_contents() {
        // A `)?` inside a string literal must not trigger expansion.
        let src = "    log(\"missing f()?\")\n";
        let out = expand_inline_question_ops(src);
        assert_eq!(out, src, "string content must not be expanded");
    }

    #[test]
    fn inline_question_op_ignores_triple_quoted_string_contents() {
        // PR #96 review: a `)?` inside a triple-quoted string on a
        // single line must not trigger expansion either. The
        // string-state tracker previously only handled single-char
        // quotes and would toggle off the in-string state on the
        // second `"` of `"""`, leaving the `?` exposed.
        let src = "    log(\"\"\"missing f()?\"\"\")\n";
        let out = expand_inline_question_ops(src);
        assert_eq!(
            out, src,
            "triple-quoted string content must not be expanded"
        );
    }

    #[test]
    fn inline_then_outer_question_op_compose() {
        // Pipeline-shaped: `let x = Ok(f()?)?` — the inner `?` is
        // lifted by the inline pass, the outer `?` is then a normal
        // end-of-line case the standard pass handles.
        let src = "    let x = Ok(f()?)?\n";
        let after_inline = expand_inline_question_ops(src);
        assert!(
            after_inline.contains("__typhon_qi_0__ = f()"),
            "out: {after_inline}"
        );
        assert!(
            after_inline.contains("let x = Ok(__typhon_qi_0__.value)?"),
            "out: {after_inline}"
        );
        // Now the standard pass picks up the remaining trailing `?`.
        let out = expand_question_ops(&after_inline);
        assert!(
            out.contains("__typhon_q_0__ = Ok(__typhon_qi_0__.value)"),
            "out: {out}"
        );
        assert!(out.contains("let x = __typhon_q_0__.value"), "out: {out}");
    }

    #[test]
    fn model_keyword_with_existing_base_merges_basemodel() {
        // `model User(Timestamped):` → `class User(Timestamped, BaseModel):`
        let result = preprocess("model User(Timestamped):\n    id: int\n");
        assert!(
            result
                .python_source
                .contains("class User(Timestamped, BaseModel):"),
            "got: {}",
            result.python_source
        );
    }

    #[test]
    fn model_keyword_underscore_name() {
        // `model _Internal:` should be rewritten (underscore-starting names are valid).
        let result = preprocess("model _Internal:\n    x: int\n");
        assert!(
            result.python_source.contains("class _Internal(BaseModel):"),
            "got: {}",
            result.python_source
        );
    }

    // ── impl keyword ─────────────────────────────────────────────────────────

    #[test]
    fn impl_keyword_becomes_typhon_impl_class() {
        let result = preprocess("impl User:\n    def greet():\n        pass\n");
        assert!(
            result
                .python_source
                .contains("class __typhon_impl_User(object):"),
            "output: {}",
            result.python_source
        );
        assert!(result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::Impl)));
    }

    #[test]
    fn impl_keyword_round_trips_via_postprocess() {
        let src = "impl User:\n    def greet():\n        pass\n";
        let prep = preprocess(src);
        let out = postprocess(&prep.python_source, &prep.stripped, &prep.optionals);
        assert_eq!(out, src);
    }

    #[test]
    fn indented_impl_preserved() {
        let src = "    impl Inner:\n        def method():\n            pass\n";
        let result = preprocess(src);
        assert!(
            result
                .python_source
                .contains("class __typhon_impl_Inner(object):"),
            "output: {}",
            result.python_source
        );
    }

    #[test]
    fn impl_underscore_name() {
        let result = preprocess("impl _Private:\n    def method():\n        pass\n");
        assert!(
            result
                .python_source
                .contains("class __typhon_impl__Private(object):"),
            "output: {}",
            result.python_source
        );
    }

    #[test]
    fn comptime_inside_function_not_recorded() {
        // `comptime val` inside a function should NOT be recorded as a comptime
        // binding (it would fail evaluation anyway since it's not top-level).
        let src = "def f():\n    comptime val X: int = 1\n";
        let result = preprocess(src);
        assert!(
            result.comptime_bindings.is_empty(),
            "indented comptime should not be recorded: {:?}",
            result.comptime_bindings
        );
    }

    // ── validate_question_ops ────────────────────────────────────────────────

    #[test]
    fn question_op_valid_in_result_function() {
        let src = "def parse(s: str) -> Result[int, str]:\n    val n = int(s)?\n    return Ok(n)\n";
        let errs = validate_question_ops(src);
        assert!(
            errs.is_empty(),
            "expected no errors, got: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn question_op_at_module_level_is_error() {
        let src = "val x = parse()?\n";
        let errs = validate_question_ops(src);
        assert_eq!(errs.len(), 1);
        assert!(
            errs[0].message.contains("module level"),
            "got: {}",
            errs[0].message
        );
        assert_eq!(errs[0].line_index, 0);
    }

    #[test]
    fn question_op_in_none_returning_function_is_error() {
        let src = "def process() -> None:\n    val x = load()?\n";
        let errs = validate_question_ops(src);
        assert_eq!(
            errs.len(),
            1,
            "expected one error, got: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        assert!(errs[0].message.contains("None"), "got: {}", errs[0].message);
    }

    #[test]
    fn question_op_in_int_returning_function_is_error() {
        let src = "def compute() -> int:\n    val x = fetch()?\n    return x\n";
        let errs = validate_question_ops(src);
        assert_eq!(errs.len(), 1);
        assert!(
            errs[0].message.contains("`int`"),
            "got: {}",
            errs[0].message
        );
    }

    #[test]
    fn question_op_in_nested_result_function_is_valid() {
        let src = "def outer() -> None:\n    def inner() -> Result[int, str]:\n        val x = load()?\n        return Ok(x)\n";
        let errs = validate_question_ops(src);
        assert!(
            errs.is_empty(),
            "expected no errors, got: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn question_op_in_multiline_sig_skips_validation() {
        // Multi-line signature: `->` appears on the `) -> RetType:` closing
        // line rather than the `def` line.  The validator picks up the return
        // type from the signature-closer and correctly accepts the `?` use.
        let src = "def process(\n    x: int,\n) -> Result[int, str]:\n    val y = load()?\n";
        let errs = validate_question_ops(src);
        assert!(
            errs.is_empty(),
            "multi-line sig with Result return must not error: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn question_op_in_pub_def_result_function_is_valid() {
        // R2-1: `pub def f() -> Result[T, E]: ... x?` must not be flagged
        // as "? operator used at module level". The `pub` modifier stacks
        // with `def` and the validator must see through it.
        let src =
            "pub def parse(s: str) -> Result[int, str]:\n    val n = int(s)?\n    return Ok(n)\n";
        let errs = validate_question_ops(src);
        assert!(
            errs.is_empty(),
            "pub def with Result return must accept `?`: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn question_op_in_pub_async_def_result_function_is_valid() {
        // R2-1: same fix must apply to `pub async def`.
        let src =
            "pub async def fetch() -> Result[int, str]:\n    val n = io()?\n    return Ok(n)\n";
        let errs = validate_question_ops(src);
        assert!(
            errs.is_empty(),
            "pub async def with Result return must accept `?`: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn question_op_in_pub_def_none_function_is_error() {
        // R2-1: the `pub` strip must not also lose the return-type check.
        let src = "pub def run() -> None:\n    val x = load()?\n";
        let errs = validate_question_ops(src);
        assert_eq!(
            errs.len(),
            1,
            "pub def returning None must still reject `?`: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn question_op_nullable_sugar_not_flagged() {
        // `str?` ends with `?` but the char before is `r`, not `)`, so it is
        // type-sugar, not the propagation operator.
        let src = "val x: str? = None\n";
        let errs = validate_question_ops(src);
        assert!(errs.is_empty(), "nullable sugar must not be flagged");
    }

    #[test]
    fn question_op_pub_def_tolerates_double_space() {
        // PR #129 gemini review: the `pub` strip uses
        // `strip_prefix("pub ").unwrap_or(trimmed).trim_start()` so the
        // detection is robust to `pub  def` (double space). Without
        // `.trim_start()` the second space would be consumed as part of
        // the keyword check (`"def "` vs `" def "`) and the function
        // would not register on `fn_stack`.
        let src =
            "pub  def parse(s: str) -> Result[int, str]:\n    val n = int(s)?\n    return Ok(n)\n";
        let errs = validate_question_ops(src);
        assert!(
            errs.is_empty(),
            "pub def with extra whitespace must accept `?`: {:?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn extract_return_type_finds_result() {
        assert_eq!(
            extract_return_type_text("def f() -> Result[int, str]:"),
            Some("Result[int, str]".to_owned())
        );
    }

    #[test]
    fn extract_return_type_finds_none() {
        assert_eq!(
            extract_return_type_text("def f() -> None:"),
            Some("None".to_owned())
        );
    }

    #[test]
    fn extract_return_type_multiline_sig() {
        // `->` not present on this line — returns None.
        assert_eq!(extract_return_type_text("def f("), None);
    }

    #[test]
    fn extract_return_type_ignores_arrow_in_string() {
        // `->` inside a default-value string must not fool the scanner.
        assert_eq!(
            extract_return_type_text("def f(x: str = \"->\") -> Result[int, str]:"),
            Some("Result[int, str]".to_owned())
        );
    }

    #[test]
    fn extract_return_type_ignores_colon_in_string() {
        assert_eq!(
            extract_return_type_text("def f(x: str = \"a:b\") -> None:"),
            Some("None".to_owned())
        );
    }

    #[test]
    fn is_result_type_accepts_bare_result() {
        assert!(is_result_type("Result"));
    }

    #[test]
    fn is_result_type_accepts_subscripted_result() {
        assert!(is_result_type("Result[int, str]"));
    }

    #[test]
    fn is_result_type_rejects_substring() {
        assert!(!is_result_type("MyResult"));
        assert!(!is_result_type("ResultWrapper[int]"));
        assert!(!is_result_type("NotAResult"));
    }

    #[test]
    fn question_op_my_result_type_is_invalid_context() {
        // `MyResult` must not be accepted as a valid Result context.
        let src = "def f() -> MyResult:\n    val x = load()?\n";
        let errs = validate_question_ops(src);
        assert_eq!(errs.len(), 1, "MyResult must not pass as Result context");
        assert!(
            errs[0].message.contains("MyResult"),
            "got: {}",
            errs[0].message
        );
    }

    #[test]
    fn question_op_error_carries_byte_offset() {
        // "val x = load()?" — v(0)a(1)l(2) (3)x(4) (5)=(6) (7)l(8)o(9)a(10)d(11)((12))(13)?(14)
        let src = "val x = load()?\n";
        let errs = validate_question_ops(src);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].offset, 14, "offset should point at the `?`");
    }

    // ── pipe operator expansion ─────────────────────────────────────────────

    #[test]
    fn pipe_bare_callable_wraps_lhs() {
        let out = expand_pipes("y = x |> f\n");
        assert_eq!(out, "y = f(x)\n");
    }

    #[test]
    fn pipe_callable_with_args_prepends_lhs() {
        let out = expand_pipes("y = x |> f(a, b)\n");
        assert_eq!(out, "y = f(x, a, b)\n");
    }

    #[test]
    fn pipe_callable_empty_args_just_passes_lhs() {
        let out = expand_pipes("y = x |> f()\n");
        assert_eq!(out, "y = f(x)\n");
    }

    #[test]
    fn pipe_chains_left_to_right() {
        let out = expand_pipes("z = x |> f |> g\n");
        assert_eq!(out, "z = g(f(x))\n");
    }

    #[test]
    fn pipe_chain_with_mixed_callables() {
        let out = expand_pipes("z = x |> f(1) |> g\n");
        assert_eq!(out, "z = g(f(x, 1))\n");
    }

    #[test]
    fn pipe_return_statement() {
        let out = expand_pipes("return data |> transform |> filter\n");
        assert_eq!(out, "return filter(transform(data))\n");
    }

    #[test]
    fn pipe_expression_statement() {
        let out = expand_pipes("data |> sink\n");
        assert_eq!(out, "sink(data)\n");
    }

    #[test]
    fn pipe_with_dotted_callable() {
        let out = expand_pipes("y = x |> mod.helper(2)\n");
        assert_eq!(out, "y = mod.helper(x, 2)\n");
    }

    #[test]
    fn pipe_preserves_typhon_keyword_prefix() {
        // The `val` keyword survives the rewrite because `expand_pipes` only
        // touches the right-hand side of the assignment.
        let out = expand_pipes("val y = x |> f\n");
        assert_eq!(out, "val y = f(x)\n");
    }

    #[test]
    fn pipe_preserves_type_annotation() {
        let out = expand_pipes("y: int = x |> f\n");
        assert_eq!(out, "y: int = f(x)\n");
    }

    #[test]
    fn pipe_inside_string_is_left_alone() {
        let src = "msg = \"a |> b not a pipe\"\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    #[test]
    fn pipe_inside_comment_is_left_alone() {
        let src = "y = f(x)  # consider x |> f\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    #[test]
    fn pipe_preserves_indent() {
        let out = expand_pipes("    return x |> f\n");
        assert_eq!(
            out,
            "    return filter_unused\n".replace("filter_unused", "f(x)")
        );
    }

    #[test]
    fn pipe_with_nested_call_in_rhs_args() {
        let out = expand_pipes("y = x |> f(g(1, 2), 3)\n");
        assert_eq!(out, "y = f(x, g(1, 2), 3)\n");
    }

    #[test]
    fn pipe_inside_parens_is_left_alone() {
        // Pipes inside `[...]` brackets (list/set/dict comprehensions or
        // index syntax) are NOT rewritten — Python's `|` already has a
        // meaning in those positions and we leave the parser to surface
        // the error. The matching recursion in `expand_pipes_in_sub-
        // expressions` only walks `(...)` groups.
        let src = "y = sum([a |> f for a in xs])\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    #[test]
    fn pipe_inside_round_parens_now_expanded() {
        // O28 / FINDINGS #119: pipes inside a `(...)` sub-expression are
        // expanded by the nested-paren pre-pass before the outer pipe
        // pass runs. `(1 |> add(2)) |> add(3)` rewrites end-to-end.
        let src = "y = (1 |> add(2)) |> add(3)\n";
        let out = expand_pipes(src);
        assert_eq!(out, "y = add((add(1, 2)), 3)\n");
    }

    #[test]
    fn pipe_into_parenthesised_lambda_call() {
        // O28 / FINDINGS #117: a `(lambda x: x * 2)()` RHS — i.e. a
        // parenthesised expression followed by an empty call — is now
        // accepted as a pipe callable head.
        let src = "y = 5 |> (lambda x: x * 2)()\n";
        let out = expand_pipes(src);
        assert_eq!(out, "y = (lambda x: x * 2)(5)\n");
    }

    #[test]
    fn pipe_into_parenthesised_lambda_call_with_extra_args() {
        // The lambda call may already carry extra positional args; the
        // pipe value goes first.
        let src = "y = 5 |> (lambda x, k: x * k)(2)\n";
        let out = expand_pipes(src);
        assert_eq!(out, "y = (lambda x, k: x * k)(5, 2)\n");
    }

    #[test]
    fn pipe_rhs_with_paren_inside_string_arg() {
        // PR #96 review: the reverse paren-walk in `apply_pipe_call`
        // must skip parens that live inside a string literal. Without
        // a string-aware mask, the `)` inside `f(")")` corrupts the
        // depth counter and the rewriter either picks the wrong `(`
        // or fails entirely, leaving `|>` in the source.
        let src = "y = 5 |> f(\")\")\n";
        let out = expand_pipes(src);
        assert_eq!(out, "y = f(5, \")\")\n");
    }

    #[test]
    fn pipe_rhs_with_open_paren_inside_string_arg() {
        // Mirror of the above for `(` inside a string literal.
        let src = "y = 5 |> f(\"(\")\n";
        let out = expand_pipes(src);
        assert_eq!(out, "y = f(5, \"(\")\n");
    }

    #[test]
    fn pipe_non_callable_rhs_passes_through() {
        // `(a + b)` is not a dotted callable; the rewriter leaves the line
        // alone so the user gets a proper syntax error rather than nonsense.
        let src = "y = x |> (a + b)\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    #[test]
    fn pipe_no_op_when_no_pipe_present() {
        let src = "y = f(x)\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    // ── with-chain expansion ─────────────────────────────────────────────────

    #[test]
    fn with_chain_single_binding_with_else() {
        let src = "\
def run() -> Result[str, str]:
    with x = f()?:
        return Ok(x)
    else err:
        return Err(err)
";
        let out = expand_with_chains(src);
        assert!(out.contains("__typhon_with_0__ = f()"), "out:\n{out}");
        assert!(
            out.contains("if isinstance(__typhon_with_0__, __typhon_Err__):"),
            "out:\n{out}"
        );
        // The user-visible `err` name is uniquified to
        // `__typhon_with_err_N__` to avoid `let err` clashing across
        // multiple `with`-chains in the same function. References to
        // `err` in the else_body are renamed to match.
        assert!(
            out.contains("let __typhon_with_err_0__ = __typhon_with_0__.error"),
            "out:\n{out}"
        );
        assert!(
            out.contains("return Err(__typhon_with_err_0__)"),
            "out:\n{out}"
        );
        assert!(
            out.contains("let x = __typhon_with_0__.value"),
            "out:\n{out}"
        );
        assert!(out.contains("return Ok(x)"), "out:\n{out}");
    }

    #[test]
    fn with_chain_multi_binding_threads_temporaries() {
        let src = "\
def run() -> Result[str, str]:
    with a = f()?,
         b = g(a)?:
        return Ok(b)
    else err:
        return Err(err)
";
        let out = expand_with_chains(src);
        assert!(out.contains("__typhon_with_0__ = f()"), "out:\n{out}");
        assert!(
            out.contains("let a = __typhon_with_0__.value"),
            "out:\n{out}"
        );
        assert!(out.contains("__typhon_with_1__ = g(a)"), "out:\n{out}");
        assert!(
            out.contains("let b = __typhon_with_1__.value"),
            "out:\n{out}"
        );
    }

    #[test]
    fn with_chain_without_else_falls_back_to_propagation() {
        let src = "\
def run() -> Result[str, str]:
    with x = f()?:
        return Ok(x)
";
        let out = expand_with_chains(src);
        assert!(
            out.contains("return __typhon_with_0__"),
            "no `else` should propagate the raw Err: {out}"
        );
        assert!(
            !out.contains("__typhon_with_0__.error"),
            "unexpected error binding: {out}"
        );
    }

    #[test]
    fn with_chain_bare_else_defaults_err_var() {
        // `else:` without a binding name still allows custom error handling;
        // the desugarer uses the reserved `_err` identifier so the body can
        // reference it.
        let src = "\
def run() -> Result[str, str]:
    with x = f()?:
        return Ok(x)
    else:
        return Err(\"oops\")
";
        let out = expand_with_chains(src);
        // Uniquified per the multi-chain fix; original name `_err`
        // becomes `__typhon_with_err_N__` keyed off the binding's tmp.
        assert!(
            out.contains("let __typhon_with_err_0__ = __typhon_with_0__.error"),
            "out:\n{out}"
        );
        assert!(out.contains("return Err(\"oops\")"), "out:\n{out}");
    }

    #[test]
    fn with_chain_preserves_following_statements() {
        // Anything outside the chain's indentation block must be passed
        // through verbatim so unrelated code is unaffected.
        let src = "\
def run() -> Result[str, str]:
    with x = f()?:
        return Ok(x)
    else err:
        return Err(err)

def other() -> int:
    return 1
";
        let out = expand_with_chains(src);
        assert!(out.contains("def other() -> int:"), "out:\n{out}");
        assert!(out.contains("    return 1"), "out:\n{out}");
    }

    #[test]
    fn pipe_handles_walrus_without_corruption() {
        // The walrus operator `:=` contains `=` and must not be mistaken for
        // an assignment when splitting the chain prefix.
        let src = "y = (z := x) |> f\n";
        let out = expand_pipes(src);
        // No pipe inside parens; the top-level pipe is the only one at depth 0.
        assert_eq!(out, "y = f((z := x))\n");
    }

    #[test]
    fn pipe_inside_triple_quoted_string_left_alone() {
        // A `|>` inside a triple-quoted string must not be treated as a pipe
        // — the apostrophe inside the contents previously confused the
        // single-char string tracker. Ensure the line round-trips unchanged.
        let src = "msg = \"\"\"don't use a |> b here\"\"\"\n";
        let out = expand_pipes(src);
        assert_eq!(out, src);
    }

    #[test]
    fn pipe_after_closing_triple_quote_is_recognised() {
        // A real `|>` *after* a closing `"""` should still be rewritten.
        let src = "y = \"\"\"x\"\"\" |> str.strip\n";
        let out = expand_pipes(src);
        assert_eq!(out, "y = str.strip(\"\"\"x\"\"\")\n");
    }

    #[test]
    fn pipe_preserves_crlf_line_endings() {
        // Windows-authored files use `\r\n`; the rewriter must not silently
        // convert them to `\n`.
        let src = "y = x |> f\r\n";
        let out = expand_pipes(src);
        assert_eq!(out, "y = f(x)\r\n");
    }

    #[test]
    fn find_assignment_eq_rejects_augmented_ops() {
        // `+=`, `*=`, `:=`, `//=`, `**=`, `<<=`, `>>=`, `@=` are not
        // bare assignments — `find_assignment_eq` should return None so the
        // pipe-prefix splitter treats the whole line as a non-assignment.
        assert!(find_assignment_eq("x += 1").is_none());
        assert!(find_assignment_eq("x -= 1").is_none());
        assert!(find_assignment_eq("x *= 2").is_none());
        assert!(find_assignment_eq("x /= 2").is_none());
        assert!(find_assignment_eq("x %= 2").is_none());
        assert!(find_assignment_eq("x **= 2").is_none());
        assert!(find_assignment_eq("x //= 2").is_none());
        assert!(find_assignment_eq("x <<= 2").is_none());
        assert!(find_assignment_eq("x >>= 2").is_none());
        assert!(find_assignment_eq("x &= 2").is_none());
        assert!(find_assignment_eq("x |= 2").is_none());
        assert!(find_assignment_eq("x ^= 2").is_none());
        assert!(find_assignment_eq("x := 2").is_none());
        assert!(find_assignment_eq("x @= m").is_none());
        // But a plain `=` is still found.
        assert_eq!(find_assignment_eq("x = 1"), Some(2));
    }

    #[test]
    fn pipe_with_augmented_assignment_prefix() {
        // `x += a |> f` must split into "prefix = `x += `, chain = `a |> f`"
        // and emit `x += f(a)` — not `x + = ...` (broken Python) and not
        // `f(x += a)` (wraps the assignment).
        assert_eq!(expand_pipes("x += a |> f\n"), "x += f(a)\n");
        assert_eq!(expand_pipes("x *= a |> f\n"), "x *= f(a)\n");
        assert_eq!(expand_pipes("x //= a |> f\n"), "x //= f(a)\n");
        assert_eq!(expand_pipes("x <<= a |> f\n"), "x <<= f(a)\n");
    }

    #[test]
    fn pipe_walrus_in_lhs_not_treated_as_prefix() {
        // `:=` is an expression operator, not a statement assignment, so
        // it must remain part of the chain LHS rather than being split off.
        let out = expand_pipes("y = (z := x) |> f\n");
        assert_eq!(out, "y = f((z := x))\n");
    }

    #[test]
    fn with_chain_indent_detection_uses_two_space_body() {
        // With a two-space body indent, the renderer must strip exactly two
        // spaces (not four) so the success body lines up at the chain indent.
        let src = "\
def run() -> Result[str, str]:
  with x = f()?:
    return Ok(x)
  else err:
    return Err(err)
";
        let out = expand_with_chains(src);
        // The success-body `return Ok(x)` was at 4 spaces (2 outer + 2 inner);
        // after flattening it should sit at 2 spaces.
        assert!(
            out.contains("\n  return Ok(x)"),
            "two-space indent not preserved: {out}"
        );
    }

    // ── lazy import tests ────────────────────────────────────────────────────

    #[test]
    fn preprocess_lazy_import_converts_to_standard_import() {
        let src = "lazy import np = numpy\n";
        let result = preprocess(src);
        assert!(
            result.python_source.contains("import numpy as np"),
            "preprocess should emit `import numpy as np`, got: {}",
            result.python_source
        );
        assert_eq!(result.lazy_imports.len(), 1);
        assert_eq!(result.lazy_imports[0].alias, "np");
        assert_eq!(result.lazy_imports[0].module, "numpy");
    }

    #[test]
    fn preprocess_lazy_import_dotted_module() {
        let src = "lazy import npr = numpy.random\n";
        let result = preprocess(src);
        assert!(
            result.python_source.contains("import numpy.random as npr"),
            "got: {}",
            result.python_source
        );
        assert_eq!(result.lazy_imports[0].module, "numpy.random");
    }

    #[test]
    fn expand_lazy_imports_emits_runtime_helper_call() {
        let src = "lazy import np = numpy\n\nx = np.array([1])\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("from typhon_runtime.lazy import lazy_import as __typhon_lazy_import"),
            "should inject runtime helper import, got:\n{out}"
        );
        assert!(
            out.contains("np = __typhon_lazy_import(\"numpy\")"),
            "should lower lazy import to a runtime helper call, got:\n{out}"
        );
        // The bespoke per-import proxy class is gone — the runtime
        // helper handles deferred loading via importlib.util.LazyLoader.
        assert!(
            !out.contains("__TyphonLazy_"),
            "old proxy class form must not be emitted, got:\n{out}"
        );
        assert!(
            !out.contains("import threading"),
            "lazy import no longer needs threading at the call site, got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_imports_multiple_imports_share_one_header_import() {
        let src = "lazy import np = numpy\nlazy import pd = pandas\n";
        let out = expand_lazy_imports(src);
        // Three imports → one header `from typhon_runtime.lazy import ...`
        // line, not three (the old emission ballooned linearly).
        let header_count = out
            .matches("from typhon_runtime.lazy import lazy_import as __typhon_lazy_import")
            .count();
        assert_eq!(
            header_count, 1,
            "header import should appear exactly once, got:\n{out}"
        );
        assert!(
            out.contains("np = __typhon_lazy_import(\"numpy\")"),
            "np call missing, got:\n{out}"
        );
        assert!(
            out.contains("pd = __typhon_lazy_import(\"pandas\")"),
            "pd call missing, got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_imports_supports_dotted_submodules() {
        // `LazyLoader` resolves dotted module paths via `find_spec`, so
        // `lazy import nn = torch.nn` lowers to the same one-line form.
        let src = "lazy import nn = torch.nn\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("nn = __typhon_lazy_import(\"torch.nn\")"),
            "dotted module name should round-trip through the call, got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_imports_safe_inside_triple_quoted_string() {
        // A `lazy import` that appears literally inside a docstring must NOT
        // be rewritten — it is string content, not a statement.
        let src = "x = \"\"\"\nlazy import np = numpy\n\"\"\"\n";
        let out = expand_lazy_imports(src);
        assert!(
            !out.contains("__typhon_lazy_import"),
            "lazy import inside string must not be expanded, got:\n{out}"
        );
        assert!(
            out.contains("lazy import np = numpy"),
            "original text must be preserved"
        );
    }

    #[test]
    fn expand_lazy_imports_trailing_comment_handled() {
        let src = "lazy import np = numpy  # noqa\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("np = __typhon_lazy_import(\"numpy\")"),
            "trailing comment should not prevent expansion, got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_imports_inserts_header_after_module_docstring_single_line() {
        // A single-line module docstring must remain the first
        // statement of the module — inserting the injected header
        // before it would demote it to a dead expression and break
        // `__doc__` / `help(module)`.
        let src = "\"\"\"My module.\"\"\"\nlazy import np = numpy\n";
        let out = expand_lazy_imports(src);
        let doc_pos = out.find("\"\"\"My module.\"\"\"").expect("docstring");
        let import_pos = out
            .find("from typhon_runtime.lazy import lazy_import")
            .expect("injected import");
        assert!(
            doc_pos < import_pos,
            "module docstring must precede the injected import; got:\n{out}",
        );
    }

    #[test]
    fn expand_lazy_imports_inserts_header_after_module_docstring_multi_line() {
        // Same guarantee for a multi-line docstring spanning several lines.
        let src = "\"\"\"First line.\n\nMore detail.\n\"\"\"\nlazy import np = numpy\n";
        let out = expand_lazy_imports(src);
        let doc_close = out.find("More detail.\n\"\"\"").expect("docstring close");
        let import_pos = out
            .find("from typhon_runtime.lazy import lazy_import")
            .expect("injected import");
        assert!(
            doc_close < import_pos,
            "multi-line docstring must fully precede the injected import; got:\n{out}",
        );
    }

    #[test]
    fn expand_lazy_imports_inserts_header_after_future_and_docstring() {
        // Real-world mixed case: `from __future__` plus a module
        // docstring. The injected import must land after both.
        let src =
            "from __future__ import annotations\n\"\"\"Module.\"\"\"\nlazy import np = numpy\n";
        let out = expand_lazy_imports(src);
        let future_pos = out.find("from __future__").expect("__future__");
        let doc_pos = out.find("\"\"\"Module.\"\"\"").expect("docstring");
        let import_pos = out
            .find("from typhon_runtime.lazy import lazy_import")
            .expect("injected import");
        assert!(
            future_pos < doc_pos && doc_pos < import_pos,
            "order must be __future__ → docstring → injected import; got:\n{out}",
        );
    }

    #[test]
    fn expand_lazy_imports_passes_through_non_lazy_lines() {
        let src = "import os\nval x: int = 1\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("import os"),
            "non-lazy imports must pass through"
        );
        assert!(
            out.contains("val x: int = 1"),
            "non-lazy lines must pass through"
        );
        assert!(
            !out.contains("threading"),
            "no lazy import should not add threading"
        );
    }

    #[test]
    fn preprocess_lazy_import_not_recognised_with_indentation() {
        // `lazy import` inside a function body is not Typhon syntax and must
        // not be rewritten; the Python parser will handle it as-is.
        let src = "def f():\n    lazy = 1\n";
        let result = preprocess(src);
        assert!(
            result.lazy_imports.is_empty(),
            "indented lazy should not be recorded"
        );
    }

    #[test]
    fn expand_lazy_let_module_level_lowers_to_lazy_let_call() {
        let src = "lazy let CONFIG: dict = load_config()\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("from typhon_runtime.lazy import lazy_let as __typhon_lazy_let"),
            "module-level lazy let should inject the runtime import; got:\n{out}"
        );
        assert!(
            out.contains("CONFIG: dict = __typhon_lazy_let(lambda: load_config())"),
            "module-level lazy let should lower to lazy_let(lambda: …); got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_let_module_level_without_annotation() {
        let src = "lazy let PORT = 8080\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("PORT = __typhon_lazy_let(lambda: 8080)"),
            "lazy let without annotation should lower without colon: {out}"
        );
    }

    #[test]
    fn expand_lazy_let_inside_class_lowers_to_cached_property() {
        let src = "class Foo:\n    lazy let expensive: int = compute()\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("from functools import cached_property as _typhon_cached_property"),
            "class-body lazy let should inject cached_property import; got:\n{out}"
        );
        assert!(
            out.contains("@_typhon_cached_property"),
            "class-body lazy let should emit the @cached_property decorator; got:\n{out}"
        );
        assert!(
            out.contains("def expensive(self) -> int:"),
            "class-body lazy let should emit a method signature; got:\n{out}"
        );
        assert!(
            out.contains("return compute()"),
            "class-body lazy let body should return the expr; got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_let_inside_impl_block_lowers_to_cached_property() {
        let src = "class Service:\n    base: int\n\nimpl Service:\n    lazy let derived: int = self.base * 10\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("@_typhon_cached_property"),
            "impl-block lazy let should emit the @cached_property decorator; got:\n{out}"
        );
        assert!(
            out.contains("def derived(self) -> int:"),
            "impl-block lazy let should emit a method signature; got:\n{out}"
        );
        assert!(
            !out.contains("lazy let derived"),
            "impl-block lazy let must be rewritten; got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_let_inserts_imports_after_future() {
        let src = "from __future__ import annotations\nlazy let X = 1\n";
        let out = expand_lazy_imports(src);
        // The __future__ line must remain first; the injected import must
        // appear after it.
        let future_pos = out
            .find("from __future__")
            .expect("future import preserved");
        let runtime_pos = out
            .find("from typhon_runtime.lazy")
            .expect("runtime import injected");
        assert!(
            future_pos < runtime_pos,
            "__future__ must precede injected import; got:\n{out}"
        );
    }

    #[test]
    fn expand_lazy_let_passes_through_inside_function() {
        // Function-local lazy let is not supported in v1; the line must not
        // be rewritten (the parser will flag it as a syntax error).
        let src = "def f():\n    lazy let x: int = 1\n    return x\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("lazy let x: int = 1"),
            "function-local lazy let should be passed through verbatim; got:\n{out}"
        );
        assert!(
            !out.contains("__typhon_lazy_let"),
            "function-local lazy let must not be lowered; got:\n{out}"
        );
    }

    #[test]
    fn preprocess_strips_only_lazy_prefix_for_round_trip() {
        // Only the `lazy` keyword is stripped; the inner `let` (or `mut`)
        // is left for the Ruff parser. The formatter restores `lazy` via
        // the stripped-keyword list.
        let result = preprocess("lazy let CONFIG: int = 1\n");
        assert_eq!(result.python_source, "let CONFIG: int = 1\n");
        let kinds: Vec<TyphonKeyword> = result.stripped.iter().map(|sk| sk.keyword).collect();
        assert!(
            kinds == vec![TyphonKeyword::Lazy],
            "stripped list should contain only Lazy; got {:?}",
            kinds
        );
        // Round-trip: postprocess must rebuild the original `lazy let` form.
        let restored = postprocess(&result.python_source, &result.stripped, &result.optionals);
        assert_eq!(restored, "lazy let CONFIG: int = 1\n");
    }

    #[test]
    fn expand_lazy_let_parser_skips_compound_assignment() {
        // `==` inside the expression must not be confused with the binding `=`.
        let src = "lazy let FLAG = a == b\n";
        let out = expand_lazy_imports(src);
        assert!(
            out.contains("__typhon_lazy_let(lambda: a == b)"),
            "RHS containing `==` must be captured intact; got:\n{out}"
        );
    }

    #[test]
    fn with_chain_else_header_without_colon_rejected() {
        // `else err` (missing trailing `:`) is malformed; the chain should be
        // emitted verbatim so the user sees a Python syntax error rather than
        // a silent rewrite that defaults to `_err`.
        let src = "\
def run() -> Result[str, str]:
    with x = f()?:
        return Ok(x)
    else err
        return Err(err)
";
        let out = expand_with_chains(src);
        // No rewrite occurred — the `with` keyword survives in the output.
        assert!(
            out.contains("with x = f()?:"),
            "malformed chain should have been left verbatim: {out}"
        );
        // And no temporary was injected.
        assert!(!out.contains("__typhon_with_"), "should not desugar: {out}");
    }

    // ── extend keyword (alias to impl for user-defined classes) ──────────────

    #[test]
    fn extend_keyword_becomes_impl_stub() {
        let result = preprocess("extend User:\n    def greet(self):\n        return 'hi'\n");
        assert!(
            result
                .python_source
                .contains("class __typhon_impl_User(object):"),
            "expected impl stub; got:\n{}",
            result.python_source
        );
        assert!(
            result
                .stripped
                .iter()
                .any(|k| matches!(k.keyword, TyphonKeyword::Extend)),
            "expected Extend keyword in stripped list"
        );
    }

    #[test]
    fn extend_keyword_round_trips() {
        let src = "extend User:\n    def greet(self):\n        return 'hi'\n";
        let result = preprocess(src);
        let restored = postprocess(&result.python_source, &result.stripped, &result.optionals);
        assert!(
            restored.starts_with("extend User:"),
            "did not round-trip: {restored}"
        );
    }

    // ── interface keyword (Protocol class lowering) ──────────────────────────

    #[test]
    fn interface_keyword_becomes_protocol_class() {
        let result = preprocess("interface Drawable:\n    def draw(self) -> None: ...\n");
        assert!(
            result.python_source.contains("class Drawable(Protocol):"),
            "expected Protocol class; got:\n{}",
            result.python_source
        );
        assert!(result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::Interface)));
    }

    #[test]
    fn interface_keyword_round_trips() {
        let src = "interface Drawable:\n    def draw(self) -> None: ...\n";
        let result = preprocess(src);
        let restored = postprocess(&result.python_source, &result.stripped, &result.optionals);
        assert!(
            restored.starts_with("interface Drawable:"),
            "did not round-trip: {restored}"
        );
    }

    // ── unsafe block ─────────────────────────────────────────────────────────

    #[test]
    fn unsafe_keyword_lowers_to_if_true() {
        let result = preprocess("unsafe:\n    x = something()\n");
        assert!(
            result.python_source.starts_with("if True:"),
            "expected if-True wrapper; got:\n{}",
            result.python_source
        );
        assert!(result
            .stripped
            .iter()
            .any(|k| matches!(k.keyword, TyphonKeyword::Unsafe)));
    }

    #[test]
    fn unsafe_keyword_round_trips() {
        let src = "unsafe:\n    x = something()\n";
        let result = preprocess(src);
        let restored = postprocess(&result.python_source, &result.stripped, &result.optionals);
        assert!(
            restored.starts_with("unsafe:"),
            "did not round-trip: {restored}"
        );
    }

    // ── lazy import (using main's LazyImport metadata + expand_lazy_imports) ─

    #[test]
    fn validate_lazy_usage_flags_lazy_from() {
        let errors = validate_lazy_usage("lazy from numpy import array\n");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("lazy from"));
    }

    #[test]
    fn validate_lazy_usage_silent_on_lazy_import() {
        let errors = validate_lazy_usage("lazy import np = numpy\n");
        assert!(errors.is_empty());
    }

    // ── gather block ─────────────────────────────────────────────────────────

    #[test]
    fn gather_default_lowers_to_task_group() {
        let src = "async def f():\n    gather:\n        user = fetch_user()\n        posts = fetch_posts()\n";
        let out = expand_gather_blocks(src);
        assert!(
            out.contains("async with asyncio.TaskGroup()"),
            "TaskGroup pattern missing: {out}"
        );
        assert!(
            out.contains(".create_task(fetch_user())"),
            "create_task call missing: {out}"
        );
        assert!(
            out.contains("let user = __typhon_gather"),
            "user binding missing or unbound by `let`: {out}"
        );
    }

    #[test]
    fn gather_best_effort_lowers_to_asyncio_gather() {
        let src = "async def f():\n    gather(strategy=\"best-effort\"):\n        a = call_one()\n        b = call_two()\n";
        let out = expand_gather_blocks(src);
        assert!(
            out.contains("await asyncio.gather("),
            "asyncio.gather missing: {out}"
        );
        assert!(
            out.contains("return_exceptions=True"),
            "return_exceptions flag missing: {out}"
        );
        // Each user binding is indexed out of the results vector with an
        // explicit `let` so Rule-2's `tyc::missing_binding_kind` doesn't
        // fire on the lowered statements.
        assert!(
            out.contains("let a = __typhon_gather"),
            "let a indexed binding missing: {out}"
        );
        assert!(
            out.contains("let b = __typhon_gather"),
            "let b indexed binding missing: {out}"
        );
        assert!(out.contains("[0]"), "index 0 missing: {out}");
        assert!(out.contains("[1]"), "index 1 missing: {out}");
    }

    #[test]
    fn gather_malformed_block_is_left_alone() {
        // No body lines after the header — should bail out and emit the input
        // verbatim so the Python parser surfaces a precise error.
        let src = "gather:\n";
        let out = expand_gather_blocks(src);
        assert_eq!(out, src);
    }

    #[test]
    fn gather_inline_comment_stripped_from_binding() {
        // FINDINGS #93: a trailing comment on a gather binding was spliced
        // into the `create_task(...)` call, closing the paren after the
        // comment text and producing a cascade of synthetic parse errors.
        let src = "async def f():\n    gather:\n        a = fetch_a()  # first fetch\n        b = fetch_b()  # second fetch\n";
        let out = expand_gather_blocks(src);
        // The comments must not appear in the lowered code at all.
        assert!(
            !out.contains("# first fetch"),
            "comment leaked into lowering: {out}"
        );
        assert!(
            !out.contains("# second fetch"),
            "comment leaked into lowering: {out}"
        );
        // The expressions must be correct.
        assert!(
            out.contains(".create_task(fetch_a())"),
            "create_task call missing or malformed: {out}"
        );
        assert!(
            out.contains(".create_task(fetch_b())"),
            "create_task call missing or malformed: {out}"
        );
    }

    #[test]
    fn gather_comment_with_equals_does_not_confuse_assignment_parser() {
        // FINDINGS #93 (hardened): a comment containing `=` must not be seen
        // by find_assignment_eq.  Previously find_assignment_eq ran on the raw
        // body including the comment, so `# k=v` would be harmless here only
        // by coincidence (first `=` wins).  The fix uses scan_line_code_end
        // first so find_assignment_eq only ever sees comment-free code.
        let src = "async def f():\n    gather:\n        a = fetch_a()  # k=v style note\n        b = fetch_b()  # result=ok\n";
        let out = expand_gather_blocks(src);
        assert!(
            !out.contains("# k=v"),
            "comment leaked into lowering: {out}"
        );
        assert!(
            out.contains(".create_task(fetch_a())"),
            "create_task(fetch_a()) missing: {out}"
        );
        assert!(
            out.contains(".create_task(fetch_b())"),
            "create_task(fetch_b()) missing: {out}"
        );
    }

    // ── go spawn ─────────────────────────────────────────────────────────────

    #[test]
    fn go_call_lowers_to_runtime_spawn() {
        let out = expand_go_calls("async def f():\n    go fetch(x)\n");
        assert!(
            out.contains("typhon_runtime.tasks.spawn(fetch(x))"),
            "spawn missing: {out}"
        );
    }

    #[test]
    fn go_call_with_handle_binds_task() {
        let out = expand_go_calls("async def f():\n    go fetch(x) -> fut\n");
        assert!(
            out.contains("let fut = typhon_runtime.tasks.spawn(fetch(x))"),
            "handle binding missing or unbound by `let`: {out}"
        );
    }

    #[test]
    fn go_with_dotted_callable() {
        let out = expand_go_calls("async def f():\n    go svc.fetch(x)\n");
        assert!(
            out.contains("typhon_runtime.tasks.spawn(svc.fetch(x))"),
            "dotted call lowering missing: {out}"
        );
    }

    #[test]
    fn go_not_rewritten_for_unrelated_identifiers() {
        // `goto` (no space) must not trigger the rewrite.
        let out = expand_go_calls("goto = 1\n");
        assert_eq!(out, "goto = 1\n");
    }

    // ── #2: multi-line guard ────────────────────────────────────────────

    #[test]
    fn expand_multiline_guards_simple_body() {
        let src = "\
def f(x: int?) -> int:
    guard v = x else:
        return 0
    return v
";
        let out = expand_multiline_guards(src);
        assert!(out.contains("let __typhon_mguard_0 = (x)"));
        assert!(out.contains("if __typhon_mguard_0 is None:"));
        assert!(out.contains("        return 0"));
        assert!(out.contains("let v = __typhon_mguard_0"));
        assert!(!out.contains("guard v = x else:"));
    }

    #[test]
    fn expand_multiline_guards_preserves_multiple_body_statements() {
        let src = "\
def f(x: int?) -> int:
    guard v = x else:
        print(\"oops\")
        return 0
    return v
";
        let out = expand_multiline_guards(src);
        assert!(out.contains("        print(\"oops\")"));
        assert!(out.contains("        return 0"));
    }

    #[test]
    fn expand_multiline_guards_leaves_single_line_form_alone() {
        // Single-line form is handled inside `preprocess`; this pre-pass
        // must not touch it.
        let src = "def f(x: int?) -> int:\n    guard v = x else: return 0\n    return v\n";
        let out = expand_multiline_guards(src);
        assert_eq!(out, src);
    }

    #[test]
    fn expand_multiline_guards_empty_body_left_alone() {
        // No indented body after `else:` — let the parser produce its
        // own (better) diagnostic. Don't rewrite into nonsense.
        let src = "def f(x: int?) -> int:\n    guard v = x else:\n    return v\n";
        let out = expand_multiline_guards(src);
        assert_eq!(out, src);
    }

    #[test]
    fn expand_multiline_guards_counter_is_per_call_unique() {
        let src = "\
def a(x: int?) -> int:
    guard v = x else:
        return 0
    return v

def b(y: int?) -> int:
    guard w = y else:
        return 0
    return w
";
        let out = expand_multiline_guards(src);
        assert!(out.contains("__typhon_mguard_0"));
        assert!(out.contains("__typhon_mguard_1"));
    }

    #[test]
    fn expand_multiline_guards_inside_string_is_untouched() {
        let src = "x = \"\"\"\nguard v = x else:\n    return 0\n\"\"\"\n";
        let out = expand_multiline_guards(src);
        // The lowered form must not appear inside a string literal.
        assert!(!out.contains("__typhon_mguard_"));
        assert_eq!(out, src);
    }

    #[test]
    fn expand_multiline_guards_leading_blank_line_is_absorbed() {
        // Python ignores blank lines for indentation; a leading blank
        // line after `else:` must not terminate the body before the
        // first indented statement is seen. Regression for the
        // gemini-code-assist / Copilot reviews on PR #51.
        let src = "\
def f(x: int?) -> int:
    guard v = x else:

        return 0
    return v
";
        let out = expand_multiline_guards(src);
        assert!(
            out.contains("if __typhon_mguard_0 is None:"),
            "leading blank must not break the rewrite; got:\n{out}"
        );
        assert!(
            out.contains("        return 0"),
            "body statement after the blank must be preserved; got:\n{out}"
        );
    }

    #[test]
    fn expand_multiline_guards_comment_only_dedent_does_not_terminate_body() {
        // Comment-only lines (even at column 0) don't change Python's
        // indentation context, so they must not split the body across
        // the lowered `if`. Regression for the Codex P1 review on
        // PR #51.
        let src = "\
def f(x: int?) -> int:
    guard v = x else:
        log(\"first\")
# leading-column comment between body statements
        return 0
    return v
";
        let out = expand_multiline_guards(src);
        // Both body statements end up inside the lowered `if`, with the
        // comment preserved between them.
        let if_block_start = out.find("if __typhon_mguard_0 is None:").unwrap();
        let after_if = &out[if_block_start..];
        let log_pos = after_if
            .find("        log(\"first\")")
            .expect("log call in body");
        let comment_pos = after_if
            .find("# leading-column comment between body statements")
            .expect("comment preserved");
        let return_pos = after_if.find("        return 0").expect("return in body");
        // Source order is preserved.
        assert!(log_pos < comment_pos && comment_pos < return_pos);
        // The `let v = …` binding sits after the body, not before
        // `return 0` (i.e. the comment did NOT terminate the body).
        let let_pos = after_if.find("    let v = __typhon_mguard_0").unwrap();
        assert!(return_pos < let_pos);
    }

    #[test]
    fn expand_multiline_guards_triple_quoted_string_in_body_preserved() {
        // A triple-quoted string opened in the body whose content
        // dedents to column 0 must not terminate the body. Regression
        // for the Copilot review on PR #51.
        let src = "\
def f(x: int?) -> int:
    guard v = x else:
        log(\"\"\"
multi-line
string content
\"\"\")
        return 0
    return v
";
        let out = expand_multiline_guards(src);
        assert!(out.contains("if __typhon_mguard_0 is None:"));
        assert!(out.contains("multi-line"));
        assert!(out.contains("string content"));
        assert!(out.contains("        return 0"));
    }

    #[test]
    fn gather_strips_trailing_comment() {
        let src = "async def f() -> None:\n    gather:\n        a = fetch_a()  # first\n        b = fetch_b()  # second\n    print(a, b)\n";
        let out = expand_gather_blocks(src);
        assert!(
            !out.contains("# first"),
            "comment should be stripped: {out}"
        );
        assert!(
            !out.contains("# second"),
            "comment should be stripped: {out}"
        );
        assert!(out.contains("create_task(fetch_a())"), "call intact: {out}");
        assert!(out.contains("create_task(fetch_b())"), "call intact: {out}");
    }

    #[test]
    fn gather_preserves_hash_inside_triple_quoted_string() {
        // A `#` inside a triple-quoted string literal is NOT a comment; the
        // expression must not be truncated.
        let src =
            "async def f() -> None:\n    gather:\n        a = get(\"\"\"a#b\"\"\")\n    print(a)\n";
        let out = expand_gather_blocks(src);
        assert!(
            out.contains(r#"create_task(get("""a#b"""))"#),
            "triple-quoted # must not be stripped: {out}"
        );
    }
}
