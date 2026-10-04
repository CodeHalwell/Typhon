//! A set with CPython's iteration order.
//!
//! CPython's `set` is an open-addressing table (`Objects/setobject.c`):
//! linear probes of nine slots, then perturbed probing; a table that grows
//! to four times the live count when it is three-fifths full; deletions
//! leave dummies. Iteration walks the table in slot order. Because the hash
//! of an `int` (and an integral `float`) is its value, the order a program
//! sees for `{-1, 0, 1}` — `{0, 1, -1}` — is deterministic on CPython, and
//! programs depend on it without knowing. A Rust `HashSet` with a random
//! seed gave the VM a different order on every run (and a sorted `repr`
//! that disagreed with its own iteration).
//!
//! [`PySet`] reproduces that table slot for slot, with the hashes of
//! [`key_hash`] (`pyhash`'s CPython algorithms — `str` included, under the
//! `PYTHONHASHSEED=0` key, so string sets match a `PYTHONHASHSEED=0`
//! CPython too). Every operation that builds or edits a set follows the
//! matching CPython routine — `set_add_entry`, `set_insert_clean`,
//! `set_table_resize`, `set_merge`, `set_discard_entry`, `set.pop`'s finger
//! — so the order of the result is CPython's.

use crate::value::HashKey;

const MIN_SIZE: usize = 8;
const LINEAR_PROBES: usize = 9;
const PERTURB_SHIFT: u32 = 5;

#[derive(Clone, Debug)]
enum Slot {
    Empty,
    Dummy,
    Active(i64, HashKey),
}

/// CPython's `hash()` of a set member, from the key alone.
pub fn key_hash(k: &HashKey) -> i64 {
    use crate::pyhash;
    match k {
        HashKey::None => pyhash::NONE,
        HashKey::Bool(b) => *b as i64,
        HashKey::Int(i) => match i.to_i64() {
            Some(n) => pyhash::small_int_hash(n),
            None => pyhash::int_hash(&i.to_bigint()),
        },
        HashKey::Float(bits) => pyhash::float_hash(f64::from_bits(*bits)),
        // CPython 3.10+: a NaN hashes by object identity.
        HashKey::NaN(id) => pyhash::pointer_hash(*id as usize),
        HashKey::Complex(re, im) => pyhash::complex_hash(f64::from_bits(*re), f64::from_bits(*im)),
        HashKey::Str(s) => pyhash::str_hash(s),
        HashKey::Tuple(items) => {
            let hs: Vec<i64> = items.iter().map(key_hash).collect();
            pyhash::tuple_hash(&hs)
        }
        HashKey::FrozenSet(items) => {
            let hs: Vec<i64> = items.iter().map(key_hash).collect();
            pyhash::frozenset_hash(&hs)
        }
        // A frozen dataclass hashes as the tuple of its fields in
        // declaration order (the key stores them sorted by name).
        HashKey::Instance { instance, key } => {
            let hs: Vec<i64> = instance
                .class
                .fields
                .iter()
                .filter_map(|f| key.fields.iter().find(|(n, _)| *n == f.name))
                .map(|(_, v)| key_hash(v))
                .collect();
            pyhash::tuple_hash(&hs)
        }
        HashKey::Identity(inst) => pyhash::pointer_hash(std::rc::Rc::as_ptr(inst) as usize),
        HashKey::Class(c) => pyhash::pointer_hash(std::rc::Rc::as_ptr(c) as usize),
        HashKey::BuiltinType(name) => pyhash::str_hash(name),
        HashKey::Mixin { value, .. } => key_hash(value),
        HashKey::UserHashed { hash, .. } => {
            if *hash == -1 {
                -2
            } else {
                *hash
            }
        }
    }
}

/// See the module docs.
#[derive(Clone, Debug)]
pub struct PySet {
    table: Vec<Slot>,
    /// Active plus dummy slots.
    fill: usize,
    /// Active slots.
    used: usize,
    /// Where `pop()` resumes its scan.
    finger: usize,
}

impl Default for PySet {
    fn default() -> Self {
        Self::new()
    }
}

impl PySet {
    pub fn new() -> Self {
        PySet {
            table: vec![Slot::Empty; MIN_SIZE],
            fill: 0,
            used: 0,
            finger: 0,
        }
    }

