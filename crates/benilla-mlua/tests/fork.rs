//! A fork is independent: host closures it copied act on the fork, not on the original.

use benilla_mlua::{Lua, Table};

#[test]
fn a_copied_host_closure_acts_on_its_own_fork() {
    let lua = Lua::new();
    let store = lua.create_table().unwrap();
    store.set("n", 0).unwrap();
    let held = store.clone();
    let bump = lua
        .create_function(move |_, ()| {
            let n: i64 = held.get("n")?;
            held.set("n", n + 1)
        })
        .unwrap();
    lua.globals().set("bump", bump).unwrap();
    lua.globals().set("store", store).unwrap();
    lua.load("bump()").exec().unwrap();

    let fork = lua.fork();
    fork.load("bump() bump()").exec().unwrap();
    let n = |l: &Lua| {
        l.globals()
            .get::<Table>("store")
            .unwrap()
            .get::<i64>("n")
            .unwrap()
    };
    assert_eq!(n(&lua), 1, "the original is untouched");
    assert_eq!(n(&fork), 3, "the fork's own table counted");
    lua.load("bump()").exec().unwrap();
    assert_eq!((n(&lua), n(&fork)), (2, 3));
}
