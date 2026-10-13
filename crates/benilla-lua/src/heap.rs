//! The heap: every collectable in a typed arena, addressed by index. Cloning a `Heap` is a
//! snapshot of every object; there is no pointer anywhere to fix up.

use std::rc::Rc;

use crate::proto::Proto;
use crate::table::Table;
use crate::value::{FuncRef, StrRef, TableRef, UserRef, Value, STR_HASH_MASK, STR_ID_BITS, V};

/// A native function. Its arguments are the `nargs` values below the top of the stack; it pushes
/// its results and answers how many.
pub type NativeFn = fn(&mut crate::vm::Vm, usize) -> Result<usize, crate::vm::LuaError>;

#[derive(Clone, Debug)]
pub struct StrObj {
    pub bytes: Box<[u8]>,
    pub hash: u32,
    /// The next string in this intern bucket, or `u32::MAX`.
    next: u32,
}

#[derive(Clone)]
pub enum FuncObj {
    Lua {
        proto: Rc<Proto>,
        upvals: Box<[u32]>,
        env: TableRef,
    },
    Native {
        f: NativeFn,
        name: &'static str,
        upvals: Box<[Value]>,
        env: TableRef,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum UpvalObj {
    /// Still a register: the absolute stack slot.
    Open(u32),
    Closed(Value),
}

#[derive(Clone, Debug)]
pub struct UserObj {
    pub meta: Option<TableRef>,
    /// The host's own handle for what this userdata stands for.
    pub tag: u64,
}

#[derive(Clone)]
struct Arena<T> {
    items: Vec<Option<T>>,
    marks: Vec<bool>,
    free: Vec<u32>,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Arena {
            items: Vec::new(),
            marks: Vec::new(),
            free: Vec::new(),
        }
    }
}

impl<T> Arena<T> {
    fn alloc(&mut self, v: T) -> u32 {
        if let Some(i) = self.free.pop() {
            self.items[i as usize] = Some(v);
            self.marks[i as usize] = false;
            i
        } else {
            self.items.push(Some(v));
            self.marks.push(false);
            (self.items.len() - 1) as u32
        }
    }

    #[inline]
    fn get(&self, i: u32) -> &T {
        debug_assert!(
            self.items.get(i as usize).is_some_and(Option::is_some),
            "a live handle"
        );
        // SAFETY: a handle the VM dereferences is reachable, and the collector frees nothing
        // reachable; a stale handle (an uncleared register) is only ever read by the
        // collector, which checks it first (`Heap::is_live`).
        unsafe {
            self.items
                .get_unchecked(i as usize)
                .as_ref()
                .unwrap_unchecked()
        }
    }

    #[inline]
    fn get_mut(&mut self, i: u32) -> &mut T {
        debug_assert!(
            self.items.get(i as usize).is_some_and(Option::is_some),
            "a live handle"
        );
        // SAFETY: as `get`.
        unsafe {
            self.items
                .get_unchecked_mut(i as usize)
                .as_mut()
                .unwrap_unchecked()
        }
    }

    fn live(&self) -> usize {
        self.items.len() - self.free.len()
    }
}

/// 5.0's string hash (`luaS_newlstr`): seeded with the length, sampling at most ~32 bytes.
pub fn str_hash(s: &[u8]) -> u32 {
    let l = s.len();
    let mut h = l as u32;
    let step = (l >> 5) + 1;
    let mut l1 = l;
    while l1 >= step {
        h ^= (h << 5).wrapping_add(h >> 2).wrapping_add(s[l1 - 1] as u32);
        l1 -= step;
    }
    h
}

#[derive(Clone)]
pub struct Heap {
    strs: Arena<StrObj>,
    /// The intern table's buckets: the first string of each chain.
    strt: Vec<u32>,
    tables: Arena<Table>,
    funcs: Arena<FuncObj>,
    upvals: Arena<UpvalObj>,
    users: Arena<UserObj>,
    /// Bytes allocated since the last collection's accounting, an estimate.
    pub bytes: usize,
    /// 5.0's `GCthreshold`: a collection runs when `bytes` reaches it.
    pub threshold: usize,
}

impl Default for Heap {
    fn default() -> Self {
        Heap {
            strs: Arena::default(),
            strt: vec![u32::MAX; 64],
            tables: Arena::default(),
            funcs: Arena::default(),
            upvals: Arena::default(),
            users: Arena::default(),
            bytes: 0,
            threshold: 64 * 1024,
        }
    }
}

impl Heap {
    // ── strings ──────────────────────────────────────────────────────────────────────────────

