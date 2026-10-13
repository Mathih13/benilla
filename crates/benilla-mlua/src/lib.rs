//! The slice of mlua 0.11's API that benilla uses, over [`benilla_lua`]'s VM for the 1.12 dialect:
//! a crate that answers to the name `mlua`, so code written against mlua runs on benilla's own
//! interpreter unchanged.
//!
//! A [`Lua`] owns one VM. Handles to tables, functions and strings ([`Table`], [`Function`],
//! [`String`]) are counted collector roots, as mlua's registry references are. A Rust callback
//! runs with the `&mut Vm` the interpreter hands it; while it runs, every handle reaches the VM
//! through that reference, so one chain of borrows covers each access. A callback's error is
//! raised in Lua as its message, a string, as a 1.12 C function raises one; a Lua error that
//! reaches Rust is a [`Error::RuntimeError`] with that value's text.

use std::any::{Any, TypeId};
use std::borrow::Cow;
use std::cell::{Cell, Ref, RefCell, RefMut, UnsafeCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

use benilla_lua::heap::HostFn;
use benilla_lua::value::V;
use benilla_lua::vm::Vm;

mod conv;
mod error;

pub use conv::{FromLua, FromLuaMulti, IntoLua, IntoLuaMulti};
pub use error::{Error, ErrorContext, ExternalError, Result};

/// The interpreter underneath, for code that works below this API.
pub use benilla_lua;

pub type Integer = i64;
pub type Number = f64;

pub mod prelude {
    pub use crate::{
        Error as LuaError, FromLua, FromLuaMulti, Function as LuaFunction, IntoLua, IntoLuaMulti,
        Lua, MultiValue as LuaMultiValue, Result as LuaResult, String as LuaString,
        Table as LuaTable, Value as LuaValue,
    };
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The state
// ─────────────────────────────────────────────────────────────────────────────────────────────

pub(crate) struct Inner {
    /// The VM, owned here and reached through `cur`.
    _owned: UnsafeCell<Box<Vm>>,
    /// The VM every access goes through: the owned one, or the `&mut Vm` a running callback was
    /// handed.
    cur: Cell<*mut Vm>,
    app: RefCell<HashMap<TypeId, Rc<dyn Any>>>,
}

/// A Lua state: a cheap handle, cloned freely, as mlua's is.
#[derive(Clone)]
pub struct Lua(pub(crate) Rc<Inner>);

impl Default for Lua {
    fn default() -> Self {
        Lua::new()
    }
}

impl Lua {
    /// A state with 1.12's libraries open.
    pub fn new() -> Lua {
        Lua::over(Box::new(benilla_lua::new_state()))
    }

    /// A state over `vm`, which learns its host here.
    fn over(mut vm: Box<Vm>) -> Lua {
        let ptr: *mut Vm = &mut *vm;
        let lua = Lua(Rc::new(Inner {
            _owned: UnsafeCell::new(vm),
            cur: Cell::new(ptr),
            app: RefCell::new(HashMap::new()),
        }));
        let host: std::rc::Weak<dyn Any> = Rc::downgrade(&lua.0) as std::rc::Weak<dyn Any>;
        lua.vm().host = Some(host);
        lua
    }

    /// The state a host closure runs in: the one the VM it was handed belongs to, so a forked
    /// VM's closures reach the fork.
    fn of_vm(vm: &Vm) -> Lua {
        let any = vm
            .host
            .as_ref()
            .and_then(std::rc::Weak::upgrade)
            .expect("a VM called back after its Lua state was dropped");
        Lua(any.downcast::<Inner>().expect("a benilla-mlua VM"))
    }

    /// An independent copy of this state: the whole VM, every value and closure, with no host
    /// data; the caller copies the host data it needs (`set_app_data`). A snapshot to restore
    /// from, or a template to start many runs from.
    pub fn fork(&self) -> Lua {
        let vm: Vm = self.vm().clone();
        Lua::over(Box::new(vm))
    }

    /// The VM in effect. Every use is short and holds nothing across a call into Lua.
    #[allow(clippy::mut_from_ref)]
    #[inline]
    pub(crate) fn vm(&self) -> &mut Vm {
        // SAFETY: `cur` is the owned VM or a callback's live `&mut Vm`; the facade never keeps
        // the reference past the operation it serves.
        unsafe { &mut *self.0.cur.get() }
    }

    /// The interpreter itself, for code that works below this API.
    #[allow(clippy::mut_from_ref)]
    pub fn raw_vm(&self) -> &mut Vm {
        self.vm()
    }

    // ── values ───────────────────────────────────────────────────────────────────────────────

    fn make_ref(&self, v: benilla_lua::Value) -> RefH {
        let id = self.vm().root(v);
        RefH {
            lua: self.clone(),
            id,
        }
    }

    /// A VM value as an API value: collectables become rooted handles, and an integral number
    /// becomes `Integer`, as mlua reads a 5.1 number.
    pub(crate) fn wrap(&self, v: benilla_lua::Value) -> Value {
        match v.unpack() {
            V::Nil => Value::Nil,
            V::Bool(b) => Value::Boolean(b),
            V::Num(n) => {
                let i = n as i64;
                if (i as f64).to_bits() == n.to_bits() {
                    Value::Integer(i)
                } else {
                    Value::Number(n)
                }
            }
            V::Str(_) => Value::String(String(self.make_ref(v))),
            V::Table(_) => Value::Table(Table(self.make_ref(v))),
            V::Func(_) => Value::Function(Function(self.make_ref(v))),
            V::User(u) => Value::LightUserData(LightUserData(u.0 as usize as *mut c_void)),
            V::Light(p) => Value::LightUserData(LightUserData(p as *mut c_void)),
        }
    }

    /// An API value as a VM value.
    pub(crate) fn unwrap(&self, v: &Value) -> benilla_lua::Value {
        match v {
            Value::Nil => benilla_lua::Value::Nil,
            Value::Boolean(b) => benilla_lua::Value::Bool(*b),
            Value::Integer(i) => benilla_lua::Value::Num(*i as f64),
            Value::Number(n) => benilla_lua::Value::Num(*n),
            Value::String(s) => s.0.get(),
            Value::Table(t) => t.0.get(),
            Value::Function(f) => f.0.get(),
            Value::LightUserData(l) => benilla_lua::Value::Light(l.0 as usize),
            Value::Thread(_) | Value::Error(_) => benilla_lua::Value::Nil,
        }
    }

    pub fn globals(&self) -> Table {
        let g = self.vm().globals;
        Table(self.make_ref(benilla_lua::Value::Table(g)))
    }

    pub fn create_table(&self) -> Result<Table> {
        let t = self.vm().heap.new_table(0, 0);
        Ok(Table(self.make_ref(benilla_lua::Value::Table(t))))
    }

    pub fn create_table_with_capacity(&self, narr: usize, nrec: usize) -> Result<Table> {
        let lnh = if nrec == 0 {
            0
        } else {
            (usize::BITS - (nrec - 1).leading_zeros()) as u8
        };
        let t = self.vm().heap.new_table(narr, lnh);
        Ok(Table(self.make_ref(benilla_lua::Value::Table(t))))
    }

    pub fn create_table_from<K: IntoLua, V2: IntoLua>(
        &self,
        iter: impl IntoIterator<Item = (K, V2)>,
    ) -> Result<Table> {
        let t = self.create_table()?;
        for (k, v) in iter {
            t.raw_set(k, v)?;
        }
        Ok(t)
    }

    pub fn create_sequence_from<T: IntoLua>(
        &self,
        iter: impl IntoIterator<Item = T>,
    ) -> Result<Table> {
        let t = self.create_table()?;
        for (i, v) in iter.into_iter().enumerate() {
            t.raw_set(i + 1, v)?;
        }
        Ok(t)
    }

    pub fn create_string(&self, s: impl AsRef<[u8]>) -> Result<String> {
        let v = self.vm().str(s.as_ref());
        Ok(String(self.make_ref(v)))
    }

    /// A Lua function over a Rust closure.
    pub fn create_function<F, A, R>(&self, func: F) -> Result<Function>
    where
        F: Fn(&Lua, A) -> Result<R> + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti,
    {
        let host: HostFn = Rc::new(move |vm: &mut Vm, nargs: usize| {
            let lua = Lua::of_vm(vm);
            let prev = lua.0.cur.replace(vm as *mut Vm);
            let res = (|| {
                let args: MultiValue = lua
                    .vm()
                    .args(nargs)
                    .to_vec()
                    .into_iter()
                    .map(|v| lua.wrap(v))
                    .collect();
                let a = A::from_lua_args(args, &lua)?;
                func(&lua, a)?.into_lua_multi(&lua)
            })();
            let out = match res {
                Ok(rets) => {
                    let n = rets.len();
                    for v in rets.iter() {
                        let x = lua.unwrap(v);
                        lua.vm().push(x);
                    }
                    Ok(n)
                }
                Err(e) => {
                    let vm = lua.vm();
                    let msg = vm.str(e.lua_message().as_bytes());
                    Err(vm.throw(msg))
                }
            };
            lua.0.cur.set(prev);
            out
        });
        let f = self.vm().new_host("?", host);
        Ok(Function(self.make_ref(f)))
    }

    /// mlua's `create_function_mut`: the closure behind a `RefCell`.
    pub fn create_function_mut<F, A, R>(&self, func: F) -> Result<Function>
    where
        F: FnMut(&Lua, A) -> Result<R> + 'static,
        A: FromLuaMulti,
        R: IntoLuaMulti,
    {
        let func = RefCell::new(func);
        self.create_function(move |lua, a| {
            let mut f = func
                .try_borrow_mut()
                .map_err(|_| Error::runtime("mutable callback called recursively"))?;
            f(lua, a)
        })
    }

    /// Load a chunk. Unnamed, it is named after the Rust call site, as mlua names it.
    #[track_caller]
    pub fn load(&self, chunk: impl AsChunk) -> Chunk<'_> {
        let at = std::panic::Location::caller();
        Chunk {
            lua: self,
            source: chunk.source(),
            name: chunk
                .name()
                .unwrap_or_else(|| format!("@{}:{}", at.file(), at.line())),
            mode: None,
            env: None,
        }
    }

    // ── host data ────────────────────────────────────────────────────────────────────────────

    pub fn set_app_data<T: 'static>(&self, data: T) -> Option<T> {
        let old = self
            .0
            .app
            .borrow_mut()
            .insert(TypeId::of::<T>(), Rc::new(RefCell::new(data)));
        old.and_then(|o| {
            Rc::try_unwrap(o.downcast::<RefCell<T>>().ok()?)
                .ok()
                .map(RefCell::into_inner)
        })
    }

    fn app_cell<T: 'static>(&self) -> Option<Rc<RefCell<T>>> {
        let app = self.0.app.borrow();
        let any = app.get(&TypeId::of::<T>())?.clone();
        any.downcast::<RefCell<T>>().ok()
    }

    /// A shared borrow of the host's `T`; panics while it is mutably borrowed, as mlua does.
    pub fn app_data_ref<T: 'static>(&self) -> Option<AppDataRef<'_, T>> {
        let cell = self.app_cell::<T>()?;
        // SAFETY: the cell lives in `app` until replaced or removed, and the guard borrows `self`.
        let r: Ref<'_, T> = unsafe { (*Rc::as_ptr(&cell)).borrow() };
        Some(AppDataRef { r, _cell: cell })
    }

    /// A mutable borrow of the host's `T`; panics while it is borrowed, as mlua does.
    pub fn app_data_mut<T: 'static>(&self) -> Option<AppDataRefMut<'_, T>> {
        let cell = self.app_cell::<T>()?;
        // SAFETY: as `app_data_ref`.
        let r: RefMut<'_, T> = unsafe { (*Rc::as_ptr(&cell)).borrow_mut() };
        Some(AppDataRefMut { r, _cell: cell })
    }

    pub fn remove_app_data<T: 'static>(&self) -> Option<T> {
        let old = self.0.app.borrow_mut().remove(&TypeId::of::<T>())?;
        Rc::try_unwrap(old.downcast::<RefCell<T>>().ok()?)
            .ok()
            .map(RefCell::into_inner)
    }

    // ── the registry ─────────────────────────────────────────────────────────────────────────

    fn registry_key(&self, name: &str) -> benilla_lua::Value {
        self.vm().str(name.as_bytes())
    }

    pub fn named_registry_value<T: FromLua>(&self, name: &str) -> Result<T> {
        let k = self.registry_key(name);
        let vm = self.vm();
        let v = vm.heap.table(vm.registry).get(k);
        T::from_lua(self.wrap(v), self)
    }

    pub fn set_named_registry_value(&self, name: &str, value: impl IntoLua) -> Result<()> {
        let v = value.into_lua(self)?;
        let k = self.registry_key(name);
        let x = self.unwrap(&v);
        let vm = self.vm();
        let reg = vm.registry;
        vm.heap
            .table_mut(reg)
            .set(k, x)
            .map_err(|_| Error::runtime("bad registry key"))
    }

    pub fn unset_named_registry_value(&self, name: &str) -> Result<()> {
        self.set_named_registry_value(name, Value::Nil)
    }

    // ── coercion ─────────────────────────────────────────────────────────────────────────────

    /// `lua_tonumber`: a number, or a string that reads as one.
    pub fn coerce_number(&self, v: Value) -> Result<Option<Number>> {
        Ok(match &v {
            Value::Integer(i) => Some(*i as f64),
            Value::Number(n) => Some(*n),
            Value::String(s) => self.vm().tonumber(s.0.get()),
            _ => None,
        })
    }

    pub fn coerce_integer(&self, v: Value) -> Result<Option<Integer>> {
        Ok(self.coerce_number(v)?.and_then(|n| {
            let i = n as i64;
            ((i as f64) == n).then_some(i)
        }))
    }

    /// `lua_tostring`: a string, or a number written as one.
    pub fn coerce_string(&self, v: Value) -> Result<Option<String>> {
        Ok(match v {
            Value::String(s) => Some(s),
            Value::Integer(_) | Value::Number(_) => {
                let x = self.unwrap(&v);
                let s = self.vm().tostr(x).expect("a number has a string form");
                Some(String(self.make_ref(benilla_lua::Value::Str(s))))
            }
            _ => None,
        })
    }

    pub fn pack_multi(&self, args: impl IntoLuaMulti) -> Result<MultiValue> {
        args.into_lua_multi(self)
    }

    pub fn unpack<T: FromLua>(&self, value: Value) -> Result<T> {
        T::from_lua(value, self)
    }

    pub fn pack(&self, t: impl IntoLua) -> Result<Value> {
        t.into_lua(self)
    }

    // ── the collector ────────────────────────────────────────────────────────────────────────

    pub fn used_memory(&self) -> usize {
        self.vm().heap.bytes
    }

    pub fn gc_collect(&self) -> Result<()> {
        self.vm().full_gc();
        Ok(())
    }

    /// 1.12 has no type metatables, and this VM none to set: a string never has one.
    pub fn set_type_metatable<T>(&self, _metatable: Option<Table>) {}

    // ── debugging ────────────────────────────────────────────────────────────────────────────

    /// The stack level `level` up, for a traceback: `f` reads it.
    pub fn inspect_stack<R>(&self, level: usize, f: impl FnOnce(&Debug) -> R) -> Option<R> {
        let info = self.vm().frame_info(level)?;
        Some(f(&Debug { info }))
    }

    /// Run `callback` about every `triggers.every_nth_instruction` instructions.
    pub fn set_hook<F>(&self, triggers: HookTriggers, callback: F) -> Result<()>
    where
        F: Fn(&Lua, &Debug) -> Result<VmState> + 'static,
    {
        let step = triggers.every_nth_instruction.unwrap_or(1_000_000) as u64;
        self.vm().set_hook(
            step,
            Rc::new(move |vm: &mut Vm| {
                let lua = Lua::of_vm(vm);
                let prev = lua.0.cur.replace(vm as *mut Vm);
                let info = lua.vm().frame_info(0).unwrap_or_default();
                let r = callback(&lua, &Debug { info });
                let out = match r {
                    Ok(_) => Ok(()),
                    Err(e) => {
                        let vm = lua.vm();
                        let msg = vm.str(e.lua_message().as_bytes());
                        Err(vm.throw(msg))
                    }
                };
                lua.0.cur.set(prev);
                out
            }),
        );
        Ok(())
    }

    pub fn remove_hook(&self) {
        self.vm().remove_hook();
    }

    /// Call `f` protected, from the host: its results, or its error.
    pub(crate) fn call_raw(&self, f: benilla_lua::Value, args: MultiValue) -> Result<MultiValue> {
        let argv: Vec<benilla_lua::Value> = args.iter().map(|v| self.unwrap(v)).collect();
        // The arguments' handles stay alive until the values are on the VM's stack.
        let r = self.vm().pcall_value(f, &argv, -1);
        drop(args);
        match r {
            Ok(vals) => Ok(vals.into_iter().map(|v| self.wrap(v)).collect()),
            Err(e) => Err(self.error_from(e)),
        }
    }

    /// A Lua error value as an `Error`: its text, as mlua reports a runtime error.
    pub(crate) fn error_from(&self, e: benilla_lua::Value) -> Error {
        Error::RuntimeError(self.vm().display(e))
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Handles
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A counted collector root: mlua's registry reference.
pub(crate) struct RefH {
    lua: Lua,
    id: u32,
}

impl RefH {
    #[inline]
    pub(crate) fn get(&self) -> benilla_lua::Value {
        self.lua.vm().rooted(self.id)
    }
}

impl Clone for RefH {
    fn clone(&self) -> Self {
        self.lua.vm().retain(self.id);
        RefH {
            lua: self.lua.clone(),
            id: self.id,
        }
    }
}

impl Drop for RefH {
    fn drop(&mut self) {
        self.lua.vm().unroot(self.id);
    }
}

impl PartialEq for RefH {
    fn eq(&self, other: &Self) -> bool {
        self.get().raw_eq(other.get())
    }
}

impl std::fmt::Debug for RefH {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.get())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Table(pub(crate) RefH);

#[derive(Clone, Debug, PartialEq)]
pub struct Function(pub(crate) RefH);

#[derive(Clone, Debug)]
pub struct String(pub(crate) RefH);

/// A coroutine; 1.12 has none, so no value ever is one.
#[derive(Clone, Debug, PartialEq)]
pub struct Thread(());

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LightUserData(pub *mut c_void);

#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Nil,
    Boolean(bool),
    LightUserData(LightUserData),
    Integer(Integer),
    Number(Number),
    String(String),
    Table(Table),
    Function(Function),
    Thread(Thread),
    Error(Box<Error>),
}