    /// An empty set; the capacity hint is ignored (CPython starts every set
    /// at the minimum table and grows it as it fills).
    pub fn with_capacity(_n: usize) -> Self {
        Self::new()
    }

    fn mask(&self) -> usize {
        self.table.len() - 1
    }

    pub fn len(&self) -> usize {
        self.used
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    /// Members in table (iteration) order.
    pub fn iter(&self) -> impl Iterator<Item = &HashKey> + '_ {
        self.table.iter().filter_map(|s| match s {
            Slot::Active(_, k) => Some(k),
            _ => None,
        })
    }

    fn entries(&self) -> impl Iterator<Item = (i64, &HashKey)> + '_ {
        self.table.iter().filter_map(|s| match s {
            Slot::Active(h, k) => Some((*h, k)),
            _ => None,
        })
    }

    /// The slot holding `key`, if present (`set_lookkey`).
    fn find(&self, key: &HashKey, hash: i64) -> Option<usize> {
        let mask = self.mask();
        let mut perturb = hash as u64;
        let mut i = (hash as u64 as usize) & mask;
        loop {
            let probes = if i + LINEAR_PROBES <= mask { LINEAR_PROBES } else { 0 };
            for j in 0..=probes {
                match &self.table[i + j] {
                    Slot::Empty => return None,
                    Slot::Active(h, k) if *h == hash && k == key => return Some(i + j),
                    _ => {}
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize)) & mask;
        }
    }

    pub fn contains(&self, key: &HashKey) -> bool {
        self.find(key, key_hash(key)).is_some()
    }

    /// The stored member equal to `key` (which may differ from `key` in
    /// type: `1`, `1.0` and `True` are one member).
    pub fn get(&self, key: &HashKey) -> Option<&HashKey> {
        self.find(key, key_hash(key)).map(|i| match &self.table[i] {
            Slot::Active(_, k) => k,
            _ => unreachable!(),
        })
    }

    /// `set.add` (`set_add_entry`). Returns whether the key was new.
    pub fn insert(&mut self, key: HashKey) -> bool {
        let hash = key_hash(&key);
        self.insert_hashed(key, hash)
    }

    fn insert_hashed(&mut self, key: HashKey, hash: i64) -> bool {
        let mask = self.mask();
        let mut perturb = hash as u64;
        let mut i = (hash as u64 as usize) & mask;
        let mut freeslot: Option<usize> = None;
        let unused = 'probe: loop {
            let probes = if i + LINEAR_PROBES <= mask { LINEAR_PROBES } else { 0 };
            for j in 0..=probes {
                match &self.table[i + j] {
                    Slot::Empty => break 'probe i + j,
                    Slot::Active(h, k) => {
                        if *h == hash && *k == key {
                            return false;
                        }
                    }
                    // CPython 3.13 remembers the *last* dummy on the path.
                    Slot::Dummy => freeslot = Some(i + j),
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize)) & mask;
        };
        if let Some(slot) = freeslot {
            self.used += 1;
            self.table[slot] = Slot::Active(hash, key);
            return true;
        }
        self.fill += 1;
        self.used += 1;
        self.table[unused] = Slot::Active(hash, key);
        if self.fill * 5 >= mask * 3 {
            let minused = if self.used > 50000 { self.used * 2 } else { self.used * 4 };
            self.resize(minused);
        }
        true
    }

    /// `set_insert_clean`: place a key known to be absent into a table with
    /// no dummies.
    fn insert_clean(table: &mut [Slot], key: HashKey, hash: i64) {
        let mask = table.len() - 1;
        let mut perturb = hash as u64;
        let mut i = (hash as u64 as usize) & mask;
        loop {
            if matches!(table[i], Slot::Empty) {
                table[i] = Slot::Active(hash, key);
                return;
            }
            if i + LINEAR_PROBES <= mask {
                for j in 1..=LINEAR_PROBES {
                    if matches!(table[i + j], Slot::Empty) {
                        table[i + j] = Slot::Active(hash, key);
                        return;
                    }
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = (i.wrapping_mul(5).wrapping_add(1).wrapping_add(perturb as usize)) & mask;
        }
    }

    /// `set_table_resize`: the smallest table larger than `minused`,
    /// re-inserted in old slot order without dummies.
    fn resize(&mut self, minused: usize) {
        let mut newsize = MIN_SIZE;
        while newsize <= minused {
            newsize <<= 1;
        }
        if newsize == MIN_SIZE && self.table.len() == MIN_SIZE && self.fill == self.used {
            return;
        }
        let old = std::mem::replace(&mut self.table, vec![Slot::Empty; newsize]);
        self.fill = self.used;
        for slot in old {
            if let Slot::Active(h, k) = slot {
                Self::insert_clean(&mut self.table, k, h);
            }
        }
    }

    /// `set_discard_entry`. Returns whether the key was present.
    pub fn remove(&mut self, key: &HashKey) -> bool {
        let hash = key_hash(key);
        match self.find(key, hash) {
            Some(i) => {
                self.table[i] = Slot::Dummy;
                self.used -= 1;
                true
            }
            None => false,
        }
    }

    /// `set.pop`: the first member at or after the finger.
    pub fn pop(&mut self) -> Option<HashKey> {
        if self.used == 0 {
            return None;
        }
        let mask = self.mask();
        let mut i = self.finger & mask;
        while !matches!(self.table[i], Slot::Active(..)) {
            i += 1;
            if i > mask {
                i = 0;
            }
        }
        let Slot::Active(_, key) = std::mem::replace(&mut self.table[i], Slot::Dummy) else {
            unreachable!()
        };
        self.used -= 1;
        self.finger = i + 1;
        Some(key)
    }

    /// `set.clear`: back to an empty minimum-size table.
    pub fn clear(&mut self) {
        let finger = self.finger;
        *self = Self::new();
        self.finger = finger;
    }

    /// `set_merge`: add every member of `other` (`set.update(other_set)`,
    /// `set(other_set)`, `a | b`).
    pub fn merge(&mut self, other: &PySet) {
        if other.used == 0 {
            return;
        }
        if (self.fill + other.used) * 5 >= self.mask() * 3 {
            self.resize((self.used + other.used) * 2);
        }
        if self.fill == 0 && self.mask() == other.mask() && other.fill == other.used {
            self.table = other.table.clone();
            self.fill = other.fill;
            self.used = other.used;
            return;
        }
        if self.fill == 0 {
            self.fill = other.used;
            self.used = other.used;
            for (h, k) in other.entries() {
                Self::insert_clean(&mut self.table, k.clone(), h);
            }
            return;
        }
        for (h, k) in other.entries() {
            self.insert_hashed(k.clone(), h);
        }
    }

    /// `set_update_dict`: add a dict's keys, resizing once up front.
    pub fn update_from_dict_keys<'a>(&mut self, keys: impl ExactSizeIterator<Item = &'a HashKey>) {
        let n = keys.len();
        if (self.fill + n) * 5 >= self.mask() * 3 {
            self.resize((self.used + n) * 2);
        }
        for k in keys {
            self.insert(k.clone());
        }
    }

    /// `set.copy` / `make_new_set(set)`.
    pub fn copy(&self) -> PySet {
        let mut out = PySet::new();
        out.merge(self);
        out
    }

    /// `set_intersection` against another set: iterate the smaller one.
    pub fn intersection(&self, other: &PySet) -> PySet {
        let (big, small) = if other.len() > self.len() {
            (other, self)
        } else {
            (self, other)
        };
        let mut out = PySet::new();
        for (h, k) in small.entries() {
            if big.find(k, h).is_some() {
                out.insert_hashed(k.clone(), h);
            }
        }
        out
    }

    /// `set_intersection` against a non-set iterable's keys, in order,
    /// stopping once the result is as large as `self`.
    pub fn intersection_keys(&self, keys: impl IntoIterator<Item = HashKey>) -> PySet {
        let mut out = PySet::new();
        for k in keys {
            let h = key_hash(&k);
            if self.find(&k, h).is_some() {
                out.insert_hashed(k, h);
                if out.len() >= self.len() {
                    break;
                }
            }
        }
        out
    }

    /// `set_difference` against another set (or a dict's key set).
    pub fn difference(&self, other: &PySet) -> PySet {
        if (self.len() >> 2) > other.len() {
            let mut out = self.copy();
            out.difference_update(other);
            return out;
        }
        let mut out = PySet::new();
        for (h, k) in self.entries() {
            if other.find(k, h).is_none() {
                out.insert_hashed(k.clone(), h);
            }
        }
        out
    }

    /// `set_difference_update_internal` with a set operand.
    pub fn difference_update(&mut self, other: &PySet) {
        let narrowed;
        let other = if (other.len() >> 3) > self.len() {
            narrowed = self.intersection(other);
            &narrowed
        } else {
            other
        };
        let keys: Vec<HashKey> = other.iter().cloned().collect();
        for k in &keys {
            self.remove(k);
        }
        self.purge_dummies();
    }

    /// `set_difference_update_internal` with a non-set iterable's keys.
    pub fn difference_update_keys(&mut self, keys: impl IntoIterator<Item = HashKey>) {
        for k in keys {
            self.remove(&k);
        }
        self.purge_dummies();
    }

    /// "If more than 1/4th are dummies, then resize them away."
    fn purge_dummies(&mut self) {
        if self.fill - self.used > self.mask() / 4 {
            let minused = if self.used > 50000 { self.used * 2 } else { self.used * 4 };
            self.resize(minused);
        }
    }

    /// `set_symmetric_difference_update_set`.
    pub fn symmetric_difference_update(&mut self, other: &PySet) {
        for (h, k) in other.entries() {
            match self.find(k, h) {
                Some(i) => {
                    self.table[i] = Slot::Dummy;
                    self.used -= 1;
                }
                None => {
                    self.insert_hashed(k.clone(), h);
                }
            }
        }
    }

    /// `set_symmetric_difference`: a copy of `other` updated with `self`.
    pub fn symmetric_difference(&self, other: &PySet) -> PySet {
        let mut out = PySet::new();
        out.merge(other);
        out.symmetric_difference_update(self);
        out
    }

    pub fn is_subset(&self, other: &PySet) -> bool {
        self.len() <= other.len() && self.entries().all(|(h, k)| other.find(k, h).is_some())
    }

    pub fn is_superset(&self, other: &PySet) -> bool {
        other.is_subset(self)
    }

    pub fn is_disjoint(&self, other: &PySet) -> bool {
        let (big, small) = if other.len() > self.len() {
            (other, self)
        } else {
            (self, other)
        };
        small.entries().all(|(h, k)| big.find(k, h).is_none())
    }

    /// Keep the members `f` accepts (removals leave dummies, as `discard`
    /// would).
    pub fn retain(&mut self, mut f: impl FnMut(&HashKey) -> bool) {
        for i in 0..self.table.len() {
            if let Slot::Active(_, k) = &self.table[i] {
                if !f(k) {
                    self.table[i] = Slot::Dummy;
                    self.used -= 1;
                }
            }
        }
    }
}

