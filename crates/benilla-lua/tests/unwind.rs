//! A Rust panic that unwinds out of the VM leaves it as it was when the host called in.

fn boom(_: &mut benilla_lua::Vm, _: usize) -> Result<usize, benilla_lua::LuaError> {
    panic!("a native broke")
}

#[test]
fn a_panic_through_the_vm_leaves_it_usable() {
    let mut vm = benilla_lua::new_state();
    vm.register("boom", boom);
    vm.exec(
        b"function deep(n) if n == 0 then return boom() end return deep(n - 1) + 1 end",
        b"=t",
    )
    .unwrap();
    for _ in 0..300 {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = vm.exec(b"local t = setmetatable({}, {__index = function() return deep(20) end}) return t.x", b"=t");
        }));
        assert!(r.is_err());
    }
    // No frame, depth or handler leaked: a chunk still compiles and deep calls still run.
    let r = vm
        .exec(b"local function f(n) if n == 0 then return 0 end return 1 + f(n - 1) end return f(5000)", b"=t")
        .unwrap();
    assert_eq!(vm.display(r[0]), "5000");
    let caught = vm.exec(b"return pcall(error, 'x')", b"=t").unwrap();
    assert_eq!(vm.display(caught[0]), "false");
}

/// A panic that passes a protected call leaves its handler behind no more: a native that
/// catches the panic and raises runs no handler of a call that already ended.
#[test]
fn a_panic_through_xpcall_leaves_no_stale_handler() {
    fn boom(_: &mut benilla_lua::Vm, _: usize) -> Result<usize, benilla_lua::LuaError> {
        panic!("boom");
    }
    fn catcher(vm: &mut benilla_lua::Vm, nargs: usize) -> Result<usize, benilla_lua::LuaError> {
        let f = vm.arg(nargs, 0);
        let r =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vm.pcall_value(f, &[], 0)));
        assert!(r.is_err(), "the panic reached the catcher");
        Err(vm.rt_error("after"))
    }
    let mut vm = benilla_lua::new_state();
    vm.register("boom", boom);
    vm.register("catcher", catcher);
    let r = vm
        .exec(
            b"local ok, e = pcall(catcher, function() xpcall(boom, function(m) Hit = true return m end) end) \
              return ok, e, Hit == nil",
            b"=t",
        )
        .unwrap();
    let got: Vec<String> = r.into_iter().map(|v| vm.display(v)).collect();
    assert_eq!(got, ["false", "after", "true"]);
}
