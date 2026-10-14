//! The Lua inspector, the dev chord + `E`: the UI's live Lua state as a tree, read straight from
//! benilla-lua's heap. An expression picks the root, and opening it picks the frame under the
//! cursor, else `_G`; a statement runs as a REPL line. Tables, metatables and a closure's upvalues
//! expand in place, each function with its source line and owning addon, a frame with its name and
//! its script handlers, and a scalar field or upvalue edits in place, written raw. Every value is read from the root each frame, and the
//! root is a VM root, so a collection keeps it.

use benilla_ui::mlua::benilla_lua::heap::{FuncObj, UpvalObj};
use benilla_ui::mlua::benilla_lua::value::{TableRef, V};
use benilla_ui::mlua::benilla_lua::{Value, Vm};
use benilla_ui::script::UiScript;
use benilla_world::modkeys::dev_chord;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};

use super::owners::owner_names;
use crate::debug_panel::{overlay_text, EguiDemand, OVERLAY_FILL, OVERLAY_TEXT_DIM};

/// The root an inspector opens on: the frame under the cursor, else the globals.
const OPEN_EXPR: &str = "GetMouseFocus() ~= WorldFrame and GetMouseFocus() or _G";
/// Children one table lists before it asks for the filter.
const MAX_CHILDREN: usize = 400;
/// A string value's preview, in characters.
const PREVIEW: usize = 80;
/// A text field's fill: the overlay's own background is too dark to show one.
const FIELD: egui::Color32 = egui::Color32::from_rgb(38, 42, 56);
/// A frame's name, then each handler it has and its script, through the 1.12 API.
const FRAME_HELPER: &[u8] = br#"
local H = { "OnLoad", "OnEvent", "OnUpdate", "OnShow", "OnHide", "OnEnter", "OnLeave", "OnClick",
  "OnDoubleClick", "OnMouseDown", "OnMouseUp", "OnMouseWheel", "OnDragStart", "OnDragStop",
  "OnReceiveDrag", "OnKeyDown", "OnKeyUp", "OnChar", "OnSizeChanged", "OnValueChanged",
  "OnTextChanged", "OnEnterPressed", "OnEscapePressed", "OnEditFocusGained", "OnEditFocusLost",
  "OnHyperlinkClick", "OnTooltipSetDefaultAnchor" }
return function(f)
  local ok, name = pcall(f.GetName, f)
  local out = { ok and name or false }
  for i = 1, table.getn(H) do
    local has, s = pcall(f.GetScript, f, H[i])
    if has and s then table.insert(out, H[i]) table.insert(out, s) end
  end
  return unpack(out)
end
"#;

/// Where an edited value is written back.
#[derive(Clone, Copy)]
enum Slot {
    Field(TableRef, Value),
    Upval(u32),
}

#[derive(Resource, Default)]
pub(crate) struct LuaInspector {
    open: bool,
    expr: String,
    /// The root value's VM root, and the session of the VM that holds it.
    root: Option<(u32, u64)>,
    /// Narrows the root table's keys.
    filter: String,
    /// The row under edit and the text typed so far.
    edit: Option<(egui::Id, String)>,
    /// The last evaluation's or edit's outcome, and whether it went through.
    status: Option<(String, bool)>,
    /// The owning addon of each chunk the VM has compiled.
    owners: Vec<String>,
    /// [`FRAME_HELPER`]'s function, a VM root, and the session of its VM.
    helper: Option<(u32, u64)>,
}

pub(super) fn toggle(
    keys: Res<ButtonInput<KeyCode>>,
    script: Option<NonSend<UiScript>>,
    mut ins: ResMut<LuaInspector>,
    mut demand: ResMut<EguiDemand>,
) {
    if !dev_chord(&keys, KeyCode::KeyE) {
        return;
    }
    let Some(script) = script else { return };
    ins.open = !ins.open;
    demand.inspector = ins.open;
    if ins.open {
        ins.expr = OPEN_EXPR.into();
        evaluate(&mut ins, &script);
    } else {
        release_root(&mut ins, &script);
    }
}

