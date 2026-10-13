//! The front end against the VM benilla runs today: every Lua chunk in the addon corpus and the
//! install's interface files, compiled by both. The fork is 5.1's lexer under a 5.0 grammar, so
//! the two may disagree only where 5.0's lexer differs; every disagreement is printed.

use std::path::{Path, PathBuf};

struct Chunk {
    name: String,
    bytes: Vec<u8>,
}

/// `benilla_ui::source::chunk`: one leading UTF-8 BOM and a leading `#` line go.
fn prepare(bytes: &[u8]) -> Vec<u8> {
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    match b.first() {
        Some(b'#') => match b.iter().position(|&c| c == b'\n') {
            Some(nl) => b[nl..].to_vec(),
            None => Vec::new(),
        },
        _ => b.to_vec(),
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Inline `<Script>` text and every handler body in an XML file, each as its own chunk.
fn xml_chunks(name: &str, bytes: &[u8], out: &mut Vec<Chunk>) {
    let text =
        String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes))
            .into_owned();
    let Ok(doc) = roxmltree::Document::parse(&text) else {
        return;
    };
    for n in doc.descendants().filter(|n| n.is_element()) {
        let tag = n.tag_name().name();
        let inside_scripts = n.parent().is_some_and(|p| p.tag_name().name() == "Scripts");
        if (tag == "Script" && n.attribute("file").is_none()) || inside_scripts {
            let body: String = n.children().filter_map(|c| c.text()).collect();
            if !body.trim().is_empty() {
                out.push(Chunk {
                    name: format!("{name}:{tag}"),
                    bytes: body.into_bytes(),
                });
            }
        }
    }
}

fn corpus_chunks(out: &mut Vec<Chunk>) -> bool {
    let Some(root) = benilla_formats::addon_corpus() else {
        return false;
    };
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.sort();
    for f in files {
        let name = f.strip_prefix(&root).unwrap().display().to_string();
        let ext = f
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        let Ok(bytes) = std::fs::read(&f) else {
            continue;
        };
        match ext.as_deref() {
            Some("lua") => out.push(Chunk {
                name,
                bytes: prepare(&bytes),
            }),
            Some("xml") => xml_chunks(&name, &bytes, out),
            _ => {}
        }
    }
    true
}

fn chain_chunks(out: &mut Vec<Chunk>) -> bool {
    let Some(data) = benilla_formats::wow_data() else {
        return false;
    };
    let chain = benilla_formats::open_chain(&data).expect("open the patch chain");
    let mut names: Vec<String> = chain
        .list()
        .expect("list the chain")
        .into_iter()
        .map(|e| e.name)
        .filter(|n| n.to_ascii_lowercase().starts_with("interface\\"))
        .collect();
    names.sort();
    names.dedup();
    for name in names {
        let lower = name.to_ascii_lowercase();
        let Ok(bytes) = chain.read(&name) else {
            continue;
        };
        if lower.ends_with(".lua") {
            out.push(Chunk {
                name,
                bytes: prepare(&bytes),
            });
        } else if lower.ends_with(".xml") {
            xml_chunks(&name, &bytes, out);
        }
    }
    true
}

#[test]
fn every_chunk_compiles_as_the_fork_compiles_it() {
    let mut chunks = Vec::new();
    let corpus = corpus_chunks(&mut chunks);
    let chain = chain_chunks(&mut chunks);
    if !corpus && !chain {
        benilla_formats::skipped("no addon corpus and no WoW install", &[]);
        return;
    }
    let lua = mlua::Lua::new();
    let (mut both_ok, mut both_err, mut ours_only, mut fork_only) = (0, 0, Vec::new(), Vec::new());
    let mut msg_diff = Vec::new();
    for c in &chunks {
        let chunkname = format!("@{}", c.name);
        let ours = benilla_lua::parse(&c.bytes, chunkname.as_bytes());
        let fork = lua.load(&c.bytes[..]).set_name(&chunkname).into_function();
        match (ours, fork) {
            (Ok(_), Ok(_)) => both_ok += 1,
            (Err(a), Err(b)) => {
                both_err += 1;
                let b = match b {
                    mlua::Error::SyntaxError { message, .. } => message,
                    other => other.to_string(),
                };
                if a.to_string() != b {
                    msg_diff.push(format!("{}\n    ours: {}\n    fork: {}", c.name, a, b));
                }
            }
            (Err(a), Ok(_)) => ours_only.push(format!("{}: {}", c.name, a)),
            (Ok(_), Err(b)) => fork_only.push(format!("{}: {}", c.name, b)),
        }
    }
    println!(
        "{} chunks: {both_ok} compile in both, {both_err} fail in both, {} fail only here, {} only in the fork",
        chunks.len(),
        ours_only.len(),
        fork_only.len()
    );
    for l in &ours_only {
        println!("REJECTED HERE ONLY  {l}");
    }
    for l in &fork_only {
        println!("REJECTED BY FORK ONLY  {l}");
    }
    for l in msg_diff.iter().take(40) {
        println!("MESSAGE  {l}");
    }
    assert!(
        ours_only.is_empty() && fork_only.is_empty(),
        "the two front ends disagree"
    );
}
