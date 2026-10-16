//! Hot reload: an edited `.lua` file under the addon folder takes effect in the running UI, its
//! functions swapped into the live closures (`benilla_lua::reload`), state and registrations
//! kept. The files the VM loaded are checked twice a second; each reload is reported in the chat
//! frame.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use benilla_ui::mlua::{ObjectLike, Table};
use benilla_ui::script::UiScript;
use bevy::prelude::*;

/// How often the files are checked.
const POLL_SECS: f32 = 0.5;
/// Reloads the profiler panel lists.
pub(super) const LOG_LINES: usize = 6;

#[derive(Resource, Default)]
pub(crate) struct HotReload {
    /// The VM session the files below were read from.
    session: u64,
    /// Each source id's file on disk, for a chunk the loader read from the addon folder, as of
    /// source epoch `epoch` (`Heap::sources_epoch`): a freed slot may hold another chunk later.
    files: Vec<Option<PathBuf>>,
    epoch: u64,
    /// Each chunk name's file, resolved once.
    resolved: HashMap<Box<[u8]>, Option<PathBuf>>,
    mtimes: HashMap<PathBuf, SystemTime>,
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
    let (Some(root), Some(script)) = (crate::ui_script::addons::root(), script) else {
        return;
    };
    let hr = &mut *hr;
    if hr.session != script.session() {
        hr.session = script.session();
        hr.files.clear();
        hr.epoch = u64::MAX;
        hr.mtimes.clear();
    }
    script.lua().with_vm(|vm| {
        let heap = &vm.heap;
        if hr.epoch != heap.sources_epoch() {
            hr.epoch = heap.sources_epoch();
            let resolved = &mut hr.resolved;
            hr.files = heap
                .sources
                .iter()
                .map(|name| {
                    let name = name.as_ref()?;
                    if let Some(p) = resolved.get(&name[..]) {
                        return p.clone();
                    }
                    let p = file_of(&root, &String::from_utf8_lossy(name));
                    resolved.insert(Box::from(&name[..]), p.clone());
                    p
                })
                .collect();
        }
    });
    let mut changed = Vec::new();
    for (id, path) in hr.files.iter().enumerate() {
        let Some(path) = path else { continue };
        let Ok(t) = std::fs::metadata(path).and_then(|m| m.modified()) else {
            continue;
        };
        // A file first seen is recorded, not reloaded.
        if hr
            .mtimes
            .insert(path.clone(), t)
            .is_some_and(|old| old != t)
        {
            changed.push((id, path.clone()));
        }
    }
    for (id, path) in changed {
        let (line, ok) = reload(&script, &root, id, &path);
        info!("devtools: {line}");
        chat(&script, &line, ok);
        hr.log.push_back((now, line, ok));
        while hr.log.len() > LOG_LINES {
            hr.log.pop_front();
        }
    }
}

/// The file a chunk was read from: the loader names it `@Interface\AddOns\<Folder>\<file>`, in
/// the `.toc`'s spelling, matched on disk case-insensitively as the loader matched it.
fn file_of(root: &Path, chunk: &str) -> Option<PathBuf> {
    let rel = chunk.strip_prefix('@')?;
    benilla_assets::loose_addon_file(root, &benilla_assets::normalize_path(rel))
}

/// Reload one file into the chunk the loader compiled it as, source `id`; the report line and
/// whether it applied.
fn reload(script: &UiScript, root: &Path, id: usize, path: &Path) -> (String, bool) {
    let shown = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    let Ok(bytes) = std::fs::read(path) else {
        return (format!("{shown}: unreadable"), false);
    };
    let src = benilla_ui::source::chunk(&bytes);
    let r = script.lua().with_vm(|vm| {
        let name = vm.heap.source_name(id as u32).cloned()?;
        Some(vm.hot_reload(&name, src))
    });
    let Some(r) = r else {
        return (format!("{shown}: no longer loaded"), false);
    };
    match r {
        Ok(r) => report_line(&shown, &r),
        Err(e) => (format!("{shown}: {e}"), false),
    }
}

/// What a reload did, in a line, and whether all of it took effect.
fn report_line(shown: &str, r: &benilla_ui::mlua::benilla_lua::reload::Report) -> (String, bool) {
    // A new function nested in one that was swapped exists from that one's next call; any other
    // is made by code that already ran.
    let (live, later): (Vec<&String>, Vec<&String>) = r.added.iter().partition(|k| {
        k.rsplit_once('/')
            .is_some_and(|(parent, _)| r.swapped.iter().any(|s| s == parent))
    });
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
    if !live.is_empty() {
        line += &format!("; {} new, live on their parent's next call", live.len());
    }
    if !later.is_empty() {
        line += &format!("; {} new, live after /reload", later.len());
    }
    if !r.removed.is_empty() {
        line += &format!(
            "; {} removed, still on the old code where held: {}",
            r.removed.len(),
            r.removed.join(", ")
        );
    }
    if r.top_level_changed {
        line += "; top-level code changed, which runs again only on /reload";
    }
    let complete = r.reshaped.is_empty() && later.is_empty() && !r.top_level_changed;
    (line, complete)
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

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_ui::mlua::benilla_lua::reload::Report;

    #[test]
    fn the_line_says_what_did_not_take_effect() {
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let r = Report {
            closures: 2,
            swapped: s(&["Outer", "F:SetScript"]),
            added: s(&["Outer/inner", "NewGlobal"]),
            removed: s(&["Old"]),
            top_level_changed: true,
            ..Default::default()
        };
        let (line, complete) = report_line("A.lua", &r);
        assert_eq!(
            line,
            "A.lua: 2 function(s) swapped into 2 closure(s); 1 new, live on their parent's next \
             call; 1 new, live after /reload; 1 removed, still on the old code where held: Old; \
             top-level code changed, which runs again only on /reload"
        );
        assert!(!complete);
        let r = Report {
            closures: 1,
            swapped: s(&["Outer"]),
            added: s(&["Outer/inner"]),
            ..Default::default()
        };
        assert!(
            report_line("A.lua", &r).1,
            "a nested addition is live at once"
        );
    }
}