pub use Value::Nil;

impl Value {
    pub const NULL: Value = Value::LightUserData(LightUserData(std::ptr::null_mut()));

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Boolean(_) => "boolean",
            Value::LightUserData(_) => "lightuserdata",
            Value::Integer(_) => "integer",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Table(_) => "table",
            Value::Function(_) => "function",
            Value::Thread(_) => "thread",
            Value::Error(_) => "error",
        }
    }

    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    pub fn is_boolean(&self) -> bool {
        matches!(self, Value::Boolean(_))
    }

    pub fn is_number(&self) -> bool {
        matches!(self, Value::Number(_) | Value::Integer(_))
    }

    pub fn is_string(&self) -> bool {
        matches!(self, Value::String(_))
    }

    pub fn is_table(&self) -> bool {
        matches!(self, Value::Table(_))
    }

    pub fn is_function(&self) -> bool {
        matches!(self, Value::Function(_))
    }

    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            Value::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_integer(&self) -> Option<Integer> {
        match self {
            Value::Integer(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<Number> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Integer(i) => Some(*i as f64),
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.as_integer()
    }

    pub fn as_str(&self) -> Option<BorrowedStr<'_>> {
        match self {
            Value::String(s) => s.to_str().ok(),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&String> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_function(&self) -> Option<&Function> {
        match self {
            Value::Function(f) => Some(f),
            _ => None,
        }
    }

    pub fn as_light_userdata(&self) -> Option<LightUserData> {
        match self {
            Value::LightUserData(l) => Some(*l),
            _ => None,
        }
    }

    /// A pointer naming the object, for identity.
    pub fn to_pointer(&self) -> *const c_void {
        match self {
            Value::String(s) => s.to_pointer(),
            Value::Table(t) => t.to_pointer(),
            Value::Function(f) => f.to_pointer(),
            Value::LightUserData(l) => l.0,
            _ => std::ptr::null(),
        }
    }

    /// `tostring`, without metamethods.
    pub fn to_string(&self) -> Result<std::string::String> {
        Ok(match self {
            Value::Nil => "nil".into(),
            Value::Boolean(b) => b.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => benilla_lua::number::fmt_g14(*n),
            Value::String(s) => s.to_string_lossy(),
            other => format!("{}: {:p}", other.type_name(), other.to_pointer()),
        })
    }

    pub fn equals(&self, other: &Value) -> Result<bool> {
        Ok(self == other)
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Boolean(a), Value::Boolean(b)) => a == b,
            (Value::LightUserData(a), Value::LightUserData(b)) => a == b,
            (Value::Integer(a), Value::Integer(b)) => a == b,
            (Value::Integer(a), Value::Number(b)) | (Value::Number(b), Value::Integer(a)) => {
                *a as f64 == *b
            }
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => a.0 == b.0,
            (Value::Table(a), Value::Table(b)) => a == b,
            (Value::Function(a), Value::Function(b)) => a == b,
            _ => false,
        }
    }
}

