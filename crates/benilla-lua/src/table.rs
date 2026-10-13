//! A table as 5.0's `ltable.c` lays it out: an array part and a chained scatter hash part with
//! Brent's variation, sized and rehashed by 5.0's rules. The layout is the iteration order, so
//! `next` walks keys in the order the client does (keys hashed by address excepted: a table,
//! function or userdata key is hashed by its handle here).

use crate::value::{TableRef, Value};

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub key: Value,
    pub val: Value,
    /// The next node in this chain, or -1.
    pub next: i32,
}

const EMPTY: Node = Node {
    key: Value::Nil,
    val: Value::Nil,
    next: -1,
};

/// 5.0's `MAXBITS`: `2^MAXBITS` is the largest array index kept in the array part.
const MAXBITS: u32 = 24;

#[derive(Clone, Debug)]
pub struct Table {
    pub array: Vec<Value>,
    /// The hash part: `2^lsizenode` nodes. A size of zero is 5.0's shared `dummynode`, kept
    /// here as one node of the table's own.
    pub node: Vec<Node>,
    pub lsizenode: u8,
    /// 5.0's `firstfree`: free positions are searched downward from here.
    firstfree: usize,
    pub meta: Option<TableRef>,
    /// 5.0's `flags`: a set bit caches "this metamethod is absent" (`luaT_gettm`).
    pub flags: u8,
    /// The size `table.setn` remembered, 5.0's `n`-less `getn` (the client's `setn`).
    pub setn: Option<usize>,
    /// The weak mode the last collection found: bit 0 keys, bit 1 values.
    pub(crate) weak: u8,
}

/// The result of a key lookup: where its slot is, if it has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Array(usize),
    Node(usize),
}

/// `lua_number2int` on the client: x87 `fistp` in round-to-nearest. A value that is exactly an
/// `i32` converts; anything else fails the `(lua_Number)k == n` test that follows every use.
pub fn num_to_int(n: f64) -> Option<i32> {
    let k = n as i32;
    if k as f64 == n {
        Some(k)
    } else {
        None
    }
}

/// `luaO_log2`: the floor of log2, -1 for zero.
fn log2(x: u32) -> i32 {
    if x == 0 {
        -1
    } else {
        31 - x.leading_zeros() as i32
    }
}

/// 5.0's `arrayindex`: a key that may live in the array part, or `None`.
fn arrayindex(key: Value) -> Option<usize> {
    if let Value::Num(n) = key {
        if let Some(k) = num_to_int(n) {
            if k >= 1 && (k as u32) <= (1 << MAXBITS) {
                return Some(k as usize);
            }
        }
    }
    None
}

/// `next` was given a key the table does not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidKey;

/// The error a write raises, which the VM renders with a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    Nil,
    NaN,
}

impl Table {
    /// `luaH_new`: `narray` array slots and `2^lnhash` nodes (none at 0).
    pub fn new(narray: usize, lnhash: u8) -> Table {
        let mut t = Table {
            array: Vec::new(),
            node: Vec::new(),
            lsizenode: 0,
            firstfree: 0,
            meta: None,
            flags: 0,
            setn: None,
            weak: 0,
        };
        t.array.resize(narray, Value::Nil);
        t.set_node_vector(lnhash);
        t
    }

    fn set_node_vector(&mut self, lsize: u8) {
        let size = 1usize << lsize;
        self.node.clear();
        self.node.resize(size, EMPTY);
        self.lsizenode = lsize;
        self.firstfree = size - 1;
    }

    fn sizenode(&self) -> usize {
        1 << self.lsizenode
    }

    /// `hashmod`: modulo an odd number, for numbers and pointers.
    fn hashmod(&self, h: u32) -> usize {
        (h % (((self.sizenode() - 1) as u32) | 1)) as usize
    }

