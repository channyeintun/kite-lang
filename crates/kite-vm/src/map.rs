//! Maps: the entries in insertion order, and a hash index beside them.
//!
//! The specification calls `{K: V}` a hash map with a deterministic order,
//! and for a long time it was the order alone: a vector of entries, scanned
//! on every lookup, insert and removal, so a loop over a map of `n` entries
//! cost `n²`. The vector is still what the map *is* — iteration, `keys()`,
//! `==` and rendering read it and nothing else — and the index only says
//! where in it a key is. Nothing observable depends on the index.
//!
//! **The hash agrees with `==`.** Equal keys hash alike, which is all an
//! index needs, and several things follow from that one rule:
//!
//! - `0.0` and `-0.0` are equal, so they hash alike; `NaN` equals nothing, so
//!   its hash does not matter.
//! - Two errors are equal when they say the same thing, so an error hashes
//!   by its message alone.
//! - A struct with a `var` field can change after it is made a key: a
//!   `var` field is written through any handle to it, and the entry holds
//!   one. The scan this replaced compared what the key *is now*, so a lookup
//!   by the changed value found it. A hash of the fields would have been
//!   taken before the change, so such a struct hashes by its type alone,
//!   which no write can change. Its entries share a bucket and are found by
//!   comparing, exactly as before. A struct with no `var` field cannot
//!   change, and hashes by its fields.
//! - A key can be as deep as the program makes it, and the hash walks all of
//!   it, in a fixed order that equal keys share, with a worklist rather than
//!   recursion. Stopping early would have been cheaper per key, but keys that
//!   differ only past the stopping point — board states, long paths — would
//!   all share one bucket, and the map would be a scan again. Walking all of
//!   it costs what building the key, or comparing it once, already did. The
//!   walk ends: a key can only be cyclic through a `var` field, and a struct
//!   with one is hashed without looking inside it.
//!
//! **First match wins**, as it did in the scan. Two entries can hold equal
//! keys only when a `var` field changed one into the other, and a lookup must
//! then find the earlier. Equal keys hash alike, so the two are in one
//! bucket, and each bucket lists its entries in insertion order — an insert
//! is the newest and goes on the end, and a rebuild lays the chains out in
//! order — so the first equal key a lookup meets is the earliest.
//!
//! **A removal leaves a hole** while the map is indexed, so that it costs a
//! constant rather than moving every later entry down. The holes are skipped
//! by everything that reads the entries, so the order of what remains is the
//! order it was: exactly what moving the entries down gave. When the holes
//! outnumber the entries, the vector is closed up and the index rebuilt.
//!
//! A map of a few entries has no index at all: below [`SMALL`] a scan is as
//! fast as hashing, and the entries stay packed.

use crate::Value;

/// The most entries a map holds before it is given an index.
const SMALL: usize = 8;

const NONE: u32 = u32::MAX;

#[derive(Clone, Debug, Default)]
pub struct MapValue {
    /// The entries in insertion order. `None` is a removed one, and only
    /// appears while the map is indexed.
    slots: Vec<Option<(Value, Value)>>,
    /// How many of `slots` are entries.
    live: usize,
    index: Option<Index>,
}

#[derive(Clone, Debug)]
struct Index {
    /// Per bucket (a power of two of them), the first slot whose key hashes
    /// there, or [`NONE`].
    heads: Vec<u32>,
    /// Per slot, the next slot in its bucket — always a later one.
    next: Vec<u32>,
    /// Per slot, its key's hash.
    hashes: Vec<u64>,
}

/// What hashing a key needs to know about the program: which structs have a
/// `var` field, by struct id.
#[derive(Clone, Copy)]
pub struct Keys<'a> {
    pub mutable_structs: &'a [bool],
}

