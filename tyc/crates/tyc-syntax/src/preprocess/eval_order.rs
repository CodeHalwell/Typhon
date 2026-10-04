//! Python evaluation order for inline `?` (W7-04).
//!
//! The inline-`?` pass lifts each propagated operand into a guard placed
//! *above* the statement that contains it:
//!
//! ```text
//! return Ok(Node(start=self.pos, text=self.word()?))
//! # becomes
//! __typhon_qi_0__ = self.word()
//! if isinstance(__typhon_qi_0__, __typhon_Err__):
//!     return __typhon_qi_0__
//! return Ok(Node(start=self.pos, text=__typhon_qi_0__.value))
//! ```
//!
//! That moves the operand's evaluation ahead of everything the statement
//! evaluates *before* it in Python's order — here `self.pos`, which
//! `word()` advances, so the node recorded `start=5` instead of `0`. The
//! same lift ran `second()` before `first()` in `combine(first(), second()?)`
//! (and skipped `first()` entirely when `second()` failed), ran list elements
//! out of order, and evaluated a subscript target's index before the
//! right-hand side. Both execution surfaces share the lowering, so the
//! VM ↔ CPython differential could not see it.
//!
//! This pass runs just before the lift. For every statement that carries an
//! inline-propagated `?`, it walks the statement in Python evaluation order
//! and hoists each *non-trivial* expression evaluated before a propagated
//! operand into a `__typhon_ev_N__` temporary, emitted above the statement
//! in that order. The lift then places its guard after those temporaries,
//! so the observable order is Python's again:
//!
//! ```text
//! __typhon_ev_0__ = self.pos
//! __typhon_qi_0__ = self.word()          # (from the inline pass)
//! ...
//! return Ok(Node(start=__typhon_ev_0__, text=__typhon_qi_0__.value))
//! ```
//!
//! A hoisted expression that itself contains `?` is hoisted whole — its own
//! operands are ordered when the pass re-runs over the new statement (the
//! pass iterates to a fixpoint).
//!
//! "Trivial" — left in place — means evaluating it can neither have an
//! effect nor observe one an operand might have: literals (and displays of
//! them), names no call can rebind, lambdas, `super()`, empty
//! builtin-constructor calls, and type-expression subscripts (`list[int]`).
//! A name *can* be rebound by the operand when the file declares it
//! `global` or `nonlocal` somewhere, or the statement assigns it with a
//! walrus; such a name is hoisted like any other value (`f(x, g()?)` where
//! `g` does `global x; x = …` reads the old `x`).
//!
//! A call receiver is evaluated before the arguments, so a dotted receiver
//! (`self.items.append(self.word()?)`) is hoisted: `word()` may rebind
//! `self.items`, and Python appends to the list it had read. A receiver
//! that is a plain name follows the name rule; one rooted at an imported
//! module (`os.path.join(…)`) is left in place.
//!
//! An augmented assignment loads its target before evaluating the value, so
//! one whose value carries a propagated operand and whose target the
//! operand could change — an attribute, a subscript, or a rebindable name —
//! is spelled out in Python's order:
//!
//! ```text
//! self.pos += self.advance()?
//! # becomes
//! __typhon_ev_0__ = self.pos
//! __typhon_ev_0__ += self.advance()?
//! self.pos = __typhon_ev_0__
//! ```
//!
//! (with the container and a non-trivial index hoisted first, so each is
//! evaluated once). `+=` on the temporary keeps the in-place `__iadd__`
//! semantics: a list target is extended, not copied.
//!
//! Anything the pass cannot model with certainty — a statement the parser
//! cannot read once its `?`s are masked, `as!` / `rescue` still in it, an
//! operand under `and` / `or` / a ternary / a lambda / a comprehension /
//! an f-string, a later `with` item, a compound header other than
//! `if` / `for` / `with` — is left exactly as it was, so the pass can only
//! ever reorder a statement it fully understands.

use std::collections::HashSet;
use std::ops::Range;

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

use super::{
    compose_line_maps, find_first_inline_propagation_q, identity_line_map, operand_start,
    q_contexts, rescue_keyword_offset, text_line_count, MappedOut,
};
use crate::lexmask::LexMask;

/// Upper bound on fixpoint rounds. Each round strictly shrinks the
/// statements that still need work; the bound only guards a logic error.
const MAX_ROUNDS: usize = 32;

