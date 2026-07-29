//! Class linearisation — the single source of truth for method-resolution
//! order and, through it, for dataclass field order.
//!
//! This lives here, in the lowest crate every consumer already depends on,
//! rather than being re-derived per crate. Three stages need the same answer:
//!
//! * `tyc-desugar` decides the order fields are *emitted* in.
//! * `tyc-types` decides the order the constructor is *checked* against.
//! * `tyc-vm` decides the order the constructor is *run* with.
//!
//! When those three disagree, a program type-checks against one signature,
//! emits another, and runs a third. That is exactly what happened before this
//! was hoisted: desugar walked direct bases left-to-right while CPython's
//! `@dataclass` builds `__dataclass_fields__` from the *reverse* MRO, so for
//! `class C(A, B)` the emitted body read `a1, a2, b1, c1` while
//! `dataclasses.fields()` reported `b1, a1, a2, c1` — positional construction
//! silently wrote each argument into the wrong field.

use std::collections::HashMap;

/// C3 linearisation of `name`, most-derived first, exactly as CPython
/// computes `__mro__`.
///
/// `parents` maps a class name to its direct bases in declaration order. A
/// base with no entry is treated as a leaf: it takes its position in the
/// order but contributes nothing below it, which is the right model for a
/// framework base, a dotted name, or an imported class whose hierarchy is not
/// visible here.
///
/// Returns `None` when linearisation is impossible — an inconsistent
/// hierarchy, or a cycle. CPython rejects both at class-creation time, so a
/// caller should fall back to whatever conservative order it used before
/// rather than invent one for a class that cannot exist.
pub fn c3_linearise(name: &str, parents: &HashMap<String, Vec<String>>) -> Option<Vec<String>> {
    fn go(
        name: &str,
        parents: &HashMap<String, Vec<String>>,
        stack: &mut Vec<String>,
        memo: &mut HashMap<String, Option<Vec<String>>>,
    ) -> Option<Vec<String>> {
        // A class's linearisation is a function of the (immutable) parents
        // map alone, so a completed result — `Some` or `None` — is reused
        // wherever the class reappears. Without this a chain of diamonds
        // recomputes each shared base once per path through it and the walk
        // is exponential; CPython is immune for the same reason (it caches
        // per-class `__mro__`).
        if let Some(cached) = memo.get(name) {
            return cached.clone();
        }
        // A cycle is not expressible in valid Python; bail rather than hang.
        // The in-progress re-entry itself is not memoised — the enclosing
        // frames record the `None` as they unwind (a class on a cycle, or
        // above one, has no linearisation on any path).
        if stack.iter().any(|s| s == name) {
            return None;
        }
        let result = compute(name, parents, stack, memo);
        memo.insert(name.to_owned(), result.clone());
        result
    }

    fn compute(
        name: &str,
        parents: &HashMap<String, Vec<String>>,
        stack: &mut Vec<String>,
        memo: &mut HashMap<String, Option<Vec<String>>>,
    ) -> Option<Vec<String>> {
        let Some(bases) = parents.get(name) else {
            return Some(vec![name.to_owned()]);
        };
        if bases.is_empty() {
            return Some(vec![name.to_owned()]);
        }
        stack.push(name.to_owned());
        let mut sequences: Vec<Vec<String>> = Vec::with_capacity(bases.len() + 1);
        for b in bases {
            match go(b, parents, stack, memo) {
                Some(seq) => sequences.push(seq),
                None => {
                    stack.pop();
                    return None;
                }
            }
        }
        sequences.push(bases.clone());
        stack.pop();

        let mut result = vec![name.to_owned()];
        loop {
            sequences.retain(|s| !s.is_empty());
            if sequences.is_empty() {
                return Some(result);
            }
            // The C3 merge: take the head of the first sequence that appears
            // in no other sequence's tail. If every candidate is blocked, the
            // hierarchy has no consistent linearisation.
            let head = sequences.iter().find_map(|seq| {
                let candidate = &seq[0];
                let blocked = sequences.iter().any(|other| other[1..].contains(candidate));
                if blocked {
                    None
                } else {
                    Some(candidate.clone())
                }
            })?;
            result.push(head.clone());
            for seq in sequences.iter_mut() {
                if seq.first() == Some(&head) {
                    seq.remove(0);
                }
            }
        }
    }
    go(name, parents, &mut Vec::new(), &mut HashMap::new())
}

