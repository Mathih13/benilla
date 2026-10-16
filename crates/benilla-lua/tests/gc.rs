//! The collector: garbage goes, what is reachable stays intact, weak tables drop their dead.

use benilla_lua::Value;

fn run(vm: &mut benilla_lua::Vm, src: &str) -> String {
    match vm.exec(src.as_bytes(), b"=t") {
        Ok(v) => v.first().map_or(String::new(), |&v| vm.display(v)),
        Err(e) => format!("<raised> {}", vm.display(e)),
    }
}

#[test]
fn garbage_is_swept_and_reachable_values_survive() {
    let mut vm = benilla_lua::new_state();
    run(
        &mut vm,
        "keep = {} for i = 1, 20000 do local t = {i, tostring(i)} if math.mod(i, 100) == 0 then table.insert(keep, t) end end",
    );
    vm.full_gc();
    let tables = vm.heap.counts()[1];
    assert!(
        tables < 1000,
        "20000 garbage tables should be gone, {tables} live"
    );
    assert_eq!(
        run(&mut vm, "local s = 0 for _, t in ipairs(keep) do s = s + t[1] assert(t[2] == tostring(t[1])) end return getn(keep) .. ':' .. s"),
        "200:2010000"
    );
}

#[test]
fn closures_keep_their_upvalues_through_collections() {
    let mut vm = benilla_lua::new_state();
    vm.gc_stress = true;
    let r = run(
        &mut vm,
        "local fs = {} for i = 1, 50 do local s = string.rep('x', i) fs[i] = function() return s end end \
         local junk = {} for i = 1, 500 do junk[i] = {tostring(i)} end junk = nil \
         local n = 0 for i = 1, 50 do n = n + string.len(fs[i]()) end return n",
    );
    assert_eq!(r, "1275");
}

#[test]
fn weak_tables_drop_dead_entries() {
    let mut vm = benilla_lua::new_state();
    run(
        &mut vm,
        "wk = setmetatable({}, {__mode = 'k'}) wv = setmetatable({}, {__mode = 'v'}) \
         held = {} for i = 1, 10 do local k = {} wk[k] = i wv[i] = {} if i <= 3 then held[i] = k end end \
         wv[11] = 'strings stay'",
    );
    vm.full_gc();
    assert_eq!(
        run(&mut vm, "local a, b = 0, 0 for k in pairs(wk) do a = a + 1 end for k in pairs(wv) do b = b + 1 end return a .. ',' .. b .. ',' .. tostring(wv[11])"),
        "3,1,strings stay"
    );
}

#[test]
fn a_freed_string_slot_is_safe_to_reuse_as_a_key() {
    let mut vm = benilla_lua::new_state();
    // Keys die, their nodes stay with nil values; new strings take the slots.
    run(
        &mut vm,
        "t = {} for i = 1, 300 do t['key' .. i] = i end for i = 1, 300 do t['key' .. i] = nil end",
    );
    vm.full_gc();
    assert_eq!(
        run(&mut vm, "for i = 1, 300 do t['new' .. i] = i end local n = 0 for k, v in pairs(t) do n = n + v end return n .. ',' .. t.new150"),
        "45150,150"
    );
    let _ = Value::Nil;
}

/// Forks share their prototypes: a collection in one must not stop the other marking the
/// constants of the code they share.
#[test]
fn forks_sharing_code_each_keep_its_constants() {
    let mut a = benilla_lua::new_state();
    run(
        &mut a,
        "function f() return 'a constant string, longer than inline' .. 1 end",
    );
    let mut b = a.clone();
    for _ in 0..4 {
        a.full_gc();
        b.full_gc();
        // Churn, so a freed constant's slot is reused.
        run(&mut a, "for i = 1, 2000 do local s = 'x' .. i end");
        run(&mut b, "for i = 1, 2000 do local s = 'y' .. i end");
    }
    let want = "a constant string, longer than inline1";
    assert_eq!(run(&mut a, "return f()"), want);
    assert_eq!(run(&mut b, "return f()"), want);
}

/// The built-in `next` stays a root with no global naming it: a generic `for` steps a table in
/// place when its iterator is that function, so a new function taking its freed id would be
/// stepped as `next` instead of called.
#[test]
fn the_builtin_next_outlives_its_globals() {
    for mut vm in [
        benilla_lua::new_state(),
        benilla_lua::new_script_host_state(),
    ] {
        run(&mut vm, "next = nil pairs = nil ipairs = nil foreach = nil");
        vm.full_gc();
        assert_eq!(
            run(
                &mut vm,
                "local stubs = {} for i = 1, 2000 do stubs[i] = function(t, k) return nil end end \
                 local n = 0 for i = 1, 2000 do for k in stubs[i], {1, 2, 3} do n = n + 1 end end \
                 return n",
            ),
            "0"
        );
    }
}

/// A vararg call's `arg` table is an allocation like any other: a loop of nothing but vararg
/// calls collects as it goes (5.1.5's `adjust_varargs` checks the collector).
#[test]
fn vararg_calls_collect_as_they_go() {
    let mut vm = benilla_lua::new_state();
    run(&mut vm, "local function f(...) return arg.n end for i = 1, 200000 do f(i, i) end");
    let tables = vm.heap.counts()[1];
    assert!(
        tables < 50000,
        "200000 `arg` tables should not wait for a collection, {tables} live"
    );
}