/// Prefix of the temporaries this pass introduces. Distinct from the `?`
/// passes' `__typhon_q_` / `__typhon_qi_` so the type checker's
/// "`?` temporary" handling (`missing_await` on the operand) never applies
/// to a hoisted sibling.
const TEMP_PREFIX: &str = "__typhon_ev_";

/// Hoist, in Python evaluation order, every non-trivial expression a
/// statement evaluates before one of its inline-propagated `?` operands.
/// Returns the rewritten source and its output-line → input-line table.
pub(super) fn hoist_for_evaluation_order_mapped(source: &str) -> (String, Vec<usize>) {
    if !source.contains('?') {
        return (
            source.to_owned(),
            identity_line_map(text_line_count(source)),
        );
    }
    let mut counter = next_free_counter(source);
    let mut text = source.to_owned();
    let mut map = identity_line_map(text_line_count(source));
    for _ in 0..MAX_ROUNDS {
        let Some((out, round_map)) = hoist_once(&text, &mut counter) else {
            break;
        };
        map = compose_line_maps(&round_map, &map);
        text = out;
    }
    (text, map)
}

/// One past the largest `__typhon_ev_N__` already in `source`, so a
/// re-run over partially lowered text never reuses a name.
fn next_free_counter(source: &str) -> usize {
    let mut next = 0usize;
    let mut rest = source;
    while let Some(at) = rest.find(TEMP_PREFIX) {
        let tail = &rest[at + TEMP_PREFIX.len()..];
        let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = digits.parse::<usize>() {
            next = next.max(n + 1);
        }
        rest = tail;
    }
    next
}

/// One round over every logical statement. `None` when nothing changed.
fn hoist_once(text: &str, counter: &mut usize) -> Option<(String, Vec<usize>)> {
    let mask = LexMask::new(text);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut starts = Vec::with_capacity(lines.len() + 1);
    let mut acc = 0usize;
    for l in &lines {
        starts.push(acc);
        acc += l.len();
    }
    starts.push(acc);
    let contexts = q_contexts(text);
    let names = FileNames::scan(&mask, &lines);

    let mut out = MappedOut::with_capacity(text.len() + 64);
    let mut changed = false;
    let mut i = 0usize;
    while i < lines.len() {
        let mut j = i + 1;
        while j < lines.len() && !mask.is_logical_line_start(j) {
            j += 1;
        }
        let stmt = StmtSpan {
            text,
            mask: &mask,
            lines: &lines,
            starts: &starts,
            contexts: &contexts,
            names: &names,
            first: i,
            end: j,
        };
        match stmt.rewrite(counter) {
            Some(emitted) => {
                changed = true;
                for (line, src) in emitted {
                    out.mark(src);
                    out.push_str(&line);
                }
            }
            None => {
                for (k, line) in lines[i..j].iter().enumerate() {
                    out.mark(i + k);
                    out.push_str(line);
                }
            }
        }
        i = j;
    }
    changed.then(|| out.finish())
}

/// File-wide facts about names the walk needs: which names a call may
/// rebind (declared `global` / `nonlocal` somewhere), and which are bound by
/// an `import` (a receiver rooted at one is a module, left in place).
struct FileNames {
    rebindable: HashSet<String>,
    imported: HashSet<String>,
}

impl FileNames {
    fn scan(mask: &LexMask, lines: &[&str]) -> Self {
        let mut rebindable = HashSet::new();
        let mut imported = HashSet::new();
        let idents = |list: &str| -> Vec<String> {
            list.split(',')
                .map(|part| part.trim().trim_matches(['(', ')']).trim())
                .filter(|part| !part.is_empty())
                .map(|part| {
                    // `a.b as c` binds `c`; `a.b` binds `a`.
                    let bound = part.rsplit(" as ").next().unwrap_or(part).trim();
                    let bound = if part.contains(" as ") {
                        bound
                    } else {
                        bound.split('.').next().unwrap_or(bound)
                    };
                    bound.to_owned()
                })
                .collect()
        };
        for (li, line) in lines.iter().enumerate() {
            if !mask.is_logical_line_start(li) || mask.line_starts_in_string(li) {
                continue;
            }
            let raw = line.trim_end_matches(['\n', '\r']);
            let code = raw[..mask.line_code_end(li).min(raw.len())].trim();
            let code = code.strip_prefix("pub ").unwrap_or(code);
            if let Some(rest) = code
                .strip_prefix("global ")
                .or_else(|| code.strip_prefix("nonlocal "))
            {
                rebindable.extend(idents(rest));
            } else if let Some(rest) = code.strip_prefix("import ") {
                imported.extend(idents(rest));
            } else if let Some(rest) = code.strip_prefix("lazy import ") {
                // `lazy import np = numpy`
                if let Some(name) = rest.split('=').next() {
                    imported.insert(name.trim().to_owned());
                }
            } else if code.starts_with("from ") {
                if let Some((_, rest)) = code.split_once(" import ") {
                    imported.extend(idents(rest));
                }
            }
        }
        FileNames {
            rebindable,
            imported,
        }
    }
}

