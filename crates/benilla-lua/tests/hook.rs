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
        let n = run(&mut vm, "return n").parse::<u32>().unwrap();
        assert!((5000..=20000).contains(&n), "{n} tail calls ran");
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
