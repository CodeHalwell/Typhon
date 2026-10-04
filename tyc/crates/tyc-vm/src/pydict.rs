//! The storage behind a VM `dict`: an insertion-ordered hash map with O(1)
//! deletion, laid out as CPython's `dictobject.c` is.
//!
//! `IndexMap::shift_remove` keeps order by moving every later entry down
//! one place, so `del d[k]` cost O(n) and draining a dict key by key was
//! quadratic (177× slower than CPython at 80k keys). CPython instead leaves
//! a hole in its entries array and compacts it when the array is rebuilt.
//! `PyDict` does the same: `slots` holds the entries in insertion order with
//! `None` for deleted ones, `table` maps a key's hash to its slot, and the
//! slots are compacted once holes outnumber live entries. `head` is the
//! first slot that may be live, so taking entries from the front (what
//! `OrderedDict.popitem(last=False)` does) never rescans the holes behind
//! it.
//!
//! The method names follow `IndexMap`'s, so the rest of the VM reads the
//! same; iterators that must survive mutation (a `for` loop over a dict)
//! walk slot positions through [`PyDict::next_slot`] / [`PyDict::prev_slot`],
//! exactly as CPython's dict iterator walks its entries array.
//!
//! `hashbrown::HashTable` is the one external type used, and only here.

use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};

use hashbrown::HashTable;

use crate::value::{HashKey, Value};

/// A fixed-key hasher: dict order is insertion order, so the hash only
/// places keys in the table, and a fixed one keeps collision handling (the
/// order user `__eq__` methods are consulted in) reproducible.
type Hasher = BuildHasherDefault<DefaultHasher>;

/// One stored key and value, with the key's hash (so compaction never
/// re-hashes a key).
#[derive(Clone, Debug)]
pub struct Entry {
    hash: u64,
    key: HashKey,
    value: Value,
}

#[derive(Clone, Debug, Default)]
pub struct PyDict {
    slots: Vec<Option<Entry>>,
    table: HashTable<usize>,
    live: usize,
    /// No live entry sits before this slot.
    head: usize,
}

fn hash_of(key: &HashKey) -> u64 {
    Hasher::default().hash_one(key)
}

impl PyDict {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            slots: Vec::with_capacity(n),
            table: HashTable::with_capacity(n),
            live: 0,
            head: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    pub fn clear(&mut self) {
        self.slots.clear();
        self.table.clear();
        self.live = 0;
        self.head = 0;
    }

    fn find(&self, key: &HashKey) -> Option<usize> {
        let slots = &self.slots;
        self.table
            .find(hash_of(key), |&i| {
                slots[i].as_ref().is_some_and(|e| e.key == *key)
            })
            .copied()
    }

    pub fn get(&self, key: &HashKey) -> Option<&Value> {
        let i = self.find(key)?;
        self.slots[i].as_ref().map(|e| &e.value)
    }

    pub fn get_mut(&mut self, key: &HashKey) -> Option<&mut Value> {
        let i = self.find(key)?;
        self.slots[i].as_mut().map(|e| &mut e.value)
    }

    /// The stored key equal to `key` (which may differ from it: `1` and
    /// `True`, or `1` and `1.0`, are the same key) and its value.
    pub fn get_key_value(&self, key: &HashKey) -> Option<(&HashKey, &Value)> {
        let i = self.find(key)?;
        self.slots[i].as_ref().map(|e| (&e.key, &e.value))
    }

    pub fn contains_key(&self, key: &HashKey) -> bool {
        self.find(key).is_some()
    }

    /// Insert or overwrite. An existing key keeps both its place and its
    /// original key object, as in CPython.
    pub fn insert(&mut self, key: HashKey, value: Value) -> Option<Value> {
        let hash = hash_of(&key);
        let slots = &self.slots;
        if let Some(&i) = self.table.find(hash, |&i| {
            slots[i].as_ref().is_some_and(|e| e.key == key)
        }) {
            let e = self.slots[i].as_mut().expect("table points at a live slot");
            return Some(std::mem::replace(&mut e.value, value));
        }
        self.push_new(hash, key, value);
        None
    }

    fn push_new(&mut self, hash: u64, key: HashKey, value: Value) {
        if self.slots.len() - self.live > self.live.max(8) {
            self.compact();
        }
        let i = self.slots.len();
        self.slots.push(Some(Entry { hash, key, value }));
        let slots = &self.slots;
        self.table.insert_unique(hash, i, |&j| {
            slots[j].as_ref().map_or(0, |e| e.hash)
        });
        self.live += 1;
    }

    /// `d.setdefault(key, default)`: the stored value, inserting `default`
    /// first when the key is absent.
    pub fn get_or_insert(&mut self, key: HashKey, default: Value) -> &mut Value {
        let i = match self.find(&key) {
            Some(i) => i,
            None => {
                let hash = hash_of(&key);
                self.push_new(hash, key, default);
                self.slots.len() - 1
            }
        };
        &mut self.slots[i].as_mut().expect("live slot").value
    }

