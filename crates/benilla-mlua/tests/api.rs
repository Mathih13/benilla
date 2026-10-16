//! The API answers as mlua 0.11's does over the C Lua build, for the calls benilla makes.

use benilla_mlua::{Function, Lua};

/// `Debug::function` is the function running at the level: `lua_getinfo(L, "f", ar)`.
#[test]
fn a_stack_level_names_its_function() {
    let lua = Lua::new();
    let seen = lua
        .create_function(|lua, ()| {
            let mine = lua.inspect_stack(0, |d| d.function()).unwrap();
            let caller = lua.inspect_stack(1, |d| d.function()).unwrap();
            Ok((mine, caller))
        })
        .unwrap();
    lua.globals().set("seen", seen.clone()).unwrap();
    let (mine, caller, f): (Function, Function, Function) = lua
        .load("function F() local a, b = seen() return a, b, F end return F()")
        .eval()
        .unwrap();
    assert_eq!(mine, seen);
    assert_eq!(caller, f);
}