fn pointer_of(r: &RefH) -> *const c_void {
    let v = r.get();
    // A synthetic address per object: its type and handle.
    let (kind, id): (usize, usize) = match v.unpack() {
        V::Table(t) => (1, t.0 as usize),
        V::Func(f) => (2, f.0 as usize),
        V::Str(s) => (3, s.id as usize),
        _ => (0, 0),
    };
    ((kind << 40) | (id << 4) | 8) as *const c_void
}

// ── tables ───────────────────────────────────────────────────────────────────────────────────

impl Table {
    fn lua(&self) -> &Lua {
        &self.0.lua
    }

    fn tref(&self) -> benilla_lua::value::TableRef {
        self.0.get().as_table().expect("a table handle")
    }

    /// `t[key] = value`, with metamethods.
    pub fn set(&self, key: impl IntoLua, value: impl IntoLua) -> Result<()> {
        let lua = self.lua();
        let k = key.into_lua(lua)?;
        let v = value.into_lua(lua)?;
        let (kx, vx) = (lua.unwrap(&k), lua.unwrap(&v));
        let t = self.0.get();
        lua.vm()
            .protected(|vm| vm.setindex(t, kx, vx, None))
            .map_err(|e| lua.error_from(e))
    }

    /// `t[key]`, with metamethods.
    pub fn get<V2: FromLua>(&self, key: impl IntoLua) -> Result<V2> {
        let lua = self.lua();
        let k = key.into_lua(lua)?;
        let kx = lua.unwrap(&k);
        let t = self.0.get();
        let v = lua
            .vm()
            .protected(|vm| vm.index(t, kx, None))
            .map_err(|e| lua.error_from(e))?;
        V2::from_lua(lua.wrap(v), lua)
    }

