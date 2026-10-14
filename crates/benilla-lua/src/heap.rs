//! The heap: every collectable in a typed arena, addressed by index. Cloning a `Heap` is a
//! snapshot of every object; there is no pointer anywhere to fix up.

use std::rc::Rc;

use crate::proto::Proto;
use crate::table::Table;
use crate::value::{FuncRef, StrRef, TableRef, UserRef, Value, STR_HASH_MASK, STR_ID_BITS, V};

/// A native function. Its arguments are the `nargs` values below the top of the stack; it pushes
/// its results and answers how many.
pub type NativeFn = fn(&mut crate::vm::Vm, usize) -> Result<usize, crate::vm::LuaError>;

/// A native function the host built at run time, with state of its own: the same calling
/// convention as [`NativeFn`].
pub type HostFn = Rc<dyn Fn(&mut crate::vm::Vm, usize) -> Result<usize, crate::vm::LuaError>>;

/// The longest string kept inside its object; a longer one is boxed. Most UI strings fit, and
/// an inline one costs no allocation and no second pointer to compare.
const INLINE: usize = 22;

#[derive(Clone, Debug)]
enum StrData {
    Inline(u8, [u8; INLINE]),
    Boxed(Box<[u8]>),
}

#[derive(Clone, Debug)]
pub struct StrObj {
    data: StrData,
    pub hash: u32,
}

impl StrObj {
    fn new(s: &[u8], hash: u32) -> StrObj {
        let data = if s.len() <= INLINE {
            let mut buf = [0; INLINE];
            buf[..s.len()].copy_from_slice(s);
            StrData::Inline(s.len() as u8, buf)
        } else {
            StrData::Boxed(s.into())
        };
        StrObj { data, hash }
    }

    #[inline]
    pub fn bytes(&self) -> &[u8] {
        match &self.data {
            StrData::Inline(n, buf) => &buf[..*n as usize],
            StrData::Boxed(b) => b,
        }
    }
}

/// An empty intern slot. No entry equals it: string ids stop below `(1 << STR_ID_BITS) - 1`.
const STRT_EMPTY: u32 = u32::MAX;
const STR_ID_MASK: u32 = (1 << STR_ID_BITS) - 1;

/// An intern slot, four bytes so a large table stays in cache: string `id` in the low
/// `STR_ID_BITS`, eight bits of its hash above, so a probe passes most other strings without
/// touching them.
#[inline]
fn strt_entry(hash: u32, id: u32) -> u32 {
    id | strt_tag(hash)
}

#[inline]
fn strt_tag(hash: u32) -> u32 {
    (hash & 0xff) << STR_ID_BITS
}

/// Where a probe for `hash` starts in a table of `mask + 1` slots: the hash mixed, so 5.0's
/// weak low bits do not cluster a linear probe.
#[inline]
fn strt_home(hash: u32, mask: usize) -> usize {
    (hash.wrapping_mul(0x9E37_79B9) as usize >> 7) & mask
}

/// What a call needs of a Lua function's prototype, copied into the closure so a call
/// reads it without following the prototype pointer.
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    /// The chunk the function was compiled from (`Heap::sources`).
    pub source: u32,
    pub nparams: u8,
    pub vararg: bool,
    pub maxstack: u16,
    /// The first instruction, so a call starts its frame without reaching the prototype.
    /// Valid as long as the prototype, which the closure holds.
    pub code: *const crate::proto::Op,
    /// The constant table, for the same reason.
    pub consts: *const Value,
}

impl Shape {
    pub fn of(p: &Proto) -> Shape {
        Shape {
            source: p.source_id,
            nparams: p.nparams,
            vararg: p.vararg,
            maxstack: p.maxstack,
            code: p.code.as_ptr(),
            consts: p.consts.as_ptr(),
        }
    }
}

