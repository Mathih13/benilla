//! Inputs that once crashed the process from Lua source: each must raise a catchable Lua error
//! instead, in a debug build, on a 2 MB thread.

/// Run `src` in the script host's state on a 2 MB thread; its results as text, or the error.
fn run(src: &'static str) -> Result<Vec<String>, String> {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let mut vm = benilla_lua::new_script_host_state();
            match vm.exec(src.as_bytes(), b"=t") {
                Ok(vs) => Ok(vs.into_iter().map(|v| vm.display(v)).collect()),
                Err(e) => Err(vm.display(e)),
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

/// A result past `MAX_STRING` is 5.1's `not enough memory`, before anything is allocated.
#[test]
fn a_huge_string_rep_is_not_enough_memory() {
    assert_eq!(
        run(r#"return pcall(string.rep, string.rep("x", 1024), 2^31 - 1)"#),
        Ok(vec!["false".into(), "not enough memory".into()])
    );
    assert_eq!(
        run(r#"return pcall(string.rep, "xy", 2^31 - 1)"#),
        Ok(vec!["false".into(), "not enough memory".into()])
    );
    assert_eq!(
        run(r#"return string.rep("ab", 3), string.rep("", 2^31 - 1), string.rep("x", -1)"#),
        Ok(vec!["ababab".into(), "".into(), "".into()])
    );
}

/// A match nested past the matcher's bound is 5.2's `pattern too complex`, where 5.1 overflows
/// the stack: through `find`, `gsub` and `gfind`, and under the deepest native nesting the VM
/// allows.
#[test]
fn a_deep_match_is_pattern_too_complex() {
    let deep = r#"local n = 20000
        local s, p = string.rep("a", n), string.rep("a?", n)
        local ok1, e1 = pcall(string.find, s, p)
        local ok2, e2 = pcall(string.gsub, s, p, "")
        local ok3, e3 = pcall(function() for w in string.gfind(s, p) do end end)
        return ok1, e1, ok2, e2, ok3, e3"#;
    let msg = "pattern too complex";
    let got = run(deep).unwrap();
    assert_eq!(got.len(), 6, "{got:?}");
    for (i, r) in got.chunks(2).enumerate() {
        assert_eq!(r[0], "false", "call {i}: {r:?}");
        assert!(r[1].ends_with(msg), "call {i}: {r:?}");
    }
    let nested = r#"local s, p = string.rep("a", 20000), string.rep("a?", 20000)
        local d = 0
        local function f()
          d = d + 1
          local ok, e = pcall(string.find, s, p)
          if not ok and not string.find(e, "too complex", 1, true) then return e end
          local r = string.gsub("x", "x", f)
          return r
        end
        local ok, e = pcall(f)
        return d, e"#;
    let got = run(nested).unwrap();
    assert!(got[0].parse::<u32>().unwrap() > 150, "{got:?}");
    assert!(got[1].ends_with("C stack overflow"), "{got:?}");
    // Below the bound a match as deep as a pattern can get still runs.
    assert_eq!(
        run(r#"return string.find(string.rep("a", 9000), string.rep("a?", 9000))"#),
        Ok(vec!["1".into(), "9000".into()])
    );
}
