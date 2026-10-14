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