fn release_root(ins: &mut LuaInspector, script: &UiScript) {
    if let Some((id, session)) = ins.root.take() {
        if session == script.session() {
            script.lua().raw_vm().unroot(id);
        }
    }
}

/// `return <text>` when it compiles, else `<text>` as statements: the first result, if any.
fn run(vm: &mut Vm, text: &str) -> Result<Option<Value>, String> {
    const NAME: &[u8] = b"=[inspector]";
    let expr = format!("return {text}");
    let src = if vm.load(expr.as_bytes(), NAME).is_ok() {
        expr.as_bytes().to_vec()
    } else {
        text.as_bytes().to_vec()
    };
    match vm.exec(&src, NAME) {
        Ok(r) => Ok(r.first().copied()),
        Err(e) => Err(vm.display(e)),
    }
}

fn evaluate(ins: &mut LuaInspector, script: &UiScript) {
    let vm = script.lua().raw_vm();
    match run(vm, &ins.expr) {
        Ok(Some(v)) => {
            release_root(ins, script);
            ins.root = Some((vm.root(v), script.session()));
            ins.filter.clear();
            ins.status = None;
        }
        Ok(None) => ins.status = Some(("ran; no value".into(), true)),
        Err(e) => ins.status = Some((e, false)),
    }
    if ins.root.is_none() {
        let g = Value::Table(vm.globals);
        ins.root = Some((vm.root(g), script.session()));
    }
}

/// What one frame's drawing asks for, applied after it.
#[derive(Default)]
struct Asks {
    commit: Option<(Slot, String)>,
    evaluate: bool,
}

/// The drawing's view of the VM and the inspector.
struct Cx<'a> {
    vm: &'a mut Vm,
    owners: &'a [String],
    edit: &'a mut Option<(egui::Id, String)>,
    asks: &'a mut Asks,
    helper: Value,
}

/// A frame's name and its scripts, when `t` is a frame's table (its `[0]` is a userdata).
fn frame_info(cx: &mut Cx, t: TableRef) -> Option<(String, Vec<(String, Value)>)> {
    let zero = cx.vm.heap.table(t).get(Value::Num(0.0));
    if !matches!(zero.unpack(), V::User(_) | V::Light(_)) {
        return None;
    }
    let r = cx.vm.pcall_value(cx.helper, &[Value::Table(t)], -1).ok()?;
    let name = match r.first().map(|v| v.unpack()) {
        Some(V::Str(_)) => cx.vm.display(r[0]),
        _ => "(anonymous)".into(),
    };
    let scripts = r[1..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&[h, f]| (cx.vm.display(h), f))
        .collect();
    Some((name, scripts))
}