    /// `luaH_mainposition`.
    fn mainposition(&self, key: Value) -> usize {
        let mask = self.sizenode() - 1;
        match key {
            Value::Num(n) => {
                // `hashnum`: `n + 1` (so -0 and 0 agree), its two 32-bit words summed.
                let bits = (n + 1.0).to_bits();
                let h = (bits as u32).wrapping_add((bits >> 32) as u32);
                self.hashmod(h)
            }
            Value::Str(s) => s.hash as usize & mask,
            Value::Bool(b) => b as usize & mask,
            Value::Table(r) => self.hashmod(r.0),
            Value::Func(r) => self.hashmod(r.0),
            Value::User(r) => self.hashmod(r.0),
            Value::Light(p) => self.hashmod(p as u32),
            Value::Nil => 0,
        }
    }

    /// `luaH_get`: the slot holding `key`, which may hold `nil`.
    pub fn find(&self, key: Value) -> Option<Slot> {
        match key {
            Value::Nil => None,
            Value::Num(n) => match num_to_int(n) {
                Some(k) => self.find_int(k),
                None => self.find_chain(key),
            },
            Value::Str(s) => {
                let mut i = s.hash as usize & (self.sizenode() - 1);
                loop {
                    let n = &self.node[i];
                    if let Value::Str(k) = n.key {
                        if k.id == s.id {
                            return Some(Slot::Node(i));
                        }
                    }
                    if n.next < 0 {
                        return None;
                    }
                    i = n.next as usize;
                }
            }
            _ => self.find_chain(key),
        }
    }

    /// `luaH_getnum`.
    pub fn find_int(&self, k: i32) -> Option<Slot> {
        if k >= 1 && (k as usize) <= self.array.len() {
            return Some(Slot::Array(k as usize - 1));
        }
        let nk = k as f64;
        let mut i = self.mainposition(Value::Num(nk));
        loop {
            let n = &self.node[i];
            if let Value::Num(x) = n.key {
                if x == nk {
                    return Some(Slot::Node(i));
                }
            }
            if n.next < 0 {
                return None;
            }
            i = n.next as usize;
        }
    }

    fn find_chain(&self, key: Value) -> Option<Slot> {
        let mut i = self.mainposition(key);
        loop {
            let n = &self.node[i];
            if n.key.raw_eq(key) {
                return Some(Slot::Node(i));
            }
            if n.next < 0 {
                return None;
            }
            i = n.next as usize;
        }
    }

    pub fn slot(&self, s: Slot) -> Value {
        match s {
            Slot::Array(i) => self.array[i],
            Slot::Node(i) => self.node[i].val,
        }
    }

    pub fn slot_mut(&mut self, s: Slot) -> &mut Value {
        match s {
            Slot::Array(i) => &mut self.array[i],
            Slot::Node(i) => &mut self.node[i].val,
        }
    }

    /// A raw read.
    pub fn get(&self, key: Value) -> Value {
        self.find(key).map_or(Value::Nil, |s| self.slot(s))
    }

    pub fn get_int(&self, k: i32) -> Value {
        self.find_int(k).map_or(Value::Nil, |s| self.slot(s))
    }

    /// `luaH_set` then the store: a raw write.
    pub fn set(&mut self, key: Value, val: Value) -> Result<(), KeyError> {
        let s = self.slot_for(key)?;
        *self.slot_mut(s) = val;
        Ok(())
    }

    pub fn set_int(&mut self, k: i32, val: Value) {
        let s = match self.find_int(k) {
            Some(s) => s,
            None => self.newkey(Value::Num(k as f64)),
        };
        *self.slot_mut(s) = val;
    }

    /// `luaH_set`: the slot for `key`, made if absent. 5.0 makes a key even to store `nil`.
    pub fn slot_for(&mut self, key: Value) -> Result<Slot, KeyError> {
        self.flags = 0;
        if let Some(s) = self.find(key) {
            // A dead node may hold a collected key whose handle `key` now reuses; C compares the
            // pointer and so reuses the node, and the key's hash must now be `key`'s own.
            if let Slot::Node(i) = s {
                self.node[i].key = key;
            }
            return Ok(s);
        }
        match key {
            Value::Nil => Err(KeyError::Nil),
            Value::Num(n) if n.is_nan() => Err(KeyError::NaN),
            _ => Ok(self.newkey(key)),
        }
    }