/// One logical statement: physical lines `first..end` of `text`.
struct StmtSpan<'a> {
    text: &'a str,
    mask: &'a LexMask,
    lines: &'a [&'a str],
    starts: &'a [usize],
    contexts: &'a [super::QContext<'a>],
    names: &'a FileNames,
    first: usize,
    end: usize,
}

impl StmtSpan<'_> {
    fn start(&self) -> usize {
        self.starts[self.first]
    }

    fn body(&self) -> &str {
        &self.text[self.start()..self.starts[self.end]]
    }

    /// Source line of statement-relative byte `offset`.
    fn line_of(&self, offset: usize) -> usize {
        self.first + self.body()[..offset].matches('\n').count()
    }

    /// The rewritten statement as `(physical line, source line)` pairs, or
    /// `None` when it needs no reordering (or cannot be modelled).
    fn rewrite(&self, counter: &mut usize) -> Option<Vec<(String, usize)>> {
        let body = self.body();
        let base = self.start();
        let bytes = body.as_bytes();

        // Cheap filter: a `?` in code somewhere in the statement.
        if !(0..bytes.len()).any(|k| bytes[k] == b'?' && self.mask.is_code(base + k)) {
            return None;
        }

        // Module level: a propagating `?` is illegal there (it lowers to a
        // `return`), and the checker reports it; leave the text alone.
        let indent_len = bytes
            .iter()
            .take_while(|b| matches!(b, b' ' | b'\t' | b'\x0c'))
            .count();
        if indent_len == 0 {
            return None;
        }
        // Compound headers other than `if` / `for` / `with` (and soft
        // keywords used as a statement's first word) are not modelled.
        let head = &body[indent_len..];
        let first_word = head
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or("");
        if head.starts_with('@')
            || matches!(
                first_word,
                "elif"
                    | "else"
                    | "try"
                    | "except"
                    | "finally"
                    | "case"
                    | "match"
                    | "def"
                    | "async"
                    | "class"
                    | "while"
                    | "lambda"
            )
        {
            return None;
        }

        // The inline-propagated `?`s, found exactly as the inline pass finds
        // them (statement-relative offsets), plus the operand each applies to.
        let mut inline_qs: Vec<usize> = Vec::new();
        let mut operands: Vec<Range<usize>> = Vec::new();
        for li in self.first..self.end {
            if self.mask.line_starts_in_string(li) {
                continue;
            }
            let raw = self.lines[li].trim_end_matches(['\n', '\r']);
            if rescue_keyword_offset(raw, None).is_some() {
                return None;
            }
            let code_end = self.mask.line_code_end(li).min(raw.len());
            let code = &raw[..code_end];
            let line_off = self.starts[li] - base;
            let ctx = self.contexts.get(li).copied().unwrap_or_default();
            let mut scratch = code.to_owned();
            while let Some(p) = find_first_inline_propagation_q(&scratch, ctx) {
                let op_start = operand_start(code, p)?;
                inline_qs.push(line_off + p);
                operands.push(line_off + op_start..line_off + p);
                scratch.replace_range(p..p + 1, " ");
            }
        }
        // The statement's trailing `?` (lowered by the end-of-line pass): a
        // hoisted expression that ends right before it carries it along.
        let last_line = self.end - 1;
        let trailing_q = {
            let raw = self.lines[last_line].trim_end_matches(['\n', '\r']);
            let code_end = self.mask.line_code_end(last_line).min(raw.len());
            let code = raw[..code_end].trim_end();
            code.ends_with('?')
                .then(|| self.starts[last_line] - base + code.len() - 1)
        };
        if inline_qs.is_empty() && trailing_q.is_none() {
            return None;
        }

        // A probe the parser can read: every code `?` masked to a space
        // (offsets unchanged), the head indentation dropped, and a compound
        // header given a body. Residual `as!` sugar is not modelled.
        let mut probe: Vec<u8> = bytes.to_vec();
        for (k, b) in probe.iter_mut().enumerate() {
            if *b == b'?' && self.mask.is_code(base + k) {
                *b = b' ';
            }
        }
        if (0..bytes.len().saturating_sub(2))
            .any(|k| &bytes[k..k + 3] == b"as!" && self.mask.is_code(base + k))
        {
            return None;
        }
        let mut probe = String::from_utf8(probe[indent_len..].to_vec()).ok()?;
        let last_code_end = {
            let raw = self.lines[last_line].trim_end_matches(['\n', '\r']);
            let code_end = self.mask.line_code_end(last_line).min(raw.len());
            self.starts[last_line] - base + raw[..code_end].trim_end().len()
        };
        if bytes.get(last_code_end.wrapping_sub(1)) == Some(&b':') {
            probe.insert_str(last_code_end - indent_len, " pass");
        }
        let parsed = ruff_python_parser::parse_module(&probe).ok()?;
        let module = parsed.syntax();
        let [stmt] = module.body.as_slice() else {
            return None;
        };

        // Names this statement could see rebound before it reads them: the
        // file's `global` / `nonlocal` names and its own walrus targets.
        let mut rebindable = self.names.rebindable.clone();
        collect_walrus_targets(stmt, &mut rebindable);

        if let Stmt::AugAssign(aug) = stmt {
            let value = aug.value.range();
            let value = value.start().to_usize() + indent_len..value.end().to_usize() + indent_len;
            let carries_operand = operands
                .iter()
                .any(|o| o.start >= value.start && o.end <= value.end)
                || trailing_q
                    .is_some_and(|q| q >= value.end && body[value.end..q].trim().is_empty());
            let target_may_change = match aug.target.as_ref() {
                Expr::Name(n) => rebindable.contains(n.id.as_str()),
                Expr::Attribute(_) | Expr::Subscript(_) => true,
                _ => false,
            };
            if carries_operand && target_may_change {
                return self.spell_out_aug_assign(
                    aug,
                    indent_len,
                    last_code_end,
                    &rebindable,
                    counter,
                );
            }
        }
        if inline_qs.is_empty() {
            return None;
        }

        let mut plan = Plan {
            offset: indent_len,
            operands: &operands,
            rebindable: &rebindable,
            imported: &self.names.imported,
            events: Vec::new(),
        };
        plan.visit_stmt(stmt).ok()?;
        // Every hoist lands above every lift. An operand the inline pass
        // would lift in place therefore has to be hoisted too when a hoist
        // follows it in evaluation order — otherwise it would run after
        // that hoist. Operands after the last hoist keep their in-place
        // lift (whose textual order is already their evaluation order).
        let last_hoist = plan
            .events
            .iter()
            .rposition(|e| matches!(e, Event::Hoist(_)))?;
        let planned: Vec<Range<usize>> = plan.events[..=last_hoist]
            .iter()
            .map(|e| match e {
                Event::Hoist(r) | Event::Operand(r) => r.clone(),
            })
            .collect();

        // Extend each hoist over a `?` that directly follows it — the
        // hoisted text is then itself a propagation the later passes lower.
        let mut hoists: Vec<Range<usize>> = Vec::with_capacity(planned.len());
        for r in planned {
            let mut r = r;
            if bytes.get(r.end) == Some(&b'?') && self.mask.is_code(base + r.end) {
                if inline_qs.contains(&r.end) || trailing_q == Some(r.end) {
                    r.end += 1;
                } else {
                    // A nullable-sugar `?` right after a value expression:
                    // not a shape this pass models.
                    return None;
                }
            }
            hoists.push(r);
        }
        // Disjoint by construction; verify rather than trust.
        let mut sorted = hoists.clone();
        sorted.sort_by_key(|r| r.start);
        if sorted.windows(2).any(|w| w[0].end > w[1].start) {
            return None;
        }

        let indent = &body[..indent_len];
        let mut emitted: Vec<(String, usize)> = Vec::new();
        let mut temps: Vec<(Range<usize>, String)> = Vec::new();
        for r in &hoists {
            let temp = format!("{TEMP_PREFIX}{}__", *counter);
            *counter += 1;
            let expr = &body[r.clone()];
            let first_src = self.line_of(r.start);
            let assign = format!("{indent}{temp} = {expr}\n");
            for (k, line) in assign.split_inclusive('\n').enumerate() {
                emitted.push((line.to_owned(), first_src + k));
            }
            temps.push((r.clone(), temp));
        }

        // The statement with every hoisted span replaced by its temporary.
        temps.sort_by_key(|(r, _)| r.start);
        let mut rewritten = String::with_capacity(body.len());
        // `(offset into rewritten, statement-relative source offset)` at the
        // start of every output line, to attribute it to a source line.
        let mut line_starts: Vec<usize> = vec![0];
        let mut pos = 0usize;
        let mut push_src = |rewritten: &mut String, from: usize, to: usize| {
            for (k, ch) in body[from..to].char_indices() {
                rewritten.push(ch);
                if ch == '\n' {
                    line_starts.push(from + k + 1);
                }
            }
        };
        for (r, temp) in &temps {
            push_src(&mut rewritten, pos, r.start);
            rewritten.push_str(temp);
            pos = r.end;
        }
        push_src(&mut rewritten, pos, body.len());
        for (k, line) in rewritten.split_inclusive('\n').enumerate() {
            let src_off = line_starts.get(k).copied().unwrap_or(0);
            emitted.push((line.to_owned(), self.line_of(src_off.min(body.len()))));
        }
        Some(emitted)
    }
}