    pub fn contains_key(&self, key: impl IntoLua) -> Result<bool> {
        Ok(!self.get::<Value>(key)?.is_nil())
    }

    pub fn raw_set(&self, key: impl IntoLua, value: impl IntoLua) -> Result<()> {
        let lua = self.lua();
        let k = key.into_lua(lua)?;
        let v = value.into_lua(lua)?;
        let (kx, vx) = (lua.unwrap(&k), lua.unwrap(&v));
        let t = self.tref();
        lua.vm().heap.table_mut(t).set(kx, vx).map_err(|e| match e {
            benilla_lua::table::KeyError::Nil => Error::runtime("table index is nil"),
            benilla_lua::table::KeyError::NaN => Error::runtime("table index is NaN"),
        })
    }

    pub fn raw_get<V2: FromLua>(&self, key: impl IntoLua) -> Result<V2> {
        let lua = self.lua();
        let k = key.into_lua(lua)?;
        let kx = lua.unwrap(&k);
        let v = lua.vm().heap.table(self.tref()).get(kx);
        V2::from_lua(lua.wrap(v), lua)
    }

    pub fn raw_remove(&self, key: impl IntoLua) -> Result<()> {
        self.raw_set(key, Value::Nil)
    }

    /// The border: the count from 1 to the first nil.
    pub fn raw_len(&self) -> usize {
        self.lua().vm().heap.table(self.tref()).border()
    }

