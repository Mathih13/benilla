//! Values. Every collectable is a handle into the [`crate::heap::Heap`]'s arenas, so a value is
//! `Copy` and the whole state can be cloned.

/// A string handle with its 5.0 hash beside it, so a table can place a string key without a
/// trip to the heap. Strings are interned: one handle per distinct byte string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StrRef {
    pub id: u32,
    pub hash: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TableRef(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FuncRef(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UserRef(pub u32);

#[derive(Clone, Copy, Debug, Default)]
pub enum Value {
    #[default]
    Nil,
    Bool(bool),
    Num(f64),
    Str(StrRef),
    Table(TableRef),
    /// A Lua closure or a native function.
    Func(FuncRef),
    /// A full userdata: a frame or other host object, with a metatable.
    User(UserRef),
    /// 5.0's light userdata: a bare host pointer, compared by value.
    Light(usize),
}

impl Value {
    pub fn is_nil(self) -> bool {
        matches!(self, Value::Nil)
    }

    /// Lua truth: only `nil` and `false` are false.
    pub fn truthy(self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }

    /// `luaO_rawequalObj`.
    pub fn raw_eq(self, other: Value) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Num(a), Value::Num(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a.id == b.id,
            (Value::Table(a), Value::Table(b)) => a == b,
            (Value::Func(a), Value::Func(b)) => a == b,
            (Value::User(a), Value::User(b)) => a == b,
            (Value::Light(a), Value::Light(b)) => a == b,
            _ => false,
        }
    }

    /// `luaT_typenames`, 5.0's spelling.
    pub fn type_name(self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Table(_) => "table",
            Value::Func(_) => "function",
            Value::User(_) | Value::Light(_) => "userdata",
        }
    }
}