impl StmtSpan<'_> {
    /// `TARGET op= VALUE` whose `VALUE` carries a propagated operand that
    /// could change `TARGET`: load the target into a temporary first, apply
    /// the operator to the temporary, store it back — Python's order for an
    /// augmented assignment. The container and a non-trivial index are
    /// hoisted so each is evaluated exactly once. `None` (leave the
    /// statement alone) for a slice target.
    fn spell_out_aug_assign(
        &self,
        aug: &ruff_python_ast::StmtAugAssign,
        indent_len: usize,
        last_code_end: usize,
        rebindable: &HashSet<String>,
        counter: &mut usize,
    ) -> Option<Vec<(String, usize)>> {
        let body = self.body();
        let indent = &body[..indent_len];
        let span = |node: &dyn Ranged| -> Range<usize> {
            let r = node.range();
            r.start().to_usize() + indent_len..r.end().to_usize() + indent_len
        };
        let mut emitted: Vec<(String, usize)> = Vec::new();
        let next_temp = |counter: &mut usize| {
            let t = format!("{TEMP_PREFIX}{}__", *counter);
            *counter += 1;
            t
        };
        // `e` as an operand that is read once, here: its text when reading
        // it again later cannot differ, else a hoisted temporary.
        let once = |e: &Expr, emitted: &mut Vec<(String, usize)>, counter: &mut usize| {
            let r = span(e);
            let stays = match e {
                Expr::Name(n) => !rebindable.contains(n.id.as_str()),
                Expr::StringLiteral(_)
                | Expr::BytesLiteral(_)
                | Expr::NumberLiteral(_)
                | Expr::BooleanLiteral(_)
                | Expr::NoneLiteral(_) => true,
                _ => false,
            };
            if stays {
                return body[r].to_owned();
            }
            let temp = next_temp(counter);
            let line = format!("{indent}{temp} = {}\n", &body[r.clone()]);
            let first = self.line_of(r.start);
            for (k, l) in line.split_inclusive('\n').enumerate() {
                emitted.push((l.to_owned(), first + k));
            }
            temp
        };
        let target = match aug.target.as_ref() {
            Expr::Name(n) => n.id.as_str().to_owned(),
            Expr::Attribute(a) => {
                let container = once(&a.value, &mut emitted, counter);
                format!("{container}.{}", a.attr.as_str())
            }
            Expr::Subscript(s) => {
                if matches!(s.slice.as_ref(), Expr::Slice(_)) {
                    return None;
                }
                let container = once(&s.value, &mut emitted, counter);
                let index = once(&s.slice, &mut emitted, counter);
                format!("{container}[{index}]")
            }
            _ => return None,
        };
        let target_span = span(aug.target.as_ref());
        let value_span = span(aug.value.as_ref());
        let op = body[target_span.end..value_span.start].trim();
        if !op.ends_with('=') {
            return None;
        }
        let first = self.first;
        let temp = format!("{TEMP_PREFIX}{}__", *counter);
        *counter += 1;
        emitted.push((format!("{indent}{temp} = {target}\n"), first));
        // The value as written, `?`s included — the later passes lower them
        // against the temporary's augmented assignment.
        let value_text = &body[value_span.start..last_code_end.max(value_span.end)];
        let line = format!("{indent}{temp} {op} {value_text}\n");
        let value_line = self.line_of(value_span.start);
        for (k, l) in line.split_inclusive('\n').enumerate() {
            emitted.push((l.to_owned(), value_line + k));
        }
        emitted.push((
            format!("{indent}{target} = {temp}\n"),
            self.line_of(last_code_end.saturating_sub(1)),
        ));
        Some(emitted)
    }
}

