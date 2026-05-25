# 12 — Static site generator

A Markdown → HTML static site generator built end-to-end in Typhon, with:

- **Markdown lexer + recursive parser** producing a sealed-union `Block` / `Inline` AST
- **YAML-ish frontmatter parser** at the head of every `.md` source
- **Template engine** with `{{ expr }}`, `{% if %}`, `{% for %}`, `{% block %}`, `{% extends %}`
- **Template inheritance** — child layouts override named blocks from a parent
- **Multi-stage pipeline** — read → parse frontmatter → parse markdown → render → write, each stage with its own `Result[T, StageError]` variant
- **Asset pipeline** — copies `assets/` recursively, fingerprints filenames with sha256 for cache-busting
- **RSS feed**, **sitemap.xml**, **robots.txt** emitted alongside the rendered pages
- **Incremental rebuild** backed by a SQLite manifest of `(page_id, source_hash, out_path)`
- **CLI** with `build`, `clean`, `serve` subcommands via `argparse`

## Files

| File | Responsibility |
|---|---|
| `src/ids.ty` | `newtype` wrappers (`PageId`, `AssetHash`, `TemplateName`, `SlugStr`, `SourceHash`) |
| `src/models.ty` | Sealed-union AST nodes (`Block`/`Inline`/`TplNode`), `FrontMatter` model, `StageError` variants, factory functions |
| `src/config.ty` | `freeze let` escape tables, `comptime let` build version, `SiteConfig` |
| `src/frontmatter.ty` | YAML-lite frontmatter parser → `Result[FrontMatter, FrontMatterError]` |
| `src/md_lex.ty` | Line-level Markdown lexer (`MdLine` sealed union) |
| `src/md_parse.ty` | Inline + block Markdown parser, recursive HTML renderer |
| `src/tpl_lex.ty` | Template tokenizer (`{{ ... }}` / `{% ... %}` / text) |
| `src/tpl_parse.ty` | Template parser → `Template` AST |
| `src/tpl_render.ty` | Context-driven template renderer with block inheritance |
| `src/manifest.ty` | SQLite-backed incremental-build manifest with transactions |
| `src/assets.ty` | Asset hashing + copy pipeline |
| `src/feed.ty` | RSS (Atom) + sitemap.xml + robots.txt emission |
| `src/pipeline.ty` | Multi-stage Result-chained build pass |
| `src/cli.ty` | `argparse` subcommand dispatcher |
| `src/main.ty` | Entry point |

## Features exercised

- Two distinct sealed-union ASTs (`Block`/`Inline` for Markdown, `TplNode` for templates)
- Recursive AST walking + recursive HTML rendering
- `freeze let` for HTML escape table, Atom namespace, max incremental cache size
- `comptime let` for build version, site URL, site title
- `model` boundary at frontmatter (`FrontMatter` lives in models)
- `Result[T, StageError]` with 6 distinct stage-error variants
- `match` over Markdown AST and template AST with full coverage (12+ arms)
- `newtype` for `PageId`, `AssetHash`, `TemplateName`, `SlugStr`, `SourceHash`
- `pub` markers on every public symbol; factory functions in the union's module (Round-1 #1 workaround)
- SQLite manifest with explicit `BEGIN IMMEDIATE` transactions
- `argparse` subcommand dispatch (`build`/`clean`/`serve`)
- Heterogeneous-error pipeline (R2-6) — see `pipeline.ty` for the practical shape

## Running

```bash
cd examples/apps/12-static-site-gen
tyc check src/
tyc build
# Populate a site:
mkdir -p content templates assets
cat > content/hello.md <<'EOF'
---
title: Hello
date: 2026-01-15
tags: [intro, demo]
layout: post
---
# Hello, world

Some *emphasised* and **strong** text plus a [link](https://example.com).
EOF
cat > templates/post.html <<'EOF'
<!doctype html><html><head><title>{{ title }} – {{ site_title }}</title></head>
<body>{% block content %}{{ raw content }}{% endblock %}</body></html>
EOF
python build/main.py build --content content --templates templates --output public
ls public/
```