pub(super) fn panel_ui(
    mut contexts: EguiContexts,
    script: Option<NonSend<UiScript>>,
    mut ins: ResMut<LuaInspector>,
) -> Result {
    if !ins.open {
        return Ok(());
    }
    let Some(script) = script else { return Ok(()) };
    // A new VM (a world entry, a `/reload`) dropped the old root with the old heap.
    if ins.root.is_some_and(|(_, s)| s != script.session()) {
        ins.root = None;
        ins.owners.clear();
        evaluate(&mut ins, &script);
    }
    let vm = script.lua().raw_vm();
    if ins.helper.is_none_or(|(_, s)| s != script.session()) {
        let f = vm
            .exec(FRAME_HELPER, b"=[inspector]")
            .map_err(|e| vm.display(e));
        let f = f.expect("the frame helper compiles")[0];
        ins.helper = Some((vm.root(f), script.session()));
    }
    let helper = vm.rooted(ins.helper.unwrap().0);
    if ins.owners.len() != vm.heap.sources.len() {
        ins.owners = owner_names(&vm.heap.sources);
    }
    let Some((root_id, _)) = ins.root else {
        return Ok(());
    };
    let root = vm.rooted(root_id);
    let ctx = contexts.ctx_mut()?;
    let ins = &mut *ins;
    let mut asks = Asks::default();

    egui::Window::new("Lua inspector")
        .default_pos(egui::pos2(560.0, 200.0))
        .default_size(egui::vec2(520.0, 560.0))
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(OVERLAY_FILL)
                .inner_margin(10),
        )
        .show(ctx, |ui| {
            overlay_text(ui);
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut ins.expr)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("an expression, or a line of Lua")
                        .background_color(FIELD)
                        .desired_width(ui.available_width() - 70.0),
                );
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Run").clicked() || enter {
                    asks.evaluate = true;
                }
            });
            ui.horizontal(|ui| {
                if ui.small_button("_G").clicked() {
                    ins.expr = "_G".into();
                    asks.evaluate = true;
                }
                ui.add(
                    egui::TextEdit::singleline(&mut ins.filter)
                        .hint_text("filter keys")
                        .background_color(FIELD)
                        .desired_width(160.0),
                );
                if let Some((msg, ok)) = &ins.status {
                    let c = if *ok {
                        egui::Color32::from_rgb(102, 255, 153)
                    } else {
                        egui::Color32::from_rgb(255, 102, 102)
                    };
                    ui.label(egui::RichText::new(msg).small().color(c));
                }
            });
            ui.label(
                egui::RichText::new("click a value to edit it · Enter writes · Esc cancels")
                    .small()
                    .color(OVERLAY_TEXT_DIM),
            );
            ui.separator();
            let mut cx = Cx {
                vm: &mut *vm,
                owners: &ins.owners,
                edit: &mut ins.edit,
                asks: &mut asks,
                helper,
            };
            let head = describe(&mut cx, root);
            ui.label(egui::RichText::new(head).strong().color(colour(root)));
            let filter = ins.filter.to_lowercase();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match root.unpack() {
                    V::Table(t) => children(ui, &mut cx, egui::Id::new("lua_root"), t, &filter),
                    _ => row(ui, &mut cx, egui::Id::new("lua_root"), "value", root, None),
                });
        });

    if let Some((slot, text)) = asks.commit {
        let vm = script.lua().raw_vm();
        ins.status = Some(match run(vm, &text) {
            Ok(v) => {
                write(vm, slot, v.unwrap_or(Value::Nil));
                (format!("wrote {text}"), true)
            }
            Err(e) => (e, false),
        });
    }
    if asks.evaluate {
        evaluate(ins, &script);
    }
    Ok(())
}

/// A raw write: no `__newindex`, no frame script runs.
fn write(vm: &mut Vm, slot: Slot, v: Value) {
    match slot {
        Slot::Field(t, k) => {
            let _ = vm.heap.table_mut(t).set(k, v);
        }
        Slot::Upval(i) => match vm.heap.upval(i) {
            UpvalObj::Open(s) => vm.stack[s as usize] = v,
            UpvalObj::Closed(_) => *vm.heap.upval_mut(i) = UpvalObj::Closed(v),
        },
    }
}