    pub fn len(&self) -> Result<Integer> {
        Ok(self.raw_len() as Integer)
    }

    pub fn is_empty(&self) -> bool {
        let lua = self.lua();
        matches!(
            lua.vm()
                .heap
                .table(self.tref())
                .next(benilla_lua::Value::Nil),
            Ok(None)
        )
    }

    pub fn push(&self, value: impl IntoLua) -> Result<()> {
        let n = self.raw_len();
        self.set(n + 1, value)
    }

    pub fn raw_push(&self, value: impl IntoLua) -> Result<()> {
        let n = self.raw_len();
        self.raw_set(n + 1, value)
    }

    pub fn pop<V2: FromLua>(&self) -> Result<V2> {
        let n = self.raw_len();
        if n == 0 {
            return V2::from_lua(Value::Nil, self.lua());
        }
        let v: Value = self.get(n)?;
        self.set(n, Value::Nil)?;
        V2::from_lua(v, self.lua())
    }

    pub fn raw_pop<V2: FromLua>(&self) -> Result<V2> {
        let n = self.raw_len();
        if n == 0 {
            return V2::from_lua(Value::Nil, self.lua());
        }
        let v: Value = self.raw_get(n)?;
        self.raw_set(n, Value::Nil)?;
        V2::from_lua(v, self.lua())
    }

    pub fn raw_insert(&self, idx: Integer, value: impl IntoLua) -> Result<()> {
        let n = self.raw_len() as Integer;
        let mut i = n;
        while i >= idx {
            let v: Value = self.raw_get(i)?;
            self.raw_set(i + 1, v)?;
            i -= 1;
        }
        self.raw_set(idx, value)
    }

    pub fn clear(&self) -> Result<()> {
        let keys: Vec<Value> = self
            .pairs::<Value, Value>()
            .map(|r| r.map(|(k, _)| k))
            .collect::<Result<_>>()?;
        for k in keys {
            self.raw_set(k, Value::Nil)?;
        }
        Ok(())
    }

    pub fn metatable(&self) -> Option<Table> {
        let lua = self.lua();
        let m = lua.vm().heap.table(self.tref()).meta?;
        Some(Table(lua.make_ref(benilla_lua::Value::Table(m))))
    }

    pub fn get_metatable(&self) -> Option<Table> {
        self.metatable()
    }

    pub fn set_metatable(&self, metatable: Option<Table>) -> Result<()> {
        let m = metatable.map(|m| m.tref());
        let t = self.tref();
        self.lua().vm().heap.table_mut(t).meta = m;
        Ok(())
    }

    pub fn has_metatable(&self) -> bool {
        self.metatable().is_some()
    }

    pub fn equals(&self, other: &Table) -> Result<bool> {
        let (a, b) = (self.0.get(), other.0.get());
        let lua = self.lua();
        lua.vm()
            .protected(|vm| vm.equals(a, b))
            .map_err(|e| lua.error_from(e))
    }

    pub fn to_pointer(&self) -> *const c_void {
        pointer_of(&self.0)
    }

    /// The pairs `next` walks, each converted.
    pub fn pairs<K: FromLua, V2: FromLua>(&self) -> TablePairs<K, V2> {
        TablePairs {
            table: self.clone(),
            key: Some(Value::Nil),
            _p: std::marker::PhantomData,
        }
    }

    pub fn for_each<K: FromLua, V2: FromLua>(
        &self,
        mut f: impl FnMut(K, V2) -> Result<()>,
    ) -> Result<()> {
        for kv in self.pairs::<K, V2>() {
            let (k, v) = kv?;
            f(k, v)?;
        }
        Ok(())
    }

    /// `t[1]`, `t[2]` … to the first nil.
    pub fn sequence_values<V2: FromLua>(&self) -> TableSequence<V2> {
        TableSequence {
            table: self.clone(),
            index: 1,
            _p: std::marker::PhantomData,
        }
    }

    pub fn for_each_value<V2: FromLua>(&self, mut f: impl FnMut(V2) -> Result<()>) -> Result<()> {
        for v in self.sequence_values::<V2>() {
            f(v?)?;
        }
        Ok(())
    }
}

pub struct TablePairs<K, V2> {
    table: Table,
    key: Option<Value>,
    _p: std::marker::PhantomData<(K, V2)>,
}

impl<K: FromLua, V2: FromLua> Iterator for TablePairs<K, V2> {
    type Item = Result<(K, V2)>;

    fn next(&mut self) -> Option<Self::Item> {
        let key = self.key.take()?;
        let lua = self.table.lua().clone();
        let kx = lua.unwrap(&key);
        let next = lua.vm().heap.table(self.table.tref()).next(kx);
        match next {
            Ok(Some((k, v))) => {
                let (k, v) = (lua.wrap(k), lua.wrap(v));
                self.key = Some(k.clone());
                Some(K::from_lua(k, &lua).and_then(|k| Ok((k, V2::from_lua(v, &lua)?))))
            }
            Ok(None) => None,
            Err(_) => Some(Err(Error::runtime("invalid key for `next'"))),
        }
    }
}

