//! The front end over every Lua chunk in the addon corpus and the install's interface files: each
//! compiles, but for the corpus chunks the script host's C Lua build rejected, which must fail
//! with the message it gave.

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

/// The chunks the C Lua build rejected, with its messages: 5.1 constructs the 1.12 grammar lacks,
/// and handlers written as prose.
const REJECTED: &[(&str, &str)] = &[
    ("AceGUI/AceGUIDropDownMenu.lua", "AceGUI/AceGUIDropDownMenu.lua:14: `<name>' expected near `<eof>'"),
    ("AckisRecipeList/ARLDataminer.lua", "AckisRecipeList/ARLDataminer.lua:2: unexpected symbol near `#'"),
    ("AckisRecipeList/ARLFrame.lua", "AckisRecipeList/ARLFrame.lua:1043: unexpected symbol near `#'"),
    ("AckisRecipeList/AckisRecipeList.lua", "AckisRecipeList/AckisRecipeList.lua:1604: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceAddon-3.0/AceAddon-3.0.lua", "AckisRecipeList/libs/AceAddon-3.0/AceAddon-3.0.lua:63: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceConfig-3.0/AceConfigCmd-3.0/AceConfigCmd-3.0.lua", "AckisRecipeList/libs/AceConfig-3.0/AceConfigCmd-3.0/AceConfigCmd-3.0.lua:43: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceConfig-3.0/AceConfigDialog-3.0/AceConfigDialog-3.0.lua", "...ecipeList/libs/AceConfig-3.0/AceConfigDialog-3.0/AceConfigDialog-3.0.lua:76: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceConfig-3.0/AceConfigRegistry-3.0/AceConfigRegistry-3.0.lua", "...eList/libs/AceConfig-3.0/AceConfigRegistry-3.0/AceConfigRegistry-3.0.lua:43: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceConsole-3.0/AceConsole-3.0.lua", "AckisRecipeList/libs/AceConsole-3.0/AceConsole-3.0.lua:31: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceEvent-3.0/AceEvent-3.0.lua", "AckisRecipeList/libs/AceEvent-3.0/AceEvent-3.0.lua:70: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceGUI-3.0/AceGUI-3.0.lua", "AckisRecipeList/libs/AceGUI-3.0/AceGUI-3.0.lua:72: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-DropDown-Items.lua", "AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-DropDown-Items.lua:7: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-DropDown.lua", "AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-DropDown.lua:8: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-Icon.lua", "AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-Icon.lua:30: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-Label.lua", "AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-Label.lua:83: unexpected symbol near `...'"),
    ("AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-TabGroup.lua", "AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-TabGroup.lua:173: unexpected symbol near `#'"),
    ("AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-TreeGroup.lua", "AckisRecipeList/libs/AceGUI-3.0/widgets/AceGUIWidget-TreeGroup.lua:316: unexpected symbol near `#'"),
    ("AckisRecipeList/libs/CallbackHandler-1.0/CallbackHandler-1.0.lua", "AckisRecipeList/libs/CallbackHandler-1.0/CallbackHandler-1.0.lua:91: unexpected symbol near `...'"),
    ("Enchantrix/EnchantrixBarker.xml:OnValueChanged", "Enchantrix/EnchantrixBarker.xml:OnValueChanged:1: `=' expected near `need'"),
    ("FixCTGroups/FixCTGroups.xml:OnLoad", "FixCTGroups/FixCTGroups.xml:OnLoad:2: `=' expected near `pressing'"),
    ("FixCTGroups/FixCTGroupsCells.xml:OnLoad", "FixCTGroups/FixCTGroupsCells.xml:OnLoad:2: `=' expected near `pressing'"),
    ("FuBar_AtlasFu/Libs/Tablet-2.0/Tablet-2.0.lua", "FuBar_AtlasFu/Libs/Tablet-2.0/Tablet-2.0.lua:150: unexpected symbol near `#'"),
    ("TipBuddy/TipBuddy.xml:OnEnter", "TipBuddy/TipBuddy.xml:OnEnter:3: `=' expected near `TipBuddy'"),
    ("TipBuddy/TipBuddy.xml:OnUpdate", "TipBuddy/TipBuddy.xml:OnUpdate:3: `=' expected near `should'"),
];

#[test]
fn every_chunk_compiles_as_the_c_build_compiled_it() {
    let mut chunks = Vec::new();
    let corpus = corpus_chunks(&mut chunks);
    let chain = chain_chunks(&mut chunks);
    if !corpus && !chain {
        benilla_formats::skipped("no addon corpus and no WoW install", &[]);
        return;
    }
    let got: Vec<(&str, String)> = chunks
        .iter()
        .filter_map(|c| {
            let chunkname = format!("@{}", c.name);
            let e = benilla_lua::check(&c.bytes, chunkname.as_bytes()).err()?;
            Some((c.name.as_str(), e.to_string()))
        })
        .collect();
    // The rejected chunks are all the corpus's.
    let want: Vec<(&str, String)> = REJECTED
        .iter()
        .filter(|_| corpus)
        .map(|&(n, m)| (n, m.to_string()))
        .collect();
    println!("{} chunks, {} rejected", chunks.len(), got.len());
    assert_eq!(got, want);
}
