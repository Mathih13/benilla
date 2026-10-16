//! A native holds `&mut Vm`, so safe code could put another VM or heap in place of the running
//! one, freeing the code and stack the running frames point into. The interpreter stops with a
//! panic there; it never runs on.

use benilla_lua::Vm;

fn replace_heap(vm: &mut Vm, _nargs: usize) -> benilla_lua::api::R<usize> {
    vm.heap = Default::default();
    Ok(0)
}

fn replace_vm(vm: &mut Vm, _nargs: usize) -> benilla_lua::api::R<usize> {
    *vm = benilla_lua::new_state();
    Ok(0)
}

fn run_with(native: benilla_lua::heap::NativeFn) -> String {
    let mut vm = benilla_lua::new_state();
    let f = vm.new_native("replace", native);
    vm.set_global("replace", f);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // In a protected call, inside a Lua function, with a loop after: every way on would
        // read the freed frames.
        vm.exec(
            b"local function g() replace() local s = 0 for i = 1, 10 do s = s + i end return s end \
              return pcall(g)",
            b"=t",
        )
    }));
    match r {
        Ok(_) => "ran on".to_owned(),
        Err(p) => p
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_default(),
    }
}

#[test]
fn a_native_that_replaces_the_heap_stops_the_vm() {
    assert_eq!(
        run_with(replace_heap),
        "a native replaced the running Lua VM or its heap"
    );
}

#[test]
fn a_native_that_replaces_the_vm_stops_it() {
    assert_eq!(
        run_with(replace_vm),
        "a native replaced the running Lua VM or its heap"
    );
}