/// Every name a walrus in `stmt` binds.
fn collect_walrus_targets(stmt: &Stmt, out: &mut HashSet<String>) {
    use ruff_python_ast::visitor::{walk_expr, Visitor};
    struct V<'o>(&'o mut HashSet<String>);
    impl<'a> Visitor<'a> for V<'_> {
        fn visit_expr(&mut self, e: &'a Expr) {
            if let Expr::Named(n) = e {
                if let Expr::Name(t) = n.target.as_ref() {
                    self.0.insert(t.id.as_str().to_owned());
                }
            }
            walk_expr(self, e);
        }
    }
    V(out).visit_stmt(stmt);
}

/// The evaluation-order walk over one statement.
struct Plan<'a> {
    /// Probe offset → statement offset (the dropped head indentation).
    offset: usize,
    /// Statement-relative ranges of the inline-propagated operands.
    operands: &'a [Range<usize>],
    /// Names the operand could rebind before the statement reads them.
    rebindable: &'a HashSet<String>,
    /// Names bound by an `import`: a receiver rooted at one is a module.
    imported: &'a HashSet<String>,
    /// Expressions evaluated before a later operand, in evaluation order.
    events: Vec<Event>,
}

/// One expression the walk met before a later operand.
enum Event {
    /// Must be hoisted into a temporary.
    Hoist(Range<usize>),
    /// Exactly a propagated operand: the inline pass lifts it in place, in
    /// order — unless a hoist follows it (see `StmtSpan::rewrite`).
    Operand(Range<usize>),
}