    /// The interned string for `s`.
    pub fn intern(&mut self, s: &[u8]) -> StrRef {
        let hash = str_hash(s);
        let b = hash as usize & (self.strt.len() - 1);
        let mut i = self.strt[b];
        while i != u32::MAX {
            let o = self.strs.get(i);
            if o.hash == hash && &*o.bytes == s {
                return StrRef {
                    id: i,
                    hash: hash & STR_HASH_MASK,
                };
            }
            i = o.next;
        }
        self.bytes += 32 + s.len();
        let id = self.strs.alloc(StrObj {
            bytes: s.into(),
            hash,
            next: self.strt[b],
        });
        self.strt[b] = id;
        // A string value holds its id in `STR_ID_BITS`: sixteen million live strings.
        assert!(
            id < 1 << STR_ID_BITS,
            "more live strings than a value can name"
        );
        if self.strs.live() > self.strt.len() {
            self.resize_strt(self.strt.len() * 2);
        }
        StrRef {
            id,
            hash: hash & STR_HASH_MASK,
        }
    }

    fn resize_strt(&mut self, size: usize) {
        let mut strt = vec![u32::MAX; size];
        for (i, slot) in self.strs.items.iter_mut().enumerate() {
            if let Some(o) = slot {
                let b = o.hash as usize & (size - 1);
                o.next = strt[b];
                strt[b] = i as u32;
            }
        }
        self.strt = strt;
    }

    pub fn str_value(&mut self, s: &[u8]) -> Value {
        Value::Str(self.intern(s))
    }

    #[inline]
    pub fn str(&self, s: StrRef) -> &[u8] {
        &self.strs.get(s.id).bytes
    }

    // ── tables ───────────────────────────────────────────────────────────────────────────────

    pub fn new_table(&mut self, narray: usize, lnhash: u8) -> TableRef {
        let t = Table::new(narray, lnhash);
        self.bytes += t.bytes();
        TableRef(self.tables.alloc(t))
    }

    #[inline]
    pub fn table(&self, t: TableRef) -> &Table {
        self.tables.get(t.0)
    }

    #[inline]
    pub fn table_mut(&mut self, t: TableRef) -> &mut Table {
        self.tables.get_mut(t.0)
    }

    // ── functions and upvalues ───────────────────────────────────────────────────────────────

    pub fn new_func(&mut self, f: FuncObj) -> FuncRef {
        self.bytes += 48;
        FuncRef(self.funcs.alloc(f))
    }

    #[inline]
    pub fn func(&self, f: FuncRef) -> &FuncObj {
        self.funcs.get(f.0)
    }

    #[inline]
    pub fn func_mut(&mut self, f: FuncRef) -> &mut FuncObj {
        self.funcs.get_mut(f.0)
    }

    /// Every live function, for a hot reload's walk.
    pub fn funcs_mut(&mut self) -> impl Iterator<Item = &mut FuncObj> {
        self.funcs.items.iter_mut().flatten()
    }

    pub fn new_upval(&mut self, u: UpvalObj) -> u32 {
        self.bytes += 24;
        self.upvals.alloc(u)
    }

    #[inline]
    pub fn upval(&self, i: u32) -> UpvalObj {
        *self.upvals.get(i)
    }

    #[inline]
    pub fn upval_mut(&mut self, i: u32) -> &mut UpvalObj {
        self.upvals.get_mut(i)
    }

    pub fn new_user(&mut self, u: UserObj) -> UserRef {
        self.bytes += 32;
        UserRef(self.users.alloc(u))
    }

    pub fn user(&self, u: UserRef) -> &UserObj {
        self.users.get(u.0)
    }