impl MapValue {
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// The entries, in insertion order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &(Value, Value)> {
        self.slots.iter().flatten()
    }

    /// The entries, given up, in insertion order.
    pub fn into_entries(self) -> impl Iterator<Item = (Value, Value)> {
        self.slots.into_iter().flatten()
    }

    pub fn get(&self, key: &Value, keys: Keys<'_>) -> Option<&Value> {
        let at = self.find(key, keys)?;
        self.slots[at].as_ref().map(|(_, v)| v)
    }

    /// The slot holding `key`: the earliest, should two hold equal keys.
    pub fn find(&self, key: &Value, keys: Keys<'_>) -> Option<usize> {
        match &self.index {
            None => self
                .slots
                .iter()
                .position(|e| matches!(e, Some((k, _)) if k == key)),
            Some(index) => index.locate(&self.slots, key, hash(key, keys)).0,
        }
    }

    /// `m[key] = value`: an existing key keeps its position and its stored
    /// key, which matters where equal keys differ (`-0.0` and `0.0`); a new
    /// one goes on the end.
    pub fn set(&mut self, key: Value, value: Value, keys: Keys<'_>) {
        let Some(index) = &mut self.index else {
            match self.slots.iter_mut().flatten().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 = value,
                None => {
                    self.slots.push(Some((key, value)));
                    self.live += 1;
                    if self.slots.len() > SMALL {
                        self.reindex(keys);
                    }
                }
            }
            return;
        };
        let h = hash(&key, keys);
        let (found, last) = index.locate(&self.slots, &key, h);
        if let Some(at) = found {
            if let Some(entry) = &mut self.slots[at] {
                entry.1 = value;
            }
            return;
        }
        let at = self.slots.len() as u32;
        index.next.push(NONE);
        index.hashes.push(h);
        match last {
            NONE => {
                let b = bucket(h, index.heads.len());
                index.heads[b] = at;
            }
            tail => index.next[tail as usize] = at,
        }
        self.slots.push(Some((key, value)));
        self.live += 1;
        if self.slots.len() > index.heads.len() {
            let buckets = 2 * index.heads.len();
            index.rebuild(&self.slots, buckets);
        }
    }

    /// Take out the entry in slot `at`, which [`Self::find`] answered.
    pub fn remove_at(&mut self, at: usize) {
        let Some(index) = &mut self.index else {
            self.slots.remove(at);
            self.live -= 1;
            return;
        };
        let b = bucket(index.hashes[at], index.heads.len());
        let after = index.next[at];
        if index.heads[b] == at as u32 {
            index.heads[b] = after;
        } else {
            let mut i = index.heads[b];
            while index.next[i as usize] != at as u32 {
                i = index.next[i as usize];
            }
            index.next[i as usize] = after;
        }
        index.next[at] = NONE;
        self.slots[at] = None;
        self.live -= 1;
        let holes = self.slots.len() - self.live;
        if holes > SMALL && holes > self.live {
            self.close_up();
        }
    }

    /// Drop the holes, and index what is left if it is not small.
    fn close_up(&mut self) {
        let hashes = self.index.take().map(|i| i.hashes);
        let slots = std::mem::take(&mut self.slots);
        let mut kept_hashes = Vec::with_capacity(self.live);
        for (i, e) in slots.into_iter().enumerate() {
            if e.is_some() {
                if let Some(h) = &hashes {
                    kept_hashes.push(h[i]);
                }
                self.slots.push(e);
            }
        }
        if self.slots.len() > SMALL {
            let mut index = Index { heads: Vec::new(), next: Vec::new(), hashes: kept_hashes };
            index.rebuild(&self.slots, buckets_for(self.slots.len()));
            self.index = Some(index);
        }
    }

    /// Index a map that has outgrown a scan.
    fn reindex(&mut self, keys: Keys<'_>) {
        let hashes = self
            .slots
            .iter()
            .map(|e| e.as_ref().map_or(0, |(k, _)| hash(k, keys)))
            .collect();
        let mut index = Index { heads: Vec::new(), next: Vec::new(), hashes };
        index.rebuild(&self.slots, buckets_for(self.slots.len()));
        self.index = Some(index);
    }
}

impl Index {
    /// Walk `key`'s bucket: the slot holding it, if any, and the bucket's
    /// last slot, which is where a new entry is linked.
    fn locate(&self, slots: &[Option<(Value, Value)>], key: &Value, h: u64) -> (Option<usize>, u32) {
        let mut i = self.heads[bucket(h, self.heads.len())];
        let mut last = NONE;
        while i != NONE {
            let at = i as usize;
            if self.hashes[at] == h {
                if let Some((k, _)) = &slots[at] {
                    if k == key {
                        return (Some(at), last);
                    }
                }
            }
            last = i;
            i = self.next[at];
        }
        (None, last)
    }

    /// Lay the chains out afresh over `buckets` buckets. Walking the slots
    /// from the back and putting each at the front of its bucket leaves every
    /// bucket in insertion order.
    fn rebuild(&mut self, slots: &[Option<(Value, Value)>], buckets: usize) {
        self.heads = vec![NONE; buckets];
        self.next = vec![NONE; slots.len()];
        for at in (0..slots.len()).rev() {
            if slots[at].is_some() {
                let b = bucket(self.hashes[at], buckets);
                self.next[at] = self.heads[b];
                self.heads[b] = at as u32;
            }
        }
    }
}

/// Room for `n` entries with as many buckets as entries at most.
fn buckets_for(n: usize) -> usize {
    (2 * n).next_power_of_two()
}

#[inline]
fn bucket(h: u64, buckets: usize) -> usize {
    h as usize & (buckets - 1)
}

