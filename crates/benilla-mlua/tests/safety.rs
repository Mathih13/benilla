//! The state's lifetime and its handles: no cycle keeps a state alive, a callback's panic
//! leaves the state sound, a string read stays put, and another state's value is refused.

use std::cell::Cell;
use std::rc::Rc;

use benilla_mlua::{Function, Lua, Table, Value};

/// Sets its flag when dropped: kept in a state's app data, it says whether the state went.
struct Sentinel(Rc<Cell<bool>>);

impl Drop for Sentinel {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

/// A panic payload's text.
fn text(p: &(dyn std::any::Any + Send)) -> Option<&str> {
    p.downcast_ref::<&str>()
        .copied()
        .or_else(|| p.downcast_ref::<String>().map(String::as_str))
}

fn watched(lua: &Lua) -> Rc<Cell<bool>> {
    let gone = Rc::new(Cell::new(false));
    lua.set_app_data(Sentinel(gone.clone()));
    gone
}

#[test]
fn a_state_whose_closures_capture_its_handles_is_freed() {
    let lua = Lua::new();
    let gone = watched(&lua);
    let t = lua.create_table().unwrap();
    let s = lua.create_string("held").unwrap();
    let f = lua.create_function(|_, ()| Ok(1)).unwrap();
    let (ht, hs, hf) = (t.clone(), s.clone(), f.clone());
    let keep = lua
        .create_function(move |_, ()| {
            let _ = (&ht, &hs, &hf);
            Ok(())
        })
        .unwrap();
    lua.globals().set("keep", keep).unwrap();
    lua.globals().set("t", t).unwrap();
    drop((s, f));
    drop(lua);
    assert!(
        gone.get(),
        "the VM and its app data went with the last `Lua`"
    );
}

#[test]
fn a_handle_outliving_its_state_panics_on_use() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    drop(lua);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.raw_len()));
    assert_eq!(text(&*r.unwrap_err()), Some("Lua instance is destroyed"));
    // Dropping it touches nothing.
    drop(t);
}

fn panicking(lua: &Lua) -> Function {
    lua.create_function(|_, ()| -> benilla_mlua::Result<()> { panic!("callback broke") })
        .unwrap()
}

#[test]
fn a_callback_panic_is_a_lua_error_pcall_catches() {
    let lua = Lua::new();
    lua.globals().set("boom", panicking(&lua)).unwrap();
    let got: String = lua
        .load(
            "local ok, e = pcall(boom) return tostring(ok) .. ' ' .. type(e) .. ' ' .. tostring(e)",
        )
        .eval()
        .unwrap();
    assert_eq!(got, "false userdata callback broke");
    // The state still runs.
    assert_eq!(lua.load("return 1 + 1").eval::<i64>().unwrap(), 2);
}

#[test]
fn an_uncaught_callback_panic_resumes_in_rust() {
    let lua = Lua::new();
    lua.globals().set("boom", panicking(&lua)).unwrap();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lua.load("boom()").exec()));
    let payload = r.expect_err("the panic comes back out");
    assert_eq!(text(&*payload), Some("callback broke"));
    assert_eq!(lua.load("return 2 + 2").eval::<i64>().unwrap(), 4);
}

#[test]
fn a_panic_in_a_fork_leaves_the_template_sound() {
    let template = Lua::new();
    template.globals().set("x", 7).unwrap();
    template
        .globals()
        .set("boom", panicking(&template))
        .unwrap();
    let fork = template.fork();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fork.load("boom()").exec()));
    assert!(r.is_err());
    let caught: bool = fork.load("return not pcall(boom)").eval().unwrap();
    assert!(caught);
    drop(fork);
    // The template reads its own VM, not the dropped fork's.
    assert_eq!(template.globals().get::<i64>("x").unwrap(), 7);
    for _ in 0..1000 {
        template.create_string("filler").unwrap();
    }
    assert_eq!(template.globals().get::<i64>("x").unwrap(), 7);
}

#[test]
fn a_string_read_stays_put_while_the_heap_grows() {
    let lua = Lua::new();
    let s = lua.create_string("short sentinel").unwrap();
    let long = lua
        .create_string("a sentinel longer than the inline limit")
        .unwrap();
    let (b, l) = (s.as_bytes(), long.to_str().unwrap());
    let keep: Vec<_> = (0..10_000)
        .map(|i| lua.create_string(format!("s{i}")).unwrap())
        .collect();
    lua.gc_collect().unwrap();
    assert_eq!(&*b, b"short sentinel");
    assert_eq!(&*l, "a sentinel longer than the inline limit");
    drop(keep);
}

#[test]
fn another_states_value_is_refused() {
    let a = Lua::new();
    let b = Lua::new();
    let foreign: Table = a.create_table().unwrap();
    foreign.set("x", 1).unwrap();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        b.globals().set("foreign", foreign.clone())
    }));
    assert_eq!(
        text(&*r.unwrap_err()),
        Some("Lua instance passed Value created from a different main Lua state")
    );
    // A fork is another state too, outside a callback.
    let fork = a.fork();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fork.globals().set("t", Value::Table(foreign.clone()))
    }));
    assert!(r.is_err());
    assert_eq!(
        b.load("return foreign").eval::<Value>().unwrap(),
        Value::Nil
    );
}

/// A freed host closure's captures drop after the collection, as host code on the VM: a
/// destructor that uses the state reaches it soundly, and runs.
#[test]
fn a_destructor_run_by_the_collector_can_use_the_state() {
    struct D(Lua, Rc<Cell<bool>>);
    impl Drop for D {
        fn drop(&mut self) {
            let f = self.0.create_function(|_, ()| Ok(5)).unwrap();
            assert_eq!(f.call::<i64>(()).unwrap(), 5);
            self.1.set(true);
        }
    }
    let lua = Lua::new();
    let ran = Rc::new(Cell::new(false));
    let d = D(lua.clone(), ran.clone());
    let f = lua
        .create_function(move |_, ()| {
            let _ = &d;
            Ok(())
        })
        .unwrap();
    drop(f);
    lua.gc_collect().unwrap();
    assert!(ran.get(), "the destructor ran");
    // And from a collection the interpreter starts itself.
    let ran = Rc::new(Cell::new(false));
    let d = D(lua.clone(), ran.clone());
    let f = lua
        .create_function(move |_, ()| {
            let _ = &d;
            Ok(())
        })
        .unwrap();
    drop(f);
    lua.load("for i = 1, 20000 do local t = {} end")
        .exec()
        .unwrap();
    assert!(
        ran.get(),
        "the destructor ran inside the interpreter's collection"
    );
}
