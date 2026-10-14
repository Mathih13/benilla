//! Hot reload: an edited `.lua` file under the addon folder takes effect in the running UI, its
//! functions swapped into the live closures (`benilla_lua::reload`), state and registrations
//! kept. The folder is polled twice a second; each reload is reported in the chat frame.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use benilla_ui::mlua::{ObjectLike, Table};
use benilla_ui::script::UiScript;
use bevy::prelude::*;

/// How often the folder is polled.
const POLL_SECS: f32 = 0.5;
/// Reloads the profiler panel lists.
pub(super) const LOG_LINES: usize = 6;

#[derive(Resource, Default)]
pub(crate) struct HotReload {
    mtimes: HashMap<PathBuf, SystemTime>,
    primed: bool,
    next_poll: f32,
    /// The latest reloads, newest last: the time, the line and whether it applied.
    pub(super) log: VecDeque<(f32, String, bool)>,
}

pub(super) fn poll(time: Res<Time>, script: Option<NonSend<UiScript>>, mut hr: ResMut<HotReload>) {
    let now = time.elapsed_secs();
    if now < hr.next_poll {
        return;
    }
    hr.next_poll = now + POLL_SECS;
    let Some(root) = crate::ui_script::addons::root() else {
        return;
    };
    let mut seen = HashMap::new();
    walk(&root, &mut seen);
    let changed: Vec<PathBuf> = seen
        .iter()
        .filter(|(p, t)| hr.primed && hr.mtimes.get(*p) != Some(*t))
        .map(|(p, _)| p.clone())
        .collect();
    hr.mtimes = seen;
    hr.primed = true;
    let Some(script) = script else { return };
    for path in changed {
        let (line, ok) = reload(&script, &root, &path);
        info!("devtools: {line}");
        chat(&script, &line, ok);
        hr.log.push_back((now, line, ok));
        while hr.log.len() > LOG_LINES {
            hr.log.pop_front();
        }
    }
}

/// Every `.lua` file under `dir`, with its modification time.
fn walk(dir: &Path, out: &mut HashMap<PathBuf, SystemTime>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("lua")) {
            if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
                out.insert(p, t);
            }
        }
    }
}

/// Reload one file into the chunk the loader compiled it as; the report line and whether it
/// applied.
fn reload(script: &UiScript, root: &Path, path: &Path) -> (String, bool) {
    let rel: Vec<String> = path
        .strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let shown = rel.join("/");
    // The loader names a file chunk `@Interface\AddOns\<Folder>\<file>` in the `.toc`'s own
    // spelling, which may differ from the disk's in case.
    let key = format!("@interface\\addons\\{}", rel.join("\\")).to_ascii_lowercase();
    let lua = script.lua();
    let vm = lua.raw_vm();
    let Some(name) = vm
        .heap
        .sources
        .iter()
        .find(|s| String::from_utf8_lossy(s).to_ascii_lowercase() == key)
        .cloned()
    else {
        return (format!("{shown}: not loaded, nothing to reload"), false);
    };
    let Ok(bytes) = std::fs::read(path) else {
        return (format!("{shown}: unreadable"), false);
    };
    let src = benilla_ui::source::chunk(&bytes);
    match vm.hot_reload(&name, src) {
        Ok(r) => {
            let mut line = format!(
                "{shown}: {} function(s) swapped into {} closure(s)",
                r.swapped.len(),
                r.closures
            );
            if !r.reshaped.is_empty() {
                line += &format!(
                    "; kept old code for {} (captures changed): {}",
                    r.reshaped.len(),
                    r.reshaped.join(", ")
                );
            }
            if !r.added.is_empty() {
                line += &format!("; {} new, live after /reload", r.added.len());
            }
            (line, r.reshaped.is_empty())
        }
        Err(e) => (format!("{shown}: {e}"), false),
    }
}

/// The line in the default chat frame: green when it applied, red when it did not.
fn chat(script: &UiScript, line: &str, ok: bool) {
    let lua = script.lua();
    let Ok(frame) = lua.globals().get::<Table>("DEFAULT_CHAT_FRAME") else {
        return;
    };
    let (r, g, b) = if ok { (0.4, 1.0, 0.6) } else { (1.0, 0.4, 0.4) };
    let _ = frame.call_method::<()>("AddMessage", (format!("hot reload: {line}"), r, g, b));
}