impl PartialEq for PySet {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.is_subset(other)
    }
}
impl Eq for PySet {}

impl Extend<HashKey> for PySet {
    fn extend<I: IntoIterator<Item = HashKey>>(&mut self, iter: I) {
        for k in iter {
            self.insert(k);
        }
    }
}

impl FromIterator<HashKey> for PySet {
    fn from_iter<I: IntoIterator<Item = HashKey>>(iter: I) -> Self {
        let mut s = PySet::new();
        s.extend(iter);
        s
    }
}

impl IntoIterator for PySet {
    type Item = HashKey;
    type IntoIter = std::vec::IntoIter<HashKey>;
    fn into_iter(self) -> Self::IntoIter {
        self.table
            .into_iter()
            .filter_map(|s| match s {
                Slot::Active(_, k) => Some(k),
                _ => None,
            })
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl<'a> IntoIterator for &'a PySet {
    type Item = &'a HashKey;
    type IntoIter = Box<dyn Iterator<Item = &'a HashKey> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmInt;

    fn ints(s: &PySet) -> Vec<i64> {
        s.iter()
            .map(|k| match k {
                HashKey::Int(i) => i.to_i64().unwrap(),
                _ => panic!(),
            })
            .collect()
    }

    #[test]
    fn insertion_order_matches_cpython_tables() {
        // CPython 3.13: list({5, 3, 1, 100, 33, 2}) is built by adding in
        // order; set([5, 3, 1, 100, 33, 2]) == [1, 33, 3, 100, 5, 2].
        let s: PySet = [5, 3, 1, 100, 33, 2]
            .into_iter()
            .map(|n| HashKey::Int(VmInt::from(n)))
            .collect();
        assert_eq!(ints(&s), vec![1, 33, 3, 100, 5, 2]);
        let s: PySet = [-1, 0, 1].into_iter().map(|n| HashKey::Int(VmInt::from(n))).collect();
        assert_eq!(ints(&s), vec![0, 1, -1]);
    }
}