pub struct TableSequence<V2> {
    table: Table,
    index: usize,
    _p: std::marker::PhantomData<V2>,
}

impl<V2: FromLua> Iterator for TableSequence<V2> {
    type Item = Result<V2>;

    fn next(&mut self) -> Option<Self::Item> {
        let v: Value = match self.table.raw_get(self.index) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        if v.is_nil() {
            return None;
        }
        self.index += 1;
        Some(V2::from_lua(v, self.table.lua()))
    }
}

/// mlua's `ObjectLike`, which a table answers.
pub trait ObjectLike {
    fn get<V2: FromLua>(&self, key: impl IntoLua) -> Result<V2>;
    fn set(&self, key: impl IntoLua, value: impl IntoLua) -> Result<()>;
    fn call<R: FromLuaMulti>(&self, args: impl IntoLuaMulti) -> Result<R>;
    fn call_method<R: FromLuaMulti>(&self, name: &str, args: impl IntoLuaMulti) -> Result<R>;
    fn call_function<R: FromLuaMulti>(&self, name: &str, args: impl IntoLuaMulti) -> Result<R>;
    fn get_path<V2: FromLua>(&self, path: &str) -> Result<V2>;
    fn to_string(&self) -> Result<std::string::String>;
    fn to_value(&self) -> Value;
}

impl ObjectLike for Table {
    fn get<V2: FromLua>(&self, key: impl IntoLua) -> Result<V2> {
        Table::get(self, key)
    }

    fn set(&self, key: impl IntoLua, value: impl IntoLua) -> Result<()> {
        Table::set(self, key, value)
    }

    fn call<R: FromLuaMulti>(&self, args: impl IntoLuaMulti) -> Result<R> {
        let lua = self.lua();
        let mut a = args.into_lua_multi(lua)?;
        a.push_front(Value::Table(self.clone()));
        let r = lua.call_raw(self.0.get(), a)?;
        R::from_lua_multi(r, lua)
    }

    fn call_method<R: FromLuaMulti>(&self, name: &str, args: impl IntoLuaMulti) -> Result<R> {
        let f: Function = Table::get(self, name)?;
        let lua = self.lua();
        let mut a = args.into_lua_multi(lua)?;
        a.push_front(Value::Table(self.clone()));
        f.call(a)
    }

    fn call_function<R: FromLuaMulti>(&self, name: &str, args: impl IntoLuaMulti) -> Result<R> {
        let f: Function = Table::get(self, name)?;
        f.call(args)
    }

    fn get_path<V2: FromLua>(&self, path: &str) -> Result<V2> {
        let mut cur = Value::Table(self.clone());
        for part in path.split('.') {
            cur = match cur {
                Value::Table(t) => Table::get(&t, part)?,
                _ => Value::Nil,
            };
        }
        V2::from_lua(cur, self.lua())
    }

    fn to_string(&self) -> Result<std::string::String> {
        let lua = self.lua();
        let t = self.0.get();
        let s = lua
            .vm()
            .protected(|vm| benilla_lua::lib_base::tostring_value(vm, t))
            .map_err(|e| lua.error_from(e))?;
        Ok(lua.vm().display(s))
    }

    fn to_value(&self) -> Value {
        Value::Table(self.clone())
    }
}

// ── functions ────────────────────────────────────────────────────────────────────────────────

impl Function {
    pub fn call<R: FromLuaMulti>(&self, args: impl IntoLuaMulti) -> Result<R> {
        let lua = &self.0.lua;
        let a = args.into_lua_multi(lua)?;
        let r = lua.call_raw(self.0.get(), a)?;
        R::from_lua_multi(r, lua)
    }

    pub fn to_pointer(&self) -> *const c_void {
        pointer_of(&self.0)
    }

    /// The function's environment, for a Lua function.
    pub fn environment(&self) -> Option<Table> {
        let lua = &self.0.lua;
        let f = self.0.get().as_func()?;
        let env = match lua.vm().heap.func(f) {
            benilla_lua::heap::FuncObj::Lua { env, .. } => *env,
            benilla_lua::heap::FuncObj::Native { env, .. } => *env,
        };
        Some(Table(lua.make_ref(benilla_lua::Value::Table(env))))
    }

    /// Where the function was defined, as `lua_getinfo(L, "S")` says.
    pub fn info(&self) -> FunctionInfo {
        let lua = &self.0.lua;
        let mut out = FunctionInfo {
            what: "C",
            source: Some("=[C]".into()),
            short_src: Some("[C]".into()),
            ..Default::default()
        };
        let Some(f) = self.0.get().as_func() else {
            return out;
        };
        if let benilla_lua::heap::FuncObj::Lua { proto, .. } = lua.vm().heap.func(f) {
            let short = benilla_lua::chunkid_n(&proto.source, 60);
            out.what = if proto.line_defined == 0 {
                "main"
            } else {
                "Lua"
            };
            out.source = Some(std::string::String::from_utf8_lossy(&proto.source).into_owned());
            out.short_src = Some(std::string::String::from_utf8_lossy(&short).into_owned());
            out.line_defined = Some(proto.line_defined as usize);
        }
        out
    }