/// A key's hash: equal keys (by `==`) hash alike. See the module comment.
pub fn hash(key: &Value, keys: Keys<'_>) -> u64 {
    let mut h = Hasher(0);
    // A scalar key — the usual kind — is hashed without a worklist.
    if !h.scalar(key) {
        let mut work: Vec<Value> = vec![key.clone()];
        while let Some(v) = work.pop() {
            if h.scalar(&v) {
                continue;
            }
            // Children go on in reverse so they are hashed in order; the
            // order only has to be the same for equal keys, and it is.
            match &v {
                Value::Struct(s) => {
                    h.word(5);
                    h.word(s.struct_id as u64);
                    let fixed = !keys
                        .mutable_structs
                        .get(s.struct_id as usize)
                        .copied()
                        .unwrap_or(true);
                    if fixed {
                        work.extend(s.fields.borrow().iter().rev().cloned());
                    }
                }
                Value::Enum(e) => {
                    h.word(6);
                    h.word(e.enum_id as u64);
                    h.word(e.variant as u64);
                    work.extend(e.fields.iter().rev().cloned());
                }
                Value::Slice(items) | Value::Tuple(items) => {
                    h.word(7);
                    h.word(items.len() as u64);
                    work.extend(items.iter().rev().cloned());
                }
                Value::Map(m) => {
                    h.word(8);
                    h.word(m.len() as u64);
                    for (k, v) in m.iter().rev() {
                        work.push(v.clone());
                        work.push(k.clone());
                    }
                }
                Value::Pair(p) => {
                    h.word(9);
                    work.push(p.1.clone());
                    work.push(p.0.clone());
                }
                _ => {}
            }
        }
    }
    h.finish()
}

/// FxHash's step with a finishing mix, so the low bits a bucket is chosen by
/// depend on all of the input.
struct Hasher(u64);

impl Hasher {
    #[inline]
    fn word(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn bytes(&mut self, b: &[u8]) {
        self.word(b.len() as u64);
        let mut chunks = b.chunks_exact(8);
        for c in &mut chunks {
            self.word(u64::from_le_bytes(c.try_into().unwrap()));
        }
        let mut tail = [0u8; 8];
        let rest = chunks.remainder();
        tail[..rest.len()].copy_from_slice(rest);
        self.word(u64::from_le_bytes(tail));
    }

    /// Hash `v` if it has no parts, answering whether it had none.
    #[inline]
    fn scalar(&mut self, v: &Value) -> bool {
        match v {
            Value::Unit => self.word(0),
            Value::Nil => self.word(1),
            Value::PresentNil => self.word(5),
            Value::Int(i) => self.word(*i as u64),
            Value::Bool(b) => self.word(2 + *b as u64),
            // `-0.0 == 0.0`, so both hash as `0.0`.
            Value::Float(f) => self.word(if *f == 0.0 { 0 } else { f.to_bits() }),
            Value::Str(s) => self.bytes(s.as_bytes()),
            Value::Err(e) => self.bytes(e.message.as_bytes()),
            // No `==` reaches a closure as a key; the checker refuses one.
            Value::Closure(_) => self.word(4),
            _ => return false,
        }
        true
    }

    fn finish(&self) -> u64 {
        let mut x = self.0;
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        x ^ (x >> 33)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StructValue;
    use std::cell::RefCell;
    use std::rc::Rc;

    const KEYS: Keys<'static> = Keys { mutable_structs: &[false, true] };

    fn record(id: u32, n: i64) -> Value {
        Value::Struct(Rc::new(StructValue { struct_id: id, fields: RefCell::new(vec![Value::Int(n)]) }))
    }

    #[test]
    fn equal_keys_hash_alike() {
        assert_eq!(hash(&Value::Float(0.0), KEYS), hash(&Value::Float(-0.0), KEYS));
        let a = Value::Tuple(Rc::new(vec![Value::Int(1), Value::Str("é".into())]));
        let b = Value::Tuple(Rc::new(vec![Value::Int(1), Value::Str("é".into())]));
        assert_eq!(hash(&a, KEYS), hash(&b, KEYS));
        assert_eq!(hash(&record(0, 3), KEYS), hash(&record(0, 3), KEYS));
        assert_ne!(hash(&record(0, 3), KEYS), hash(&record(0, 4), KEYS));
    }

    /// A struct with a `var` field hashes by its type, so changing the field
    /// cannot move it out of the bucket a lookup searches.
    #[test]
    fn a_struct_that_can_change_hashes_by_its_type() {
        let key = record(1, 3);
        let before = hash(&key, KEYS);
        if let Value::Struct(s) = &key {
            s.fields.borrow_mut()[0] = Value::Int(4);
        }
        assert_eq!(hash(&key, KEYS), before);
    }

    /// Holes are skipped, the order of what is left is kept, and the map
    /// closes up once the holes outnumber the entries.
    #[test]
    fn removal_keeps_order_and_closes_up() {
        let mut m = MapValue::default();
        for i in 0..40 {
            m.set(Value::Int(i), Value::Int(i), KEYS);
        }
        let gone: Vec<i64> = (0..25).map(|j| j * 7 % 40).collect();
        for k in &gone {
            let at = m.find(&Value::Int(*k), KEYS).unwrap();
            m.remove_at(at);
        }
        assert_eq!(m.len(), 15);
        // Closed up after the twenty-first, with four holes since.
        assert_eq!(m.slots.len(), 19, "closed up");
        let want: Vec<i64> = (0..40).filter(|i| !gone.contains(i)).collect();
        let left: Vec<i64> = m
            .iter()
            .map(|(k, _)| match k {
                Value::Int(i) => *i,
                _ => -1,
            })
            .collect();
        assert_eq!(left, want);
        for i in 0..40 {
            assert_eq!(m.find(&Value::Int(i), KEYS).is_some(), want.contains(&i));
        }
    }
}
