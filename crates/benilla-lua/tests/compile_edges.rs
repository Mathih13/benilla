//! Programs the C build compiles whose code generation leaves 5.1's own invariants unmet; a
//! debug build must compile and run them as the C build does.

fn run(src: &str) -> String {
    let mut vm = benilla_lua::new_state();
    match vm.exec(src.as_bytes(), b"=t") {
        Ok(v) => v.first().map_or("none".into(), |&v| vm.display(v)),
        Err(e) => vm.display(e),
    }
}

/// 5.1's `forlist` adjusts the values to three without freeing a fourth, so the loop's block
/// opens with a register above its locals. A local declared first in the body is then read
/// from a register its value did not go to: the C build and lua5.1 raise the same error.
#[test]
fn a_generic_for_with_more_than_three_values_compiles() {
    assert_eq!(
        run("for k in function() end, nil, nil, 42 do end return 7"),
        "7"
    );
    assert_eq!(
        run("local n = 0 for k in function(s, c) if c ~= 3 then return (c or 0) + 1 end end, nil, nil, 9, 8 do \
             do local y = k n = n + y end end return n"),
        "t:1: attempt to perform arithmetic on local `y' (a nil value)"
    );
}

/// The same leaked register under a `return` in the loop's body: 5.1 asserts the values sit at
/// the first free local, which they do not, and returns from there, as the C build does.
#[test]
fn a_return_in_a_generic_for_with_more_than_three_values_compiles() {
    let f = "local function f(s, c) if not c then return 1 end end ";
    let r = |body: &str| run(&format!("{f}{body}"));
    assert_eq!(r("for k in f, nil, nil, 42 do return k, 5 end"), "nil");
    assert_eq!(
        r("local function g(x) return 'g' .. tostring(x) end for k in f, nil, nil, 42 do return g(k) end"),
        "g1"
    );
}