    pub fn set_environment(&self, env: Table) -> Result<bool> {
        let lua = &self.0.lua;
        let Some(f) = self.0.get().as_func() else {
            return Ok(false);
        };
        let t = env.tref();
        match lua.vm().heap.func_mut(f) {
            benilla_lua::heap::FuncObj::Lua { env, .. } => {
                *env = t;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

/// mlua's `FunctionInfo`.
#[derive(Clone, Debug, Default)]
pub struct FunctionInfo {
    pub name: Option<std::string::String>,
    pub name_what: Option<&'static str>,
    pub what: &'static str,
    pub source: Option<std::string::String>,
    pub short_src: Option<std::string::String>,
    pub line_defined: Option<usize>,
    pub last_line_defined: Option<usize>,
}

// ── strings ──────────────────────────────────────────────────────────────────────────────────

impl String {
    fn bytes_raw(&self) -> &[u8] {
        let v = self.0.get();
        let s = v.as_str().expect("a string handle");
        let b = self.0.lua.vm().heap.str(s);
        // SAFETY: the string is rooted while this handle lives, and its bytes are a boxed slice
        // that never moves.
        unsafe { std::slice::from_raw_parts(b.as_ptr(), b.len()) }
    }

    pub fn to_str(&self) -> Result<BorrowedStr<'_>> {
        std::str::from_utf8(self.bytes_raw())
            .map(|s| BorrowedStr { s })
            .map_err(|e| Error::FromLuaConversionError {
                from: "string",
                to: "&str".to_string(),
                message: Some(e.to_string()),
            })
    }

    pub fn to_string_lossy(&self) -> std::string::String {
        std::string::String::from_utf8_lossy(self.bytes_raw()).into_owned()
    }

    pub fn as_bytes(&self) -> BorrowedBytes<'_> {
        BorrowedBytes {
            b: self.bytes_raw(),
        }
    }

    pub fn to_pointer(&self) -> *const c_void {
        pointer_of(&self.0)
    }

    pub fn display(&self) -> impl std::fmt::Display + '_ {
        self.to_string_lossy()
    }
}

impl PartialEq for String {
    fn eq(&self, other: &Self) -> bool {
        self.bytes_raw() == other.bytes_raw()
    }
}

impl<T: AsRef<[u8]> + ?Sized> PartialEq<T> for String {
    fn eq(&self, other: &T) -> bool {
        self.bytes_raw() == other.as_ref()
    }
}

/// A string's text, borrowed from its handle. Not `Clone`, as mlua's is not, so `to_owned()`
/// reaches `str`'s and answers a `String`.
pub struct BorrowedStr<'a> {
    s: &'a str,
}

impl Deref for BorrowedStr<'_> {
    type Target = str;
    fn deref(&self) -> &str {
        self.s
    }
}

impl AsRef<str> for BorrowedStr<'_> {
    fn as_ref(&self) -> &str {
        self.s
    }
}

impl std::borrow::Borrow<str> for BorrowedStr<'_> {
    fn borrow(&self) -> &str {
        self.s
    }
}

impl std::fmt::Display for BorrowedStr<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.s.fmt(f)
    }
}

impl std::fmt::Debug for BorrowedStr<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.s.fmt(f)
    }
}

impl<T: AsRef<str> + ?Sized> PartialEq<T> for BorrowedStr<'_> {
    fn eq(&self, other: &T) -> bool {
        self.s == other.as_ref()
    }
}

impl From<BorrowedStr<'_>> for std::string::String {
    fn from(b: BorrowedStr<'_>) -> Self {
        b.s.to_owned()
    }
}

impl<'a> From<BorrowedStr<'a>> for Cow<'a, str> {
    fn from(b: BorrowedStr<'a>) -> Self {
        Cow::Borrowed(b.s)
    }
}

/// A string's bytes, borrowed from its handle.
pub struct BorrowedBytes<'a> {
    b: &'a [u8],
}

impl Deref for BorrowedBytes<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.b
    }
}

impl AsRef<[u8]> for BorrowedBytes<'_> {
    fn as_ref(&self) -> &[u8] {
        self.b
    }
}

impl std::borrow::Borrow<[u8]> for BorrowedBytes<'_> {
    fn borrow(&self) -> &[u8] {
        self.b
    }
}

impl std::fmt::Debug for BorrowedBytes<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.b.fmt(f)
    }
}

impl<T: AsRef<[u8]> + ?Sized> PartialEq<T> for BorrowedBytes<'_> {
    fn eq(&self, other: &T) -> bool {
        self.b == other.as_ref()
    }
}

impl<'a> IntoIterator for BorrowedBytes<'a> {
    type Item = &'a u8;
    type IntoIter = std::slice::Iter<'a, u8>;
    fn into_iter(self) -> Self::IntoIter {
        self.b.iter()
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Multiple values
// ─────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct MultiValue(VecDeque<Value>);

impl Deref for MultiValue {
    type Target = VecDeque<Value>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for MultiValue {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl MultiValue {
    pub const fn new() -> MultiValue {
        MultiValue(VecDeque::new())
    }

    pub fn with_capacity(capacity: usize) -> MultiValue {
        MultiValue(VecDeque::with_capacity(capacity))
    }

    pub fn from_vec(vec: Vec<Value>) -> MultiValue {
        MultiValue(vec.into())
    }

    pub fn into_vec(self) -> Vec<Value> {
        self.0.into()
    }
}

impl From<Vec<Value>> for MultiValue {
    fn from(v: Vec<Value>) -> Self {
        MultiValue::from_vec(v)
    }
}

impl From<MultiValue> for Vec<Value> {
    fn from(m: MultiValue) -> Self {
        m.into_vec()
    }
}

impl FromIterator<Value> for MultiValue {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
        MultiValue(iter.into_iter().collect())
    }
}

impl IntoIterator for MultiValue {
    type Item = Value;
    type IntoIter = std::collections::vec_deque::IntoIter<Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a MultiValue {
    type Item = &'a Value;
    type IntoIter = std::collections::vec_deque::Iter<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Any number of trailing values of one type.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Variadic<T>(Vec<T>);

impl<T> Variadic<T> {
    pub const fn new() -> Variadic<T> {
        Variadic(Vec::new())
    }

    pub fn with_capacity(capacity: usize) -> Variadic<T> {
        Variadic(Vec::with_capacity(capacity))
    }
}

impl<T> Deref for Variadic<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.0
    }
}

impl<T> DerefMut for Variadic<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.0
    }
}

impl<T> From<Vec<T>> for Variadic<T> {
    fn from(v: Vec<T>) -> Self {
        Variadic(v)
    }
}

impl<T> From<Variadic<T>> for Vec<T> {
    fn from(v: Variadic<T>) -> Self {
        v.0
    }
}

impl<T> FromIterator<T> for Variadic<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Variadic(iter.into_iter().collect())
    }
}

