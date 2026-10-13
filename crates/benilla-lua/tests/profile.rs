//! The profiler charges time to the chunk that ran it, and the census charges tables to the
//! chunk that made them.

fn source_id(vm: &benilla_lua::Vm, name: &[u8]) -> usize {
    vm.heap
        .sources
        .iter()
        .position(|s| &**s == name)
        .expect("a compiled chunk")
}

#[test]
fn time_and_tables_land_on_the_chunk_that_spent_and_made_them() {
    let mut vm = benilla_lua::new_state();
    vm.exec(
        b"function Heavy() local s = 0 for i = 1, 200000 do s = s + i end return s end \
          function Maker() Kept = {} for i = 1, 500 do Kept[i] = {i} end end",
        b"@Interface\\AddOns\\Heavy\\Heavy.lua",
    )
    .unwrap();
    vm.exec(
        b"function Light() return 1 end function Both() Light() return Heavy() end",
        b"@Interface\\AddOns\\Light\\Light.lua",
    )
    .unwrap();
    vm.set_profiling(true);
    vm.exec(b"for i = 1, 50 do Both() end Maker()", b"=driver")
        .unwrap();
    let p = vm.profile.as_ref().unwrap().clone();
    let heavy = source_id(&vm, b"@Interface\\AddOns\\Heavy\\Heavy.lua");
    let light = source_id(&vm, b"@Interface\\AddOns\\Light\\Light.lua");
    let ns = |i: usize| p.ns.get(i).copied().unwrap_or(0);
    println!("heavy {} ns, light {} ns", ns(heavy), ns(light));
    assert!(
        ns(heavy) > 20 * ns(light),
        "the loop's chunk carries the time"
    );
    let census = vm.heap.census();
    assert!(
        census[heavy].tables >= 501,
        "Maker's tables: {:?}",
        census[heavy]
    );
    assert!(census[light].tables < 5, "{:?}", census[light]);
    // Off, nothing more is charged.
    vm.set_profiling(false);
    assert!(vm.profile.is_none());
}
