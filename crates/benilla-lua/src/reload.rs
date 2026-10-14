//! Hot reload: new code for a chunk's functions, swapped into the closures that already exist.
//! Nothing reruns, so every table, upvalue and registration the chunk made stays; a handler a
//! frame holds, a hook in another addon's table, a callback in a list all run the new code from
//! their next call. A frame already executing finishes on the old code.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::Bytes;
use crate::heap::FuncObj;
use crate::proto::Proto;
use crate::vm::Vm;
use crate::SyntaxError;

/// What a reload did, by function key.
#[derive(Debug, Default)]
pub struct Report {
    /// Live closures now running new code.
    pub closures: usize,
    /// Functions whose live closures were swapped.
    pub swapped: Vec<String>,
    /// Functions left on the old code, a full reload's work: their captured variables changed,
    /// or their key is numbered and the count of functions sharing its name changed, so it may
    /// name another function now.
    pub reshaped: Vec<String>,
    /// Functions only the new source has; they exist once their definition runs again.
    pub added: Vec<String>,
    /// Functions only the old source had, with live closures still on the old code.
    pub removed: Vec<String>,
}

fn walk(p: &Rc<Proto>, out: &mut HashMap<Bytes, Rc<Proto>>) {
    out.insert(p.key.clone(), p.clone());
    for q in &p.protos {
        walk(q, out);
    }
}

fn show(k: &[u8]) -> String {
    String::from_utf8_lossy(k).into_owned()
}

impl Vm {
    /// Compile `src` as the chunk `chunkname` and swap each of its functions into the live
    /// closures of the same function. A syntax error leaves everything as it was.
    pub fn hot_reload(&mut self, chunkname: &[u8], src: &[u8]) -> Result<Report, SyntaxError> {
        let root = crate::compile::compile(src, chunkname, &mut self.heap, 0)?;
        let mut fresh = HashMap::new();
        walk(&root, &mut fresh);
        let mut report = Report::default();
        let mut seen: HashMap<Bytes, bool> = HashMap::new();
        // Replaced code may still be running in a frame; it is kept until no Lua runs.
        if self.frames_empty() {
            self.retired.clear();
        }
        let mut retired = Vec::new();
        for f in self.heap.funcs_mut() {
            let FuncObj::Lua {
                proto,
                upvals,
                shape,
                ..
            } = f
            else {
                continue;
            };
            // The main function ran once; it is not swapped.
            if &*proto.source != chunkname || proto.key.is_empty() {
                continue;
            }
            let key = proto.key.clone();
            let Some(new) = fresh.get(&key) else {
                seen.entry(key).or_insert(false);
                continue;
            };
            // The new function's upvalues, by name, from the closure's existing ones; none when
            // the key may now name another function.
            let remap: Option<Vec<u32>> = if new.key_counts != proto.key_counts {
                None
            } else {
                new.upval_names
                    .iter()
                    .map(|d| {
                        proto
                            .upval_names
                            .iter()
                            .position(|o| o == d)
                            .map(|i| upvals[i])
                    })
                    .collect()
            };
            match remap {
                Some(ups) => {
                    *shape = crate::heap::Shape::of(new);
                    let old_proto = std::mem::replace(proto, new.clone());
                    let old_ups = std::mem::replace(upvals, ups.into_boxed_slice());
                    retired.push((old_proto, old_ups));
                    report.closures += 1;
                    seen.insert(key, true);
                }
                None => {
                    report.reshaped.push(show(&key));
                    seen.entry(key).or_insert(false);
                }
            }
        }
        self.retired.extend(retired);
        for (k, swapped) in &seen {
            if *swapped {
                report.swapped.push(show(k));
            } else if !fresh.contains_key(k) {
                report.removed.push(show(k));
            }
        }
        for k in fresh.keys() {
            if !k.is_empty() && !seen.contains_key(k) {
                report.added.push(show(k));
            }
        }
        report.swapped.sort();
        report.reshaped.sort();
        report.reshaped.dedup();
        report.added.sort();
        report.removed.sort();
        Ok(report)
    }
}