impl<T> IntoIterator for Variadic<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Chunks
// ─────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkMode {
    Text,
    Binary,
}

/// What `Lua::load` takes.
pub trait AsChunk {
    fn source(&self) -> Vec<u8>;
    fn name(&self) -> Option<std::string::String> {
        None
    }
}

impl AsChunk for &str {
    fn source(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}

impl AsChunk for std::string::String {
    fn source(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}

impl AsChunk for &std::string::String {
    fn source(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}

impl AsChunk for &[u8] {
    fn source(&self) -> Vec<u8> {
        self.to_vec()
    }
}

impl<const N: usize> AsChunk for &[u8; N] {
    fn source(&self) -> Vec<u8> {
        self.to_vec()
    }
}

impl AsChunk for Vec<u8> {
    fn source(&self) -> Vec<u8> {
        self.clone()
    }
}

impl AsChunk for &Vec<u8> {
    fn source(&self) -> Vec<u8> {
        (*self).clone()
    }
}

impl AsChunk for Cow<'_, str> {
    fn source(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}

impl AsChunk for Cow<'_, [u8]> {
    fn source(&self) -> Vec<u8> {
        self.to_vec()
    }
}

pub struct Chunk<'a> {
    lua: &'a Lua,
    source: Vec<u8>,
    name: std::string::String,
    mode: Option<ChunkMode>,
    env: Option<Table>,
}

impl<'a> Chunk<'a> {
    pub fn set_name(mut self, name: impl Into<std::string::String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_mode(mut self, mode: ChunkMode) -> Self {
        self.mode = Some(mode);
        self
    }

    pub fn set_environment(mut self, env: Table) -> Self {
        self.env = Some(env);
        self
    }

    fn compile(&self, src: &[u8]) -> Result<Function> {
        if src.first() == Some(&0x1b) {
            return Err(Error::SyntaxError {
                message: "attempt to load a binary chunk".into(),
                incomplete_input: false,
            });
        }
        let lua = self.lua;
        let f = lua
            .vm()
            .load(src, self.name.as_bytes())
            .map_err(|e| Error::SyntaxError {
                message: e.to_string(),
                incomplete_input: e.0.ends_with(b"near `<eof>'"),
            })?;
        let f = Function(lua.make_ref(f));
        if let Some(env) = &self.env {
            f.set_environment(env.clone())?;
        }
        Ok(f)
    }

    pub fn into_function(self) -> Result<Function> {
        self.compile(&self.source)
    }

    pub fn exec(self) -> Result<()> {
        self.call::<()>(())
    }

    pub fn call<R: FromLuaMulti>(self, args: impl IntoLuaMulti) -> Result<R> {
        self.into_function()?.call(args)
    }

    /// As mlua: first as an expression (`return <chunk>`), then as statements.
    pub fn eval<R: FromLuaMulti>(self) -> Result<R> {
        let mut expr = b"return ".to_vec();
        expr.extend_from_slice(&self.source);
        match self.compile(&expr) {
            Ok(f) => f.call(()),
            Err(_) => self.call(()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Host data
// ─────────────────────────────────────────────────────────────────────────────────────────────

pub struct AppDataRef<'a, T: 'static> {
    r: Ref<'a, T>,
    _cell: Rc<RefCell<T>>,
}

impl<T> Deref for AppDataRef<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.r
    }
}

pub struct AppDataRefMut<'a, T: 'static> {
    r: RefMut<'a, T>,
    _cell: Rc<RefCell<T>>,
}

impl<T> Deref for AppDataRefMut<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.r
    }
}

impl<T> DerefMut for AppDataRefMut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.r
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Debugging
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// One stack level.
pub struct Debug {
    info: benilla_lua::vm::FrameInfo,
}

pub struct DebugSource<'a> {
    pub source: Option<Cow<'a, str>>,
    pub short_src: Option<Cow<'a, str>>,
    pub line_defined: Option<usize>,
    pub last_line_defined: Option<usize>,
    pub what: &'static str,
}

pub struct DebugNames<'a> {
    pub name: Option<Cow<'a, str>>,
    pub name_what: Option<&'static str>,
}

impl Debug {
    pub fn source(&self) -> DebugSource<'_> {
        let i = &self.info;
        let (source, short_src) = match (i.what, &i.source) {
            ("tail", _) => (
                Some(Cow::Borrowed("=(tail call)")),
                Some(Cow::Borrowed("(tail call)")),
            ),
            ("C", _) | (_, None) => (Some(Cow::Borrowed("=[C]")), Some(Cow::Borrowed("[C]"))),
            (_, Some(s)) => {
                let short = benilla_lua::chunkid_n(s, 60);
                (
                    Some(Cow::Owned(
                        std::string::String::from_utf8_lossy(s).into_owned(),
                    )),
                    Some(Cow::Owned(
                        std::string::String::from_utf8_lossy(&short).into_owned(),
                    )),
                )
            }
        };
        DebugSource {
            source,
            short_src,
            line_defined: (i.what != "C" && i.what != "tail").then_some(i.line_defined as usize),
            last_line_defined: None,
            what: i.what,
        }
    }

    pub fn names(&self) -> DebugNames<'_> {
        DebugNames {
            name: self
                .info
                .name
                .as_ref()
                .map(|n| Cow::Owned(std::string::String::from_utf8_lossy(n).into_owned())),
            name_what: (!self.info.namewhat.is_empty()).then_some(self.info.namewhat),
        }
    }

    pub fn current_line(&self) -> Option<usize> {
        self.info.current_line.map(|l| l as usize)
    }

    pub fn curr_line(&self) -> i32 {
        self.info.current_line.map_or(-1, |l| l as i32)
    }

    pub fn is_tail_call(&self) -> bool {
        self.info.what == "tail"
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HookTriggers {
    pub on_calls: bool,
    pub on_returns: bool,
    pub every_line: bool,
    pub every_nth_instruction: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmState {
    Continue,
    Yield,
}