/// The walk met a shape it does not model; leave the statement alone.
struct Unmodelled;

type Walk = Result<(), Unmodelled>;

impl Plan<'_> {
    fn range(&self, node: &impl Ranged) -> Range<usize> {
        let r = node.range();
        r.start().to_usize() + self.offset..r.end().to_usize() + self.offset
    }

    fn contains_operand(&self, node: &impl Ranged) -> bool {
        let r = self.range(node);
        self.operands
            .iter()
            .any(|o| o.start >= r.start && o.end <= r.end)
    }

    fn visit_stmt(&mut self, stmt: &Stmt) -> Walk {
        match stmt {
            Stmt::Expr(s) => self.visit_expr(&s.value),
            Stmt::Return(s) => match &s.value {
                Some(v) => self.visit_expr(v),
                None => Ok(()),
            },
            // The value is evaluated before every target.
            Stmt::Assign(s) => {
                let mut children: Vec<&Expr> = vec![&s.value];
                children.extend(s.targets.iter());
                self.visit_ordered(&children)
            }
            Stmt::AnnAssign(s) => match &s.value {
                Some(v) => self.visit_ordered(&[v, &s.target]),
                None => Ok(()),
            },
            // The target's container and index are evaluated first (its
            // load is not modelled — see the module docs).
            Stmt::AugAssign(s) => {
                let mut children: Vec<&Expr> = Vec::new();
                match s.target.as_ref() {
                    Expr::Attribute(a) => children.push(&a.value),
                    Expr::Subscript(sub) => {
                        children.push(&sub.value);
                        children.push(&sub.slice);
                    }
                    _ => {}
                }
                children.push(&s.value);
                self.visit_ordered(&children)
            }
            Stmt::If(s) => self.visit_expr(&s.test),
            Stmt::For(s) => self.visit_expr(&s.iter),
            Stmt::With(s) => {
                // A later item is evaluated after the earlier ones have been
                // *entered*; hoisting can't reproduce that. Model only an
                // operand in the first item.
                if s.items.iter().skip(1).any(|i| self.contains_operand(i)) {
                    return Err(Unmodelled);
                }
                match s.items.first() {
                    Some(item) => self.visit_expr(&item.context_expr),
                    None => Ok(()),
                }
            }
            Stmt::Assert(s) => {
                let mut children: Vec<&Expr> = vec![&s.test];
                if let Some(m) = &s.msg {
                    children.push(m);
                }
                self.visit_ordered(&children)
            }
            Stmt::Raise(s) => {
                let mut children: Vec<&Expr> = Vec::new();
                if let Some(e) = &s.exc {
                    children.push(e);
                }
                if let Some(c) = &s.cause {
                    children.push(c);
                }
                self.visit_ordered(&children)
            }
            Stmt::Delete(s) => {
                let children: Vec<&Expr> = s.targets.iter().collect();
                self.visit_ordered(&children)
            }
            _ => Err(Unmodelled),
        }
    }

    /// `children` are evaluated in this order. Everything before the last
    /// child that holds an operand is hoisted; that child is walked.
    fn visit_ordered(&mut self, children: &[&Expr]) -> Walk {
        let Some(last) = children.iter().rposition(|c| self.contains_operand(*c)) else {
            return Ok(());
        };
        for c in &children[..last] {
            self.hoist_candidate(c)?;
        }
        self.visit_expr(children[last])
    }

    fn visit_expr(&mut self, e: &Expr) -> Walk {
        if !self.contains_operand(e) {
            return Ok(());
        }
        let children = eval_children(e, |recv| self.receiver_is_child(recv))?;
        self.visit_ordered(&children)
    }

    /// Whether a method call's receiver must be evaluated (hoisted) before
    /// its arguments: anything but `super()`, a name no call can rebind, or
    /// a dotted name rooted at an imported module.
    fn receiver_is_child(&self, recv: &Expr) -> bool {
        if is_super_call(recv) {
            return false;
        }
        match recv {
            Expr::Name(n) => self.rebindable.contains(n.id.as_str()),
            _ if is_dotted_name(recv) => {
                let mut root = recv;
                while let Expr::Attribute(a) = root {
                    root = &a.value;
                }
                !matches!(root, Expr::Name(n) if self.imported.contains(n.id.as_str()))
            }
            _ => true,
        }
    }

    /// [`is_trivial`], except that a name a call could rebind is not.
    fn is_trivial(&self, e: &Expr) -> bool {
        is_trivial(e, &|name| self.rebindable.contains(name))
    }

    /// An expression evaluated before an operand: hoist it, or — for an
    /// effect-free container — the parts of it that need it.
    fn hoist_candidate(&mut self, e: &Expr) -> Walk {
        let range = self.range(e);
        if self.operands.contains(&range) {
            self.events.push(Event::Operand(range));
            return Ok(());
        }
        if self.contains_operand(e) {
            // Its own operands are ordered when the pass re-runs over the
            // hoisted statement.
            self.events.push(Event::Hoist(range));
            return Ok(());
        }
        if self.is_trivial(e) {
            return Ok(());
        }
        match e {
            Expr::List(l) => l.elts.iter().try_for_each(|x| self.hoist_candidate(x)),
            Expr::Tuple(t) => t.elts.iter().try_for_each(|x| self.hoist_candidate(x)),
            Expr::Set(s) => s.elts.iter().try_for_each(|x| self.hoist_candidate(x)),
            Expr::Dict(d) => {
                for item in &d.items {
                    if let Some(k) = &item.key {
                        self.hoist_candidate(k)?;
                    }
                    self.hoist_candidate(&item.value)?;
                }
                Ok(())
            }
            Expr::Starred(s) => self.hoist_candidate(&s.value),
            _ => {
                self.events.push(Event::Hoist(range));
                Ok(())
            }
        }
    }
}