    pub fn user_mut(&mut self, u: UserRef) -> &mut UserObj {
        self.users.get_mut(u.0)
    }

    /// The metatable of a value: tables and userdata carry one; 1.12 has none for any other type.
    pub fn metatable(&self, v: Value) -> Option<TableRef> {
        match v.unpack() {
            V::Table(t) => self.table(t).meta,
            V::User(u) => self.user(u).meta,
            _ => None,
        }
    }

    // ── collection ───────────────────────────────────────────────────────────────────────────

    /// Live objects per arena: strings, tables, functions, upvalues, userdata.
    pub fn counts(&self) -> [usize; 5] {
        [
            self.strs.live(),
            self.tables.live(),
            self.funcs.live(),
            self.upvals.live(),
            self.users.live(),
        ]
    }

    /// Mark from `roots` (and `extra_upvals`, the open upvalues), clear weak entries, sweep.
    /// Answers the live byte estimate, which 5.0's `luaC_collectgarbage` doubles into the next
    /// threshold.
    pub fn collect(&mut self, roots: &[Value], extra_upvals: &[u32], mode_key: StrRef) -> usize {
        let mut gray: Vec<Value> = roots.to_vec();
        let mut gray_upvals: Vec<u32> = extra_upvals.to_vec();
        let mut weak_tables: Vec<u32> = Vec::new();
        // Constants of every reached prototype, once each.
        let mut seen_protos: std::collections::HashSet<*const Proto> = Default::default();
        loop {
            if let Some(u) = gray_upvals.pop() {
                if !self.upvals.marks[u as usize] {
                    self.upvals.marks[u as usize] = true;
                    if let UpvalObj::Closed(v) = self.upval(u) {
                        gray.push(v);
                    }
                }
                continue;
            }
            let Some(v) = gray.pop() else { break };
            // A register the VM never cleared may hold a handle whose object died; such a
            // handle is skipped (`Vm::precall` leaves registers above the parameters as they
            // were, and the compiler writes every register before it reads it).
            if !self.is_live(v) {
                continue;
            }
            match v.unpack() {
                V::Str(s) => self.strs.marks[s.id as usize] = true,
                V::Table(t) => {
                    if self.tables.marks[t.0 as usize] {
                        continue;
                    }
                    self.tables.marks[t.0 as usize] = true;
                    let tab = self.tables.get(t.0);
                    let weak = match tab.meta {
                        Some(m) => self.weak_mode(m, mode_key),
                        None => 0,
                    };
                    if let Some(m) = tab.meta {
                        gray.push(Value::Table(m));
                    }
                    let (wk, wv) = (weak & 1 != 0, weak & 2 != 0);
                    if weak != 0 {
                        weak_tables.push(t.0);
                    }
                    // Strings are values, never cleared from a weak table: always marked.
                    gray.extend(tab.array.iter().copied().filter(|v| {
                        is_collectable(*v) && (!wv || matches!(v.unpack(), V::Str(_)))
                    }));
                    for n in &tab.node {
                        if n.val.is_nil() {
                            continue;
                        }
                        // A weak entry's strings stay: strings are values, never cleared.
                        if !wk || matches!(n.key.unpack(), V::Str(_)) {
                            gray.push(n.key);
                        }
                        if !wv || matches!(n.val.unpack(), V::Str(_)) {
                            gray.push(n.val);
                        }
                    }
                    self.tables.get_mut(t.0).weak = weak;
                }
                V::Func(f) => {
                    if self.funcs.marks[f.0 as usize] {
                        continue;
                    }
                    self.funcs.marks[f.0 as usize] = true;
                    match self.funcs.get(f.0) {
                        FuncObj::Lua { proto, upvals, env } => {
                            gray.push(Value::Table(*env));
                            gray_upvals.extend_from_slice(upvals);
                            mark_proto(proto, &mut gray, &mut seen_protos);
                        }
                        FuncObj::Native { upvals, env, .. } => {
                            gray.push(Value::Table(*env));
                            gray.extend_from_slice(upvals);
                        }
                    }
                }
                V::User(u) => {
                    if self.users.marks[u.0 as usize] {
                        continue;
                    }
                    self.users.marks[u.0 as usize] = true;
                    if let Some(m) = self.users.get(u.0).meta {
                        gray.push(Value::Table(m));
                    }
                }
                _ => {}
            }
        }
        // Weak tables drop entries whose collectable key or value died.
        for t in weak_tables {
            let marks = (
                &self.strs.marks,
                &self.tables.marks,
                &self.funcs.marks,
                &self.users.marks,
            );
            let alive = |v: Value| match v.unpack() {
                V::Table(r) => marks.1[r.0 as usize],
                V::Func(r) => marks.2[r.0 as usize],
                V::User(r) => marks.3[r.0 as usize],
                _ => true,
            };
            let tab = self.tables.items[t as usize].as_mut().unwrap();
            for v in tab.array.iter_mut() {
                if !alive(*v) {
                    *v = Value::Nil;
                }
            }
            for n in tab.node.iter_mut() {
                if !n.val.is_nil() && (!alive(n.key) || !alive(n.val)) {
                    n.val = Value::Nil;
                }
            }
        }
        // Sweep. Dead strings leave the intern chains.
        let mut live = 0usize;
        live += sweep(&mut self.tables, |t| t.bytes());
        live += sweep(&mut self.funcs, |_| 48);
        live += sweep(&mut self.upvals, |_| 24);
        live += sweep(&mut self.users, |_| 32);
        let mut dead_str = false;
        for (i, slot) in self.strs.items.iter_mut().enumerate() {
            if slot.is_some() {
                if self.strs.marks[i] {
                    self.strs.marks[i] = false;
                    live += 32 + slot.as_ref().unwrap().bytes.len();
                } else {
                    *slot = None;
                    self.strs.free.push(i as u32);
                    dead_str = true;
                }
            }
        }
        if dead_str {
            let size = self.strt.len();
            self.resize_strt(size);
        }
        // A key whose node is dead (nil value) may name a freed object; 5.0 keeps such keys for
        // `next` and never reads them. They stay as stale handles, compared by identity only.
        self.bytes = live;
        live
    }