    /// 5.0's `newkey`: Brent's variation, then a rehash when the last free node is used.
    fn newkey(&mut self, key: Value) -> Slot {
        let mut mp = self.mainposition(key);
        if !self.node[mp].val.is_nil() {
            let othern0 = self.mainposition(self.node[mp].key);
            let n = self.firstfree;
            if othern0 != mp {
                // The colliding node is out of its main position: move it to the free node.
                let mut othern = othern0;
                while self.node[othern].next != mp as i32 {
                    othern = self.node[othern].next as usize;
                }
                self.node[othern].next = n as i32;
                self.node[n] = self.node[mp];
                self.node[mp].next = -1;
                self.node[mp].val = Value::Nil;
            } else {
                // The colliding node is in its own main position: the new key takes the free node.
                self.node[n].next = self.node[mp].next;
                self.node[mp].next = n as i32;
                mp = n;
            }
        }
        self.node[mp].key = key;
        loop {
            if self.node[self.firstfree].key.is_nil() {
                return Slot::Node(mp);
            } else if self.firstfree == 0 {
                break;
            } else {
                self.firstfree -= 1;
            }
        }
        // No free node left: keep the key alive through the rehash, then find it again.
        self.node[mp].val = Value::Bool(false);
        self.rehash();
        let s = self.find(key).expect("a rehash keeps every live key");
        *self.slot_mut(s) = Value::Nil;
        s
    }

    /// 5.0's `rehash`: `numuse`, `computesizes`, then `resize` to a hash part of
    /// `luaO_log2(nhsize) + 1`.
    fn rehash(&mut self) {
        let mut nums = [0i32; MAXBITS as usize + 1];
        let mut totaluse = 0i32;
        // The array part, slice by slice.
        let mut i = 0usize;
        let mut lg = 0usize;
        while lg <= MAXBITS as usize {
            let mut ttlg = 1usize << lg;
            if ttlg > self.array.len() {
                ttlg = self.array.len();
                if i >= ttlg {
                    break;
                }
            }
            nums[lg] = 0;
            while i < ttlg {
                if !self.array[i].is_nil() {
                    nums[lg] += 1;
                    totaluse += 1;
                }
                i += 1;
            }
            lg += 1;
        }
        let mut narray = totaluse;
        // The hash part, last node first.
        for n in self.node.iter().rev() {
            if !n.val.is_nil() {
                if let Some(k) = arrayindex(n.key) {
                    nums[(log2(k as u32 - 1) + 1) as usize] += 1;
                    narray += 1;
                }
                totaluse += 1;
            }
        }
        // `computesizes`.
        let mut nlog = 0u32;
        let mut na = 0i32;
        let mut a = nums[0];
        let mut i = 1u32;
        while i <= MAXBITS && narray >= (1 << (i - 1)) {
            if nums[i as usize] != 0 {
                a += nums[i as usize];
                if a >= (1 << (i - 1)) {
                    nlog = i;
                    na = a;
                }
            }
            i += 1;
        }
        let nhsize = totaluse - na;
        let nasize = if nlog == 0 { 0 } else { 1usize << nlog };
        self.resize(nasize, (log2(nhsize as u32) + 1) as u8);
    }