/// A table's pairs, numbers first in order, then strings by name, then the rest.
fn pairs(vm: &mut Vm, t: TableRef) -> Vec<(String, Value, Value)> {
    let mut out = Vec::new();
    let mut k = Value::Nil;
    while let Ok(Some((key, v))) = vm.heap.table(t).next(k) {
        out.push((key_label(vm, key), key, v));
        k = key;
    }
    out.sort_by(|a, b| {
        let rank = |v: Value| match v.unpack() {
            V::Num(n) => (0, n),
            V::Str(_) => (1, 0.0),
            _ => (2, 0.0),
        };
        let (ra, rb) = (rank(a.1), rank(b.1));
        ra.0.cmp(&rb.0)
            .then(ra.1.total_cmp(&rb.1))
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

fn key_label(vm: &mut Vm, k: Value) -> String {
    match k.unpack() {
        V::Str(_) => vm.display(k),
        V::Num(_) => format!("[{}]", vm.display(k)),
        _ => format!("[{}]", brief(vm, k)),
    }
}

/// A one-line description of any value.
fn brief(vm: &mut Vm, v: Value) -> String {
    match v.unpack() {
        V::Str(_) => {
            let s = vm.display(v);
            let mut q: String = s.chars().take(PREVIEW).collect();
            if q.len() < s.len() {
                q.push('…');
            }
            format!("{q:?}")
        }
        V::Table(t) => format!("table #{}", t.0),
        V::Func(f) => match vm.heap.func(f) {
            FuncObj::Native { name, .. } => format!("native {name}"),
            FuncObj::Lua { proto, .. } => format!(
                "function {}:{}",
                String::from_utf8_lossy(&proto.source),
                proto.line_defined
            ),
        },
        V::User(u) => format!("userdata #{}", u.0),
        V::Light(p) => format!("lightuserdata {p:#x}"),
        _ => vm.display(v),
    }
}

/// [`brief`], with a frame's name.
fn describe(cx: &mut Cx, v: Value) -> String {
    match v.unpack() {
        V::Table(t) => match frame_info(cx, t) {
            Some((name, _)) => format!("frame {name} · table #{}", t.0),
            None => brief(cx.vm, v),
        },
        _ => brief(cx.vm, v),
    }
}

/// A value as Lua source, the starting text of an edit.
fn literal(vm: &mut Vm, v: Value) -> String {
    match v.unpack() {
        V::Str(_) => format!("{:?}", vm.display(v)),
        _ => vm.display(v),
    }
}

fn colour(v: Value) -> egui::Color32 {
    match v.unpack() {
        V::Str(_) => egui::Color32::from_rgb(230, 190, 120),
        V::Num(_) => egui::Color32::from_rgb(150, 200, 255),
        V::Bool(_) | V::Nil => egui::Color32::from_rgb(200, 150, 255),
        V::Func(_) => egui::Color32::from_rgb(120, 220, 160),
        _ => egui::Color32::from_gray(200),
    }
}

fn children(ui: &mut egui::Ui, cx: &mut Cx, id: egui::Id, t: TableRef, filter: &str) {
    if let Some((_, scripts)) = frame_info(cx, t).filter(|(_, s)| !s.is_empty()) {
        let head = format!("[scripts] · {}", scripts.len());
        egui::CollapsingHeader::new(egui::RichText::new(head).color(colour(scripts[0].1)))
            .id_salt(id.with("[scripts]"))
            .default_open(true)
            .show(ui, |ui| {
                for (name, f) in &scripts {
                    row(ui, cx, id.with(("[scripts]", name)), name, *f, None);
                }
            });
    }
    let all = pairs(cx.vm, t);
    let shown: Vec<_> = all
        .iter()
        .filter(|(label, ..)| filter.is_empty() || label.to_lowercase().contains(filter))
        .take(MAX_CHILDREN)
        .collect();
    for (label, k, v) in &shown {
        row(ui, cx, id.with(label), label, *v, Some(Slot::Field(t, *k)));
    }
    let hidden = all.len() - shown.len();
    if hidden > 0 && filter.is_empty() {
        ui.label(
            egui::RichText::new(format!("… {hidden} more; narrow them with the filter"))
                .small()
                .color(OVERLAY_TEXT_DIM),
        );
    }
    if let Some(m) = cx.vm.heap.table(t).meta {
        row(
            ui,
            cx,
            id.with("[metatable]"),
            "[metatable]",
            Value::Table(m),
            None,
        );
    }
}

fn row(ui: &mut egui::Ui, cx: &mut Cx, id: egui::Id, key: &str, v: Value, slot: Option<Slot>) {
    let text = format!("{key}  {}", describe(cx, v));
    match v.unpack() {
        V::Table(t) => {
            let n = pairs_count(cx.vm, t);
            egui::CollapsingHeader::new(
                egui::RichText::new(format!("{text} · {n}")).color(colour(v)),
            )
            .id_salt(id)
            .show(ui, |ui| children(ui, cx, id, t, ""));
        }
        V::Func(f) => {
            let FuncObj::Lua { proto, upvals, .. } = cx.vm.heap.func(f).clone() else {
                ui.label(egui::RichText::new(text).color(colour(v)));
                return;
            };
            let owner = cx
                .owners
                .get(proto.source_id as usize)
                .map_or("?", String::as_str);
            let head = format!("{text} · {owner}");
            egui::CollapsingHeader::new(egui::RichText::new(head).color(colour(v)))
                .id_salt(id)
                .show(ui, |ui| {
                    if upvals.is_empty() {
                        ui.label(
                            egui::RichText::new("no upvalues")
                                .small()
                                .color(OVERLAY_TEXT_DIM),
                        );
                    }
                    for (i, &u) in upvals.iter().enumerate() {
                        let name = proto.upval_names.get(i).map_or_else(
                            || format!("upvalue {i}"),
                            |n| String::from_utf8_lossy(n).into(),
                        );
                        let uv = match cx.vm.heap.upval(u) {
                            UpvalObj::Open(s) => cx.vm.stack[s as usize],
                            UpvalObj::Closed(x) => x,
                        };
                        row(ui, cx, id.with(i), &name, uv, Some(Slot::Upval(u)));
                    }
                });
        }
        V::User(_) => match cx.vm.heap.metatable(v) {
            Some(m) => {
                egui::CollapsingHeader::new(text)
                    .id_salt(id)
                    .show(ui, |ui| {
                        row(
                            ui,
                            cx,
                            id.with("[metatable]"),
                            "[metatable]",
                            Value::Table(m),
                            None,
                        )
                    });
            }
            None => {
                ui.label(text);
            }
        },
        _ => scalar(ui, cx, id, key, v, slot),
    }
}

fn pairs_count(vm: &Vm, t: TableRef) -> usize {
    let (mut n, mut k) = (0, Value::Nil);
    while let Ok(Some((key, _))) = vm.heap.table(t).next(k) {
        n += 1;
        k = key;
    }
    n
}

/// A scalar row, edited in place when it has a slot.
fn scalar(ui: &mut egui::Ui, cx: &mut Cx, id: egui::Id, key: &str, v: Value, slot: Option<Slot>) {
    ui.horizontal(|ui| {
        ui.label(key);
        let editing = cx.edit.as_ref().is_some_and(|(e, _)| *e == id);
        if let (true, Some(slot)) = (editing, slot) {
            let (_, text) = cx.edit.as_mut().unwrap();
            let r = ui.add(
                egui::TextEdit::singleline(text)
                    .font(egui::TextStyle::Monospace)
                    .background_color(FIELD)
                    .desired_width(240.0),
            );
            r.request_focus();
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                *cx.edit = None;
            } else if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let (_, text) = cx.edit.take().unwrap();
                cx.asks.commit = Some((slot, text));
            }
            return;
        }
        let label = egui::Label::new(
            egui::RichText::new(brief(cx.vm, v))
                .monospace()
                .color(colour(v)),
        )
        .sense(egui::Sense::click());
        let r = ui.add(label);
        if slot.is_some() && r.on_hover_text("click to edit").clicked() {
            *cx.edit = Some((id, literal(cx.vm, v)));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_statements_order_and_raw_writes() {
        let mut vm = benilla_ui::mlua::benilla_lua::new_state();
        assert!(run(&mut vm, "x = 5").unwrap().is_none(), "a statement runs");
        assert!(run(&mut vm, "nil +").is_err(), "a syntax error reports");

        let src = "setmetatable({b = 1, a = 2, [2] = 3, [1] = 4}, \
                   {__newindex = function() error('not raw') end})";
        let V::Table(t) = run(&mut vm, src).unwrap().unwrap().unpack() else {
            panic!("a table")
        };
        let labels: Vec<String> = pairs(&mut vm, t).into_iter().map(|p| p.0).collect();
        assert_eq!(labels, ["[1]", "[2]", "a", "b"]);
        let k = vm.str(b"c");
        write(&mut vm, Slot::Field(t, k), Value::Num(7.0));
        let c = vm.heap.table(t).get(k);
        assert_eq!(vm.display(c), "7", "written past __newindex");

        let f = run(&mut vm, "local n = 1 return function() return n end")
            .unwrap()
            .unwrap();
        let V::Func(fr) = f.unpack() else {
            panic!("a function")
        };
        let FuncObj::Lua { upvals, .. } = vm.heap.func(fr).clone() else {
            panic!("a Lua closure")
        };
        write(&mut vm, Slot::Upval(upvals[0]), Value::Num(9.0));
        vm.set_global("f", f);
        let got = run(&mut vm, "f()").unwrap().unwrap();
        assert_eq!(vm.display(got), "9", "the closure sees the edited upvalue");
    }
}
