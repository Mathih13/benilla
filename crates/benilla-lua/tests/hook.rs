//! The instruction-budget hook: it reaches every loop a script can write, and code it runs does
//! not run it again.

use std::cell::Cell;
use std::rc::Rc;

use benilla_lua::Vm;

fn run(vm: &mut Vm, src: &str) -> String {
    match vm.exec(src.as_bytes(), b"=t") {
        Ok(v) => v.first().map_or(String::new(), |&v| vm.display(v)),
        Err(e) => format!("<raised> {}", vm.display(e)),
    }
}

/// A hook that raises once it has run `limit` times.
fn budget(vm: &mut Vm, limit: u32) -> Rc<Cell<u32>> {
    let fired = Rc::new(Cell::new(0));
    let seen = fired.clone();
    vm.set_hook(
        1000,
        Rc::new(move |vm: &mut Vm| {
            seen.set(seen.get() + 1);
            if seen.get() >= limit {
                let v = vm.str(b"budget");
                return Err(vm.throw(v));
            }
            Ok(())
        }),
    );
    fired
}

/// On a 2 MB thread, as the brief's crash tests run.
fn small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn the_budget_stops_a_tail_recursive_loop() {
    small_stack(|| {
        let mut vm = benilla_lua::new_state();
        budget(&mut vm, 10);
        assert_eq!(
            run(
                &mut vm,
                "n = 0 local function f() n = n + 1 return f() end return f()"
            ),
            "<raised> budget"
        );
        // Ten runs of a 1000-instruction step, five instructions a call.
        let n = run(&mut vm, "return n").parse::<u32>().unwrap();
        assert!((1900..=2100).contains(&n), "{n} tail calls ran");
    });
}

#[test]
fn code_the_hook_runs_does_not_run_the_hook() {
    small_stack(|| {
        let mut vm = benilla_lua::new_state();
        run(
            &mut vm,
            "function Busy() local s = 0 for i = 1, 100000 do s = s + i end return s end",
        );
        let fired = Rc::new(Cell::new(0u32));
        let seen = fired.clone();
        vm.set_hook(
            100,
            Rc::new(move |vm: &mut Vm| {
                seen.set(seen.get() + 1);
                let busy = vm.get_global("Busy");
                vm.call_value(busy, &[], 1)?;
                Ok(())
            }),
        );
        assert_eq!(
            run(
                &mut vm,
                "local s = 0 for i = 1, 10000 do s = s + i end return s"
            ),
            "50005000"
        );
        // About one run per 100 instructions of the outer loop, none from `Busy`'s.
        let n = fired.get();
        assert!((10..=1000).contains(&n), "the hook ran {n} times");
    });
}

/// The instructions `src` runs, counted by a hook at every one.
fn instructions(src: &str) -> u64 {
    let mut vm = benilla_lua::new_state();
    let n = Rc::new(Cell::new(0u64));
    let seen = n.clone();
    vm.set_hook(
        1,
        Rc::new(move |_: &mut Vm| {
            seen.set(seen.get() + 1);
            Ok(())
        }),
    );
    let r = run(&mut vm, src);
    assert!(!r.starts_with("<raised>"), "{r}");
    n.get()
}

/// Each instruction a function runs counts, as 5.1's `LUA_MASKCOUNT` counts it: a call costs its
/// body, and Lua a library function calls (a `gsub` replacement, a `sort` comparator, `foreach`,
/// a metamethod, a generic `for`'s iterator) counts as any other.
#[test]
fn the_hook_counts_every_instruction_lua_runs() {
    // `F` runs two more instructions a call than `f`, its two moves.
    let fns = "local function f(x) return x end \
               local function F(x) local a = x local b = a return b end ";
    let it = "local function it(s, c) local x = c + 1 if x <= 100 then return x end end \
              local function IT(s, c) local y = c local z = y local x = z + 1 if x <= 100 then return x end end ";
    for (what, light, heavy, calls) in [
        (
            "a call",
            "for i = 1, 100 do f(i) end",
            "for i = 1, 100 do F(i) end",
            100,
        ),
        (
            "gsub",
            "string.gsub(string.rep('a', 100), 'a', f)",
            "string.gsub(string.rep('a', 100), 'a', F)",
            100,
        ),
        (
            "sort",
            "table.sort({3, 1, 2}, function(a, b) return f(a) < b end)",
            "table.sort({3, 1, 2}, function(a, b) return F(a) < b end)",
            0,
        ),
        (
            "foreach",
            "table.foreach({1, 2, 3, 4}, function(k, v) f(v) end)",
            "table.foreach({1, 2, 3, 4}, function(k, v) F(v) end)",
            4,
        ),
        (
            "__index",
            "local t = setmetatable({}, {__index = f}) for i = 1, 50 do local _ = t.x end",
            "local t = setmetatable({}, {__index = F}) for i = 1, 50 do local _ = t.x end",
            50,
        ),
        (
            "iterator",
            "for i in it, nil, 0 do end",
            "for i in IT, nil, 0 do end",
            101,
        ),
    ] {
        let (a, b) = (
            instructions(&format!("{fns}{it}{light}")),
            instructions(&format!("{fns}{it}{heavy}")),
        );
        assert!(b > a, "{what}: {a} instructions, then {b}");
        // `sort` calls its comparator as often as its algorithm needs: some number of times.
        if what == "sort" {
            assert_eq!((b - a) % 2, 0, "{what}: {a} instructions, then {b}");
        } else {
            assert_eq!(b - a, 2 * calls, "{what}: {a} instructions, then {b}");
        }
    }
}