/// The ancestors of `name` in the order `@dataclass` collects fields from
/// them: reverse MRO, excluding `name` itself.
///
/// `dataclasses` walks `cls.__mro__[-1:0:-1]` updating a dict, so a field
/// first seen in a more-distant ancestor keeps that ancestor's position even
/// if a nearer class re-declares it.
///
/// Falls back to the direct bases in declaration order when linearisation
/// fails, matching the pre-C3 behaviour for hierarchies CPython would reject.
pub fn field_collection_order(name: &str, parents: &HashMap<String, Vec<String>>) -> Vec<String> {
    match c3_linearise(name, parents) {
        Some(mro) => mro[1..].iter().rev().cloned().collect(),
        None => parents.get(name).cloned().unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    (*k).to_owned(),
                    v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
                )
            })
            .collect()
    }

    #[test]
    fn single_inheritance_chain() {
        let g = graph(&[("C", &["B"]), ("B", &["A"]), ("A", &[])]);
        assert_eq!(c3_linearise("C", &g).unwrap(), ["C", "B", "A"]);
        assert_eq!(field_collection_order("C", &g), ["A", "B"]);
    }

    #[test]
    fn multiple_bases_collect_right_to_left() {
        // `class C(A, B)` — MRO is C, A, B, so fields are collected B first.
        // Walking direct bases left-to-right (the old behaviour) produced the
        // opposite order and scrambled positional construction.
        let g = graph(&[("C", &["A", "B"]), ("A", &[]), ("B", &[])]);
        assert_eq!(c3_linearise("C", &g).unwrap(), ["C", "A", "B"]);
        assert_eq!(field_collection_order("C", &g), ["B", "A"]);
    }

    #[test]
    fn diamond_matches_cpython() {
        // The canonical diamond: D(B, C), B(A), C(A).
        // CPython's MRO is D, B, C, A, object.
        let g = graph(&[("D", &["B", "C"]), ("B", &["A"]), ("C", &["A"]), ("A", &[])]);
        assert_eq!(c3_linearise("D", &g).unwrap(), ["D", "B", "C", "A"]);
        assert_eq!(field_collection_order("D", &g), ["A", "C", "B"]);
    }

    #[test]
    fn unknown_base_is_a_leaf() {
        // A framework base (`nn.Module`, an imported class) contributes a
        // position but no hierarchy of its own.
        let g = graph(&[("M", &["Module"])]);
        assert_eq!(c3_linearise("M", &g).unwrap(), ["M", "Module"]);
    }

    #[test]
    fn inconsistent_hierarchy_declines() {
        // CPython raises `TypeError: Cannot create a consistent method
        // resolution order` for this; we return `None` so the caller can fall
        // back rather than invent an order.
        let g = graph(&[("X", &["A", "B"]), ("Y", &["B", "A"]), ("Z", &["X", "Y"])]);
        assert!(c3_linearise("Z", &g).is_none());
    }

    #[test]
    fn cycle_declines_instead_of_hanging() {
        let g = graph(&[("A", &["B"]), ("B", &["A"])]);
        assert!(c3_linearise("A", &g).is_none());
    }

    #[test]
    fn deep_diamond_chain_linearises_without_exponential_blowup() {
        // D_k(A_k, B_k) with A_k and B_k both deriving D_{k-1}: without the
        // per-class memo the recursion revisits D_{k-1} once per path — 2^n
        // calls — and this test effectively hangs at n = 40 (~10^12 calls).
        // CPython accepts and linearises the same hierarchy instantly.
        let n = 40usize;
        let mut pairs: Vec<(String, Vec<String>)> = vec![("D0".to_owned(), Vec::new())];
        for k in 1..=n {
            pairs.push((format!("D{k}"), vec![format!("A{k}"), format!("B{k}")]));
            pairs.push((format!("A{k}"), vec![format!("D{}", k - 1)]));
            pairs.push((format!("B{k}"), vec![format!("D{}", k - 1)]));
        }
        let g: HashMap<String, Vec<String>> = pairs.into_iter().collect();
        let mro = c3_linearise(&format!("D{n}"), &g).expect("hierarchy is consistent");
        // MRO is D_n, A_n, B_n, D_{n-1}, A_{n-1}, B_{n-1}, …, D0.
        assert_eq!(mro.len(), 3 * n + 1);
        assert_eq!(mro[0], format!("D{n}"));
        assert_eq!(mro[1], format!("A{n}"));
        assert_eq!(mro[2], format!("B{n}"));
        assert_eq!(mro[3], format!("D{}", n - 1));
        assert_eq!(*mro.last().unwrap(), "D0");
        // And a cycle reachable from a diamond still declines (the memo must
        // not turn the in-progress cycle probe into a cached success).
        let g2 = graph(&[
            ("Top", &["L", "R"]),
            ("L", &["Loop"]),
            ("R", &["Loop"]),
            ("Loop", &["Top"]),
        ]);
        assert!(c3_linearise("Top", &g2).is_none());
    }
}
