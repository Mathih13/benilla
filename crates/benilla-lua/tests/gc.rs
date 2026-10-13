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