/// The children of `e` in Python evaluation order, for an `e` that holds an
/// operand. Shapes whose operands are evaluated conditionally, lazily, or
/// inside a literal are not modelled.
fn eval_children(
    e: &Expr,
    receiver_is_child: impl Fn(&Expr) -> bool,
) -> Result<Vec<&Expr>, Unmodelled> {
    Ok(match e {
        Expr::Call(c) => {
            let mut v: Vec<&Expr> = Vec::new();
            match c.func.as_ref() {
                // `recv.method(…)`: the receiver is evaluated first (see the
                // module docs for the receivers left in place).
                Expr::Attribute(a) => {
                    if receiver_is_child(&a.value) {
                        v.push(&a.value);
                    }
                }
                other => v.push(other),
            }
            let mut args: Vec<&Expr> = c.arguments.args.iter().collect();
            args.extend(c.arguments.keywords.iter().map(|k| &k.value));
            args.sort_by_key(|a| a.range().start());
            v.extend(args);
            v
        }
        Expr::Attribute(a) => vec![&a.value],
        Expr::Subscript(s) => vec![&s.value, &s.slice],
        Expr::Slice(s) => [&s.lower, &s.upper, &s.step]
            .into_iter()
            .flatten()
            .map(|b| b.as_ref())
            .collect(),
        Expr::BinOp(b) => vec![&b.left, &b.right],
        Expr::Compare(c) => {
            let mut v: Vec<&Expr> = vec![&c.left];
            v.extend(c.comparators.iter());
            v
        }
        Expr::UnaryOp(u) => vec![&u.operand],
        Expr::Await(a) => vec![&a.value],
        Expr::Yield(y) => y.value.iter().map(|b| b.as_ref()).collect(),
        Expr::YieldFrom(y) => vec![&y.value],
        Expr::Named(n) => vec![&n.value],
        Expr::Starred(s) => vec![&s.value],
        Expr::List(l) => l.elts.iter().collect(),
        Expr::Tuple(t) => t.elts.iter().collect(),
        Expr::Set(s) => s.elts.iter().collect(),
        Expr::Dict(d) => {
            let mut v: Vec<&Expr> = Vec::new();
            for item in &d.items {
                if let Some(k) = &item.key {
                    v.push(k);
                }
                v.push(&item.value);
            }
            v
        }
        // An operand under `and`/`or`/a ternary is evaluated conditionally,
        // under a lambda or comprehension lazily or repeatedly, and inside an
        // f-string within a literal; hoisting around it is not modelled (the
        // checker rejects most of these placements anyway).
        Expr::BoolOp(_)
        | Expr::If(_)
        | Expr::Lambda(_)
        | Expr::ListComp(_)
        | Expr::SetComp(_)
        | Expr::DictComp(_)
        | Expr::Generator(_)
        | Expr::FString(_)
        | Expr::TString(_)
        | Expr::IpyEscapeCommand(_) => return Err(Unmodelled),
        // Leaves: an operand range equals the leaf itself.
        Expr::Name(_)
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_) => Vec::new(),
    })
}

