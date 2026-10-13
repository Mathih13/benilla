//! Hot reload: an addon's functions take new code in place, and everything that holds them
//! (a handler list, another addon's hook, a captured local helper) runs it, with state kept.

const NAME: &[u8] = b"@Interface\\AddOns\\MyAddon\\MyAddon.lua";

const V1: &str = r#"
MyAddon = { count = 0 }
local prefix = "v1"
local function helper(x) return x * 2 end
function MyAddon:OnEvent(event)
  self.count = self.count + 1
  return prefix .. ":" .. event .. ":" .. self.count
end
function MyAddon.Double(x) return helper(x) end
Handlers = {}
table.insert(Handlers, function(e) return MyAddon:OnEvent(e) end)
"#;

/// `OnEvent` and the local `helper` change; the top level is the same.
const V2: &str = r#"
MyAddon = { count = 0 }
local prefix = "v1"
local function helper(x) return x * 3 end
function MyAddon:OnEvent(event)
  self.count = self.count + 1
  return "NEW " .. prefix .. ":" .. event .. ":" .. self.count
end
function MyAddon.Double(x) return helper(x) end
Handlers = {}
table.insert(Handlers, function(e) return MyAddon:OnEvent(e) end)
"#;

/// `OnEvent` now captures a local the old closure has no slot for.
const V3: &str = r#"
MyAddon = { count = 0 }
local prefix = "v1"
local suffix = "!"
local function helper(x) return x * 3 end
function MyAddon:OnEvent(event)
  self.count = self.count + 1
  return prefix .. ":" .. event .. suffix
end
function MyAddon.Double(x) return helper(x) end
Handlers = {}
table.insert(Handlers, function(e) return MyAddon:OnEvent(e) end)
"#;

fn run(vm: &mut benilla_lua::Vm, src: &str) -> String {
    match vm.exec(src.as_bytes(), b"=probe") {
        Ok(v) => v.first().map_or(String::new(), |&v| vm.display(v)),
        Err(e) => format!("<raised> {}", vm.display(e)),
    }
}

#[test]
fn edited_functions_run_new_code_and_keep_their_state() {
    let mut vm = benilla_lua::new_state();
    vm.exec(V1.as_bytes(), NAME).unwrap();
    // Another addon keeps a direct reference, as SetScript or a hook does.
    run(&mut vm, "OtherAddonHook = MyAddon.OnEvent");
    assert_eq!(run(&mut vm, "return Handlers[1]('A')"), "v1:A:1");
    assert_eq!(
        run(&mut vm, "return OtherAddonHook(MyAddon, 'B')"),
        "v1:B:2"
    );
    assert_eq!(run(&mut vm, "return MyAddon.Double(5)"), "10");

    let report = vm.hot_reload(NAME, V2.as_bytes()).unwrap();
    println!("{report:#?}");
    assert!(report.reshaped.is_empty() && report.added.is_empty() && report.removed.is_empty());

    // The count carried on; the list's closure, the other addon's reference and the local
    // helper behind `Double` all run the new code.
    assert_eq!(run(&mut vm, "return Handlers[1]('C')"), "NEW v1:C:3");
    assert_eq!(
        run(&mut vm, "return OtherAddonHook(MyAddon, 'D')"),
        "NEW v1:D:4"
    );
    assert_eq!(run(&mut vm, "return MyAddon.Double(5)"), "15");
}

#[test]
fn a_function_whose_captures_changed_is_reported_and_left_running() {
    let mut vm = benilla_lua::new_state();
    vm.exec(V1.as_bytes(), NAME).unwrap();
    let report = vm.hot_reload(NAME, V3.as_bytes()).unwrap();
    assert_eq!(report.reshaped, vec!["MyAddon.OnEvent".to_string()]);
    // The old code still answers; the rest of the file swapped.
    assert_eq!(run(&mut vm, "return Handlers[1]('A')"), "v1:A:1");
    assert_eq!(run(&mut vm, "return MyAddon.Double(5)"), "15");
}

#[test]
fn a_syntax_error_changes_nothing() {
    let mut vm = benilla_lua::new_state();
    vm.exec(V1.as_bytes(), NAME).unwrap();
    let err = vm
        .hot_reload(NAME, b"function MyAddon:OnEvent( return end")
        .unwrap_err();
    assert!(err.to_string().contains("MyAddon.lua:1:"), "{err}");
    assert_eq!(run(&mut vm, "return Handlers[1]('A')"), "v1:A:1");
}

#[test]
fn a_reload_then_a_snapshot_restore_goes_back_to_the_old_code() {
    let mut vm = benilla_lua::new_state();
    vm.exec(V1.as_bytes(), NAME).unwrap();
    let before = vm.clone();
    vm.hot_reload(NAME, V2.as_bytes()).unwrap();
    assert_eq!(run(&mut vm, "return MyAddon.Double(1)"), "3");
    vm = before;
    assert_eq!(run(&mut vm, "return MyAddon.Double(1)"), "2");
}