#[derive(Clone)]
pub enum FuncObj {
    Lua {
        proto: Rc<Proto>,
        upvals: Box<[u32]>,
        env: TableRef,
        /// The prototype's call shape; replaced with it on a hot reload.
        shape: Shape,
    },
    Native {
        f: NativeFn,
        /// A host closure, called in `f`'s place when present.
        host: Option<HostFn>,
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

/// Objects of one type, addressed by handle. A handle is a plain index anyone can build, so
/// every slot holds a valid object: a freed slot, and the padding that keeps `items` a power of
/// two long, hold an empty one. A handle is masked into bounds, so a stale or foreign handle
/// reads some object of the right type, never freed or outside memory, and an access costs one
/// `and` and no branch.
#[derive(Clone)]
struct Arena<T> {
    items: Vec<T>,
    /// Each slot's `FREE`, `LIVE` or `MARKED`.
    state: Vec<u8>,
    free: Vec<u32>,
    /// Slots ever handed out: the rest of `items` is padding.
    used: u32,
    /// `items.len() - 1`.
    mask: usize,
}

/// A slot with no live object.
const FREE: u8 = 0;
/// A live object a collection has not reached yet.
const LIVE: u8 = 1;
/// A live object this collection reached.
const MARKED: u8 = 2;

/// The empty object a slot with no live one holds.
trait Dead {
    fn dead() -> Self;
}

impl Dead for StrObj {
    fn dead() -> Self {
        StrObj {
            data: StrData::Inline(0, [0; INLINE]),
            hash: 0,
        }
    }
}

impl Dead for Table {
    fn dead() -> Self {
        Table::dead()
    }
}

impl Dead for FuncObj {
    fn dead() -> Self {
        FuncObj::Native {
            f: dead_function,
            host: None,
            name: "?",
            upvals: Box::new([]),
            env: TableRef(0),
        }
    }
}

impl Dead for UpvalObj {
    fn dead() -> Self {
        UpvalObj::Closed(Value::Nil)
    }
}

impl Dead for UserObj {
    fn dead() -> Self {
        UserObj { meta: None, tag: 0 }
    }
}

impl<T: Dead> Arena<T> {
    fn new() -> Self {
        const FIRST: usize = 64;
        Arena {
            items: (0..FIRST).map(|_| T::dead()).collect(),
            state: vec![FREE; FIRST],
            free: Vec::new(),
            used: 0,
            mask: FIRST - 1,
        }
    }

    fn alloc(&mut self, v: T) -> u32 {
        let i = match self.free.pop() {
            Some(i) => i,
            None => {
                if self.used as usize == self.items.len() {
                    let n = self.items.len() * 2;
                    self.items.resize_with(n, T::dead);
                    self.state.resize(n, FREE);
                    self.mask = n - 1;
                }
                self.used += 1;
                self.used - 1
            }
        };
        // The empty object owns nothing (anything a stale handle wrote into it is leaked, not
        // dropped): no drop to run on the hot path of every allocation.
        std::mem::forget(std::mem::replace(&mut self.items[i as usize], v));
        self.state[i as usize] = LIVE;
        i
    }

    /// Whether handle `i` names a live object.
    fn is_live(&self, i: u32) -> bool {
        self.state.get(i as usize).is_some_and(|&s| s != FREE)
    }

    /// The object handle `i` names: masked into bounds (see the type). A debug build panics on
    /// a handle to no live object; the VM never makes one.
    #[inline(always)]
    fn get(&self, i: u32) -> &T {
        debug_assert!(self.is_live(i), "a live handle");
        let i = i as usize & self.mask;
        // SAFETY: `items.len()` is a power of two and `mask` one less, so `i` is below it.
        unsafe { self.items.get_unchecked(i) }
    }

    #[inline(always)]
    fn get_mut(&mut self, i: u32) -> &mut T {
        debug_assert!(self.is_live(i), "a live handle");
        let i = i as usize & self.mask;
        // SAFETY: as `get`.
        unsafe { self.items.get_unchecked_mut(i) }
    }

    /// Mark live object `i`: whether it was live and not yet marked.
    fn mark(&mut self, i: u32) -> bool {
        match self.state.get_mut(i as usize) {
            Some(s) if *s == LIVE => {
                *s = MARKED;
                true
            }
            _ => false,
        }
    }

    fn iter_live(&self) -> impl Iterator<Item = (usize, &T)> {
        self.items
            .iter()
            .zip(&self.state)
            .enumerate()
            .filter(|(_, (_, &s))| s != FREE)
            .map(|(i, (o, _))| (i, o))
    }

    fn iter_live_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.items
            .iter_mut()
            .zip(&self.state)
            .filter(|(_, &s)| s != FREE)
            .map(|(o, _)| o)
    }

    fn live(&self) -> usize {
        self.used as usize - self.free.len()
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

/// One owner's share of the heap.
#[derive(Clone, Copy, Debug, Default)]
pub struct Census {
    pub tables: usize,
    pub functions: usize,
    pub bytes: usize,
}

#[derive(Clone)]
pub struct Heap {
    strs: Arena<StrObj>,
    /// The intern table: open addressing, linear probing, at most half full.
    strt: Vec<u32>,
    tables: Arena<Table>,
    funcs: Arena<FuncObj>,
    upvals: Arena<UpvalObj>,
    users: Arena<UserObj>,
    /// Bytes allocated since the last collection's accounting, an estimate.
    pub bytes: usize,
    /// 5.0's `GCthreshold`: a collection runs when `bytes` reaches it.
    pub threshold: usize,
    /// Every chunk name a prototype was compiled under, by id: the owners a profile and a
    /// memory census charge to.
    pub sources: Vec<crate::ast::Bytes>,
    source_ids: std::collections::HashMap<crate::ast::Bytes, u32>,
    /// The chunk whose code runs now; a new table is charged to it.
    pub cur_source: u32,
    /// `intern_with`'s buffer.
    scratch: Vec<u8>,
}

impl Default for Heap {
    fn default() -> Self {
        Heap {
            strs: Arena::new(),
            strt: vec![STRT_EMPTY; 128],
            tables: Arena::new(),
            funcs: Arena::new(),
            upvals: Arena::new(),
            users: Arena::new(),
            bytes: 0,
            threshold: 64 * 1024,
            // Id 0 is the host: whatever no Lua chunk created.
            sources: vec![Rc::from(&b"=[host]"[..])],
            source_ids: Default::default(),
            cur_source: 0,
            scratch: Vec::new(),
        }
    }
}

impl Heap {
    // ── strings ──────────────────────────────────────────────────────────────────────────────

    /// The interned string for `s`.
    pub fn intern(&mut self, s: &[u8]) -> StrRef {
        let hash = str_hash(s);
        let mask = self.strt.len() - 1;
        let mut b = strt_home(hash, mask);
        loop {
            let e = self.strt[b];
            if e == STRT_EMPTY {
                break;
            }
            if e & !STR_ID_MASK == strt_tag(hash) {
                let id = e & STR_ID_MASK;
                let o = self.strs.get(id);
                if o.hash == hash && o.bytes() == s {
                    return StrRef {
                        id,
                        hash: hash & STR_HASH_MASK,
                    };
                }
            }
            b = (b + 1) & mask;
        }
        self.bytes += 32 + s.len();
        let id = self.strs.alloc(StrObj::new(s, hash));
        self.strt[b] = strt_entry(hash, id);
        // A string value holds its id in `STR_ID_BITS`: sixteen million live strings, the
        // last id left free so no slot reads as empty.
        assert!(id < STR_ID_MASK, "more live strings than a value can name");
        if self.strs.live() * 2 > self.strt.len() {
            self.resize_strt(self.strt.len() * 2);
        }
        StrRef {
            id,
            hash: hash & STR_HASH_MASK,
        }
    }

    /// Rebuild the intern table at `size` slots from the live strings.
    fn resize_strt(&mut self, size: usize) {
        let mut strt = vec![STRT_EMPTY; size];
        let mask = size - 1;
        for (i, o) in self.strs.iter_live() {
            let mut b = strt_home(o.hash, mask);
            while strt[b] != STRT_EMPTY {
                b = (b + 1) & mask;
            }
            strt[b] = strt_entry(o.hash, i as u32);
        }
        self.strt = strt;
    }

    /// The string `fill` writes, built in a buffer the heap keeps: a string made from heap
    /// strings without a fresh allocation.
    pub fn intern_with(&mut self, fill: impl FnOnce(&Heap, &mut Vec<u8>)) -> StrRef {
        let mut b = std::mem::take(&mut self.scratch);
        b.clear();
        fill(self, &mut b);
        let r = self.intern(&b);
        self.scratch = b;
        r
    }

    pub fn str_value(&mut self, s: &[u8]) -> Value {
        Value::Str(self.intern(s))
    }

    #[inline]
    pub fn str(&self, s: StrRef) -> &[u8] {
        self.strs.get(s.id).bytes()
    }

    // ── tables ───────────────────────────────────────────────────────────────────────────────

    /// The id of chunk name `name`, made on first sight.
    pub fn source_id(&mut self, name: &crate::ast::Bytes) -> u32 {
        if let Some(&id) = self.source_ids.get(name) {
            return id;
        }
        let id = self.sources.len() as u32;
        self.sources.push(name.clone());
        self.source_ids.insert(name.clone(), id);
        id
    }

    /// Bytes and objects per owner: each table to the chunk that made it, each closure to its
    /// prototype's chunk. Indexed by source id.
    pub fn census(&self) -> Vec<Census> {
        let mut out = vec![Census::default(); self.sources.len()];
        for (_, t) in self.tables.iter_live() {
            let c = &mut out[t.owner as usize];
            c.tables += 1;
            c.bytes += t.bytes();
        }
        for (_, f) in self.funcs.iter_live() {
            if let FuncObj::Lua { proto, upvals, .. } = f {
                let c = &mut out[proto.source_id as usize];
                c.functions += 1;
                c.bytes += 48 + upvals.len() * 4;
            }
        }
        out
    }

    /// `luaH_new`: `narray` array slots and room for `nhash` keys.
    pub fn new_table(&mut self, narray: usize, nhash: usize) -> TableRef {
        let mut t = Table::new(narray, nhash);
        t.owner = self.cur_source;
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
        self.funcs.iter_live_mut()
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

    /// Mark from `roots` (and `extra_upvals`, the open upvalues, and `code`, prototypes and
    /// upvalue lists no closure holds but a running frame does), clear weak entries, sweep.
    /// Answers the live byte estimate, which 5.0's `luaC_collectgarbage` doubles into the next
    /// threshold.
    pub fn collect(
        &mut self,
        roots: &[Value],
        extra_upvals: &[u32],
        code: &[(Rc<Proto>, Box<[u32]>)],
        mode_key: StrRef,
    ) -> usize {
        let mut gray: Vec<Value> = roots.to_vec();
        let mut gray_upvals: Vec<u32> = extra_upvals.to_vec();
        let mut weak_tables: Vec<u32> = Vec::new();
        // Constants of every reached prototype, once each.
        // A number no other collection, in any VM, has used: prototypes are shared by forks.
        let epoch = GC_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        for (p, ups) in code {
            mark_proto(p, &mut gray, epoch);
            gray_upvals.extend_from_slice(ups);
        }
        loop {
            if let Some(u) = gray_upvals.pop() {
                if self.upvals.mark(u) {
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
                V::Str(s) => {
                    self.strs.mark(s.id);
                }
                V::Table(t) => {
                    if !self.tables.mark(t.0) {
                        continue;
                    }
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
                    if !self.funcs.mark(f.0) {
                        continue;
                    }
                    match self.funcs.get(f.0) {
                        FuncObj::Lua {
                            proto, upvals, env, ..
                        } => {
                            gray.push(Value::Table(*env));
                            gray_upvals.extend_from_slice(upvals);
                            mark_proto(proto, &mut gray, epoch);
                        }
                        FuncObj::Native { upvals, env, .. } => {
                            gray.push(Value::Table(*env));
                            gray.extend_from_slice(upvals);
                        }
                    }
                }
                V::User(u) => {
                    if !self.users.mark(u.0) {
                        continue;
                    }
                    if let Some(m) = self.users.get(u.0).meta {
                        gray.push(Value::Table(m));
                    }
                }
                _ => {}
            }
        }
        // Weak tables drop entries whose collectable key or value died.
        for t in weak_tables {
            let (tables, funcs, users) = (&self.tables.state, &self.funcs.state, &self.users.state);
            let marked = |st: &Vec<u8>, i: u32| st.get(i as usize) == Some(&MARKED);
            let alive = |v: Value| match v.unpack() {
                V::Table(r) => marked(tables, r.0),
                V::Func(r) => marked(funcs, r.0),
                V::User(r) => marked(users, r.0),
                _ => true,
            };
            let tab = &mut self.tables.items[t as usize];
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
        let freed = self.strs.free.len();
        live += sweep(&mut self.strs, |s| 32 + s.bytes().len());
        let dead_str = self.strs.free.len() != freed;
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
            V::Str(s) => self.strs.is_live(s.id),
            V::Table(t) => self.tables.is_live(t.0),
            V::Func(f) => self.funcs.is_live(f.0),
            V::User(u) => self.users.is_live(u.0),
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

/// The function a handle to no live function calls.
fn dead_function(vm: &mut crate::vm::Vm, _: usize) -> Result<usize, crate::vm::LuaError> {
    Err(vm.rt_error("attempt to call a dead function"))
}

fn is_collectable(v: Value) -> bool {
    matches!(
        v.unpack(),
        V::Str(_) | V::Table(_) | V::Func(_) | V::User(_)
    )
}

/// Hands out collection numbers for `Proto::gc_mark`.
static GC_EPOCH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Constants of `p` and its nested prototypes, once per collection.
fn mark_proto(p: &Rc<Proto>, gray: &mut Vec<Value>, epoch: u32) {
    if p.gc_mark.get() == epoch {
        return;
    }
    p.gc_mark.set(epoch);
    gray.extend(p.consts.iter().copied().filter(|v| is_collectable(*v)));
    for q in &p.protos {
        mark_proto(q, gray, epoch);
    }
}

/// Free every live object the mark left unmarked, and unmark the rest; answers the bytes that
/// stay.
fn sweep<T: Dead>(a: &mut Arena<T>, size: impl Fn(&T) -> usize) -> usize {
    let mut live = 0;
    let n = a.used as usize;
    let (items, state) = (&mut a.items[..n], &mut a.state[..n]);
    for i in 0..n {
        match state[i] {
            FREE => {}
            MARKED => {
                state[i] = LIVE;
                live += size(&items[i]);
            }
            _ => {
                items[i] = T::dead();
                state[i] = FREE;
                a.free.push(i as u32);
            }
        }
    }
    live
}