/// `a`, `a.b`, `a.b.c` — names and attribute chains over names.
fn is_dotted_name(e: &Expr) -> bool {
    match e {
        Expr::Name(_) => true,
        Expr::Attribute(a) => is_dotted_name(&a.value),
        _ => false,
    }
}

/// `super()` with no arguments.
fn is_super_call(e: &Expr) -> bool {
    matches!(e, Expr::Call(c)
        if c.arguments.is_empty()
            && matches!(c.func.as_ref(), Expr::Name(n) if n.id.as_str() == "super"))
}

/// An expression whose evaluation neither has an effect nor observes one,
/// so where it runs relative to a propagated operand cannot matter.
/// `rebindable` names a call may rebind, which are not trivial.
fn is_trivial(e: &Expr, rebindable: &dyn Fn(&str) -> bool) -> bool {
    let is_trivial = |e: &Expr| is_trivial(e, rebindable);
    match e {
        Expr::Name(n) => !rebindable(n.id.as_str()),
        Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::Lambda(_) => true,
        Expr::UnaryOp(u) => matches!(u.operand.as_ref(), Expr::NumberLiteral(_)),
        Expr::List(l) => l.elts.iter().all(is_trivial),
        Expr::Tuple(t) => t.elts.iter().all(is_trivial),
        Expr::Set(s) => s.elts.iter().all(is_trivial),
        Expr::Dict(d) => d
            .items
            .iter()
            .all(|i| i.key.as_ref().is_none_or(&is_trivial) && is_trivial(&i.value)),
        Expr::Starred(s) => is_trivial(&s.value),
        Expr::Call(c) => {
            is_super_call(e)
                || (c.arguments.is_empty()
                    && matches!(c.func.as_ref(), Expr::Name(n)
                        if matches!(n.id.as_str(), "list" | "dict" | "set" | "tuple" | "frozenset")))
        }
        // A type expression (`list[int]`, `Box[str]`, `dict[str, int]`):
        // hoisting it would hide it from the checker's type-argument reading.
        Expr::Subscript(s) => match s.value.as_ref() {
            Expr::Name(n) => {
                let id = n.id.as_str();
                matches!(
                    id,
                    "list" | "dict" | "set" | "frozenset" | "tuple" | "type" | "Callable"
                ) || id.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            }
            _ => false,
        },
        _ => false,
    }
}