    /// Remove `key`, leaving a hole where it was: O(1), and the order of
    /// the remaining entries is unchanged.
    pub fn remove(&mut self, key: &HashKey) -> Option<Value> {
        self.remove_entry(key).map(|(_, v)| v)
    }

    pub fn remove_entry(&mut self, key: &HashKey) -> Option<(HashKey, Value)> {
        let hash = hash_of(key);
        let slots = &self.slots;
        let entry = self
            .table
            .find_entry(hash, |&i| {
                slots[i].as_ref().is_some_and(|e| e.key == *key)
            })
            .ok()?;
        let (i, _) = entry.remove();
        self.take_slot(i)
    }

    fn take_slot(&mut self, i: usize) -> Option<(HashKey, Value)> {
        let e = self.slots[i].take()?;
        self.live -= 1;
        if self.live == 0 {
            self.slots.clear();
            self.head = 0;
        } else if i == self.head {
            while self.slots.get(self.head).is_some_and(Option::is_none) {
                self.head += 1;
            }
        }
        // Trailing holes are dropped at once, so `last()` stays O(1).
        while self.slots.last().is_some_and(Option::is_none) {
            self.slots.pop();
        }
        Some((e.key, e.value))
    }

    fn remove_slot(&mut self, i: usize) -> Option<(HashKey, Value)> {
        let hash = self.slots.get(i)?.as_ref()?.hash;
        if let Ok(entry) = self.table.find_entry(hash, |&j| j == i) {
            entry.remove();
        }
        self.take_slot(i)
    }

    /// Rebuild the slots without holes (CPython's resize).
    fn compact(&mut self) {
        let old = std::mem::take(&mut self.slots);
        self.slots = old.into_iter().flatten().map(Some).collect();
        self.table.clear();
        let slots = &self.slots;
        for (i, e) in slots.iter().enumerate() {
            let hash = e.as_ref().map_or(0, |e| e.hash);
            self.table
                .insert_unique(hash, i, |&j| slots[j].as_ref().map_or(0, |e| e.hash));
        }
        self.head = 0;
    }

    /// The first live entry.
    pub fn first(&self) -> Option<(&HashKey, &Value)> {
        self.next_slot(0).map(|(_, k, v)| (k, v))
    }

    /// The last live entry.
    pub fn last(&self) -> Option<(&HashKey, &Value)> {
        self.slots
            .iter()
            .rev()
            .flatten()
            .next()
            .map(|e| (&e.key, &e.value))
    }

    /// Remove and return the last entry (`dict.popitem()`).
    pub fn pop(&mut self) -> Option<(HashKey, Value)> {
        let i = self.slots.iter().rposition(Option::is_some)?;
        self.remove_slot(i)
    }

    /// Remove and return the first entry.
    pub fn pop_first(&mut self) -> Option<(HashKey, Value)> {
        let (after, _, _) = self.next_slot(0)?;
        self.remove_slot(after - 1)
    }

    /// Insert `key` as the first entry (moving it there if present).
    pub fn insert_first(&mut self, key: HashKey, value: Value) {
        self.remove(&key);
        let rest = std::mem::take(&mut self.slots);
        self.table.clear();
        self.live = 0;
        self.head = 0;
        let hash = hash_of(&key);
        self.push_new(hash, key, value);
        for e in rest.into_iter().flatten() {
            self.push_new(e.hash, e.key, e.value);
        }
    }

    /// The `n`th live entry, in order.
    pub fn get_index(&self, n: usize) -> Option<(&HashKey, &Value)> {
        if self.slots.len() == self.live {
            return self.slots.get(n)?.as_ref().map(|e| (&e.key, &e.value));
        }
        self.iter().nth(n)
    }

    /// The first live slot at or after position `pos`, with the position
    /// after it — a dict iterator's step.
    pub fn next_slot(&self, pos: usize) -> Option<(usize, &HashKey, &Value)> {
        let start = pos.max(self.head);
        self.slots
            .get(start..)?
            .iter()
            .enumerate()
            .find_map(|(off, e)| e.as_ref().map(|e| (start + off + 1, &e.key, &e.value)))
    }