    /// 5.0's `resize`.
    fn resize(&mut self, nasize: usize, nhsize: u8) {
        let oldasize = self.array.len();
        let nold = std::mem::take(&mut self.node);
        if nasize > oldasize {
            self.array.resize(nasize, Value::Nil);
        }
        self.set_node_vector(nhsize);
        if nasize < oldasize {
            let vanishing: Vec<Value> = self.array.drain(nasize..).collect();
            for (j, v) in vanishing.into_iter().enumerate() {
                if !v.is_nil() {
                    self.set_int((nasize + j + 1) as i32, v);
                }
            }
        }
        for old in nold.iter().rev() {
            if !old.val.is_nil() {
                // A live old key cannot be nil or NaN.
                let s = self.slot_for(old.key).unwrap();
                *self.slot_mut(s) = old.val;
            }
        }
    }

    /// 5.0's `luaH_next`: the pair after `key`, `Ok(None)` at the end, `Err` for a key not in the
    /// table (`invalid key for 'next'`).
    pub fn next(&self, key: Value) -> Result<Option<(Value, Value)>, InvalidKey> {
        // `luaH_index`.
        let mut i: i64 = match key {
            Value::Nil => -1,
            _ => match arrayindex(key) {
                Some(k) if k <= self.array.len() => k as i64 - 1,
                _ => match self.find(key) {
                    Some(Slot::Node(n)) => (n + self.array.len()) as i64,
                    Some(Slot::Array(a)) => a as i64,
                    None => return Err(InvalidKey),
                },
            },
        };
        i += 1;
        while (i as usize) < self.array.len() {
            let v = self.array[i as usize];
            if !v.is_nil() {
                return Ok(Some((Value::Num((i + 1) as f64), v)));
            }
            i += 1;
        }
        let mut n = i as usize - self.array.len();
        while n < self.node.len() {
            let node = &self.node[n];
            if !node.val.is_nil() {
                return Ok(Some((node.key, node.val)));
            }
            n += 1;
        }
        Ok(None)
    }

    /// 5.0's `luaH_getn` has no such thing; this is the `getn` fallback scan the base library's
    /// `aux_getn` runs when no `n` is set: the count of the array prefix up to the first nil.
    pub fn border(&self) -> usize {
        let mut n = 0;
        while !self.get_int(n as i32 + 1).is_nil() {
            n += 1;
        }
        n
    }

    /// A byte estimate for the collector's accounting.
    pub fn bytes(&self) -> usize {
        64 + self.array.len() * 16 + self.node.len() * 40
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::StrRef;

    fn n(x: f64) -> Value {
        Value::Num(x)
    }

    #[test]
    fn integer_keys_move_to_the_array_part_on_rehash() {
        let mut t = Table::new(0, 0);
        for i in 1..=10 {
            t.set(n(i as f64), n(i as f64 * 10.0)).unwrap();
        }
        assert!(t.array.len() >= 8, "array part: {}", t.array.len());
        for i in 1..=10 {
            assert!(t.get(n(i as f64)).raw_eq(n(i as f64 * 10.0)));
        }
    }

    #[test]
    fn next_visits_every_key_once() {
        let mut t = Table::new(0, 0);
        for i in 0..200u32 {
            let k = if i % 3 == 0 {
                Value::Str(StrRef {
                    id: i,
                    hash: i.wrapping_mul(2654435761),
                })
            } else {
                n(i as f64 * 0.5)
            };
            t.set(k, n(i as f64)).unwrap();
        }
        let mut seen = 0;
        let mut k = Value::Nil;
        while let Some((nk, _)) = t.next(k).unwrap() {
            seen += 1;
            k = nk;
        }
        assert_eq!(seen, 200);
    }

    #[test]
    fn deleted_keys_stay_valid_for_next() {
        let mut t = Table::new(0, 0);
        for i in 0..20 {
            t.set(n(i as f64 + 0.5), n(1.0)).unwrap();
        }
        let mut k = Value::Nil;
        let mut count = 0;
        while let Some((nk, _)) = t.next(k).unwrap() {
            // Clearing the current key during a traversal is allowed.
            t.set(nk, Value::Nil).unwrap();
            k = nk;
            count += 1;
        }
        assert_eq!(count, 20);
    }
}