    /// Whether a value's object still exists; anything not collectable does.
    fn is_live(&self, v: Value) -> bool {
        match v.unpack() {
            V::Str(s) => self
                .strs
                .items
                .get(s.id as usize)
                .is_some_and(Option::is_some),
            V::Table(t) => self
                .tables
                .items
                .get(t.0 as usize)
                .is_some_and(Option::is_some),
            V::Func(f) => self
                .funcs
                .items
                .get(f.0 as usize)
                .is_some_and(Option::is_some),
            V::User(u) => self
                .users
                .items
                .get(u.0 as usize)
                .is_some_and(Option::is_some),
            _ => true,
        }
    }

    /// `__mode` of a metatable: bit 0 weak keys, bit 1 weak values.
    fn weak_mode(&self, meta: TableRef, mode_key: StrRef) -> u8 {
        match (self.table(meta).get(Value::Str(mode_key))).unpack() {
            V::Str(s) => {
                let m = self.str(s);
                (m.contains(&b'k') as u8) | ((m.contains(&b'v') as u8) << 1)
            }
            _ => 0,
        }
    }
}

fn is_collectable(v: Value) -> bool {
    matches!(
        v.unpack(),
        V::Str(_) | V::Table(_) | V::Func(_) | V::User(_)
    )
}

fn mark_proto(
    p: &Rc<Proto>,
    gray: &mut Vec<Value>,
    seen: &mut std::collections::HashSet<*const Proto>,
) {
    if !seen.insert(Rc::as_ptr(p)) {
        return;
    }
    gray.extend(p.consts.iter().copied().filter(|v| is_collectable(*v)));
    for q in &p.protos {
        mark_proto(q, gray, seen);
    }
}

fn sweep<T>(a: &mut Arena<T>, size: impl Fn(&T) -> usize) -> usize {
    let mut live = 0;
    for (i, slot) in a.items.iter_mut().enumerate() {
        if let Some(o) = slot {
            if a.marks[i] {
                a.marks[i] = false;
                live += size(o);
            } else {
                *slot = None;
                a.free.push(i as u32);
            }
        }
    }
    live
}