    /// The last live slot before position `pos`, with its own position — a
    /// reversed dict iterator's step.
    pub fn prev_slot(&self, pos: usize) -> Option<(usize, &HashKey, &Value)> {
        let end = pos.min(self.slots.len());
        self.slots[..end]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, e)| e.as_ref().map(|e| (i, &e.key, &e.value)))
    }

    /// One past the last slot position.
    pub fn slot_end(&self) -> usize {
        self.slots.len()
    }

    pub fn iter(&self) -> Iter<'_> {
        Iter {
            slots: self.slots[self.head.min(self.slots.len())..].iter(),
            remaining: self.live,
        }
    }

    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = (&HashKey, &mut Value)> {
        self.slots.iter_mut().flatten().map(|e| (&e.key, &mut e.value))
    }

    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &HashKey> + ExactSizeIterator {
        self.iter().map(|(k, _)| k)
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &Value> + ExactSizeIterator {
        self.iter().map(|(_, v)| v)
    }

    pub fn values_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut Value> {
        self.slots.iter_mut().flatten().map(|e| &mut e.value)
    }

    /// Keep the entries `keep` accepts, in order.
    pub fn retain(&mut self, mut keep: impl FnMut(&HashKey, &mut Value) -> bool) {
        let drop: Vec<usize> = self
            .slots
            .iter_mut()
            .enumerate()
            .filter_map(|(i, e)| {
                let e = e.as_mut()?;
                (!keep(&e.key, &mut e.value)).then_some(i)
            })
            .collect();
        for i in drop {
            self.remove_slot(i);
        }
    }

    /// Every stored key whose hash equals `probe`'s, visited through the
    /// table's probe sequence (keys with a user `__hash__` share one).
    pub fn for_each_colliding_key(&self, probe: &HashKey, mut f: impl FnMut(&HashKey)) {
        let slots = &self.slots;
        let _ = self.table.find(hash_of(probe), |&i| {
            if let Some(e) = &slots[i] {
                f(&e.key);
            }
            false
        });
    }
}

/// Iterator over a dict's live entries, in order.
#[derive(Clone)]
pub struct Iter<'a> {
    slots: std::slice::Iter<'a, Option<Entry>>,
    remaining: usize,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a HashKey, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        for e in self.slots.by_ref() {
            if let Some(e) = e {
                self.remaining -= 1;
                return Some((&e.key, &e.value));
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }

    fn count(self) -> usize {
        self.remaining
    }
}

impl DoubleEndedIterator for Iter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        while let Some(e) = self.slots.next_back() {
            if let Some(e) = e {
                self.remaining -= 1;
                return Some((&e.key, &e.value));
            }
        }
        None
    }
}

impl ExactSizeIterator for Iter<'_> {}

impl<'a> IntoIterator for &'a PyDict {
    type Item = (&'a HashKey, &'a Value);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

impl IntoIterator for PyDict {
    type Item = (HashKey, Value);
    type IntoIter = std::iter::FilterMap<
        std::vec::IntoIter<Option<Entry>>,
        fn(Option<Entry>) -> Option<(HashKey, Value)>,
    >;
    fn into_iter(self) -> Self::IntoIter {
        fn live(e: Option<Entry>) -> Option<(HashKey, Value)> {
            e.map(|e| (e.key, e.value))
        }
        self.slots
            .into_iter()
            .filter_map(live as fn(Option<Entry>) -> Option<(HashKey, Value)>)
    }
}

impl FromIterator<(HashKey, Value)> for PyDict {
    fn from_iter<I: IntoIterator<Item = (HashKey, Value)>>(iter: I) -> Self {
        let mut d = PyDict::new();
        d.extend(iter);
        d
    }
}

impl Extend<(HashKey, Value)> for PyDict {
    fn extend<I: IntoIterator<Item = (HashKey, Value)>>(&mut self, iter: I) {
        for (k, v) in iter {
            self.insert(k, v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: i64) -> HashKey {
        Value::Int(n.into()).to_hash_key().ok().expect("int keys hash")
    }

    fn keys(d: &PyDict) -> Vec<String> {
        d.keys().map(|k| k.clone().into_value().py_repr()).collect()
    }

    #[test]
    fn deletion_keeps_order_and_holes_compact() {
        let mut d = PyDict::new();
        for n in 0..100 {
            d.insert(k(n), Value::None);
        }
        for n in 0..90 {
            assert!(d.remove(&k(n)).is_some());
        }
        assert_eq!(d.len(), 10);
        assert_eq!(keys(&d)[0], "90");
        assert_eq!(d.first().map(|(k, _)| k.clone().into_value().py_repr()), Some("90".into()));
        d.insert(k(5), Value::None);
        assert_eq!(keys(&d).last().cloned(), Some("5".into()));
        assert!(d.contains_key(&k(95)) && !d.contains_key(&k(3)));
        assert_eq!(d.pop_first().map(|(k, _)| k.into_value().py_repr()), Some("90".into()));
        assert_eq!(d.pop().map(|(k, _)| k.into_value().py_repr()), Some("5".into()));
        assert_eq!(d.get_index(2).map(|(k, _)| k.clone().into_value().py_repr()), Some("93".into()));
        d.insert_first(k(42), Value::None);
        assert_eq!(keys(&d)[..2], ["42".to_string(), "91".to_string()]);
        let rev: Vec<_> = d.iter().rev().map(|(k, _)| k.clone().into_value().py_repr()).collect();
        assert_eq!(rev[0], "99");
        d.retain(|k, _| k.clone().into_value().py_repr() != "42");
        assert_eq!(d.len(), 9);
    }
}
