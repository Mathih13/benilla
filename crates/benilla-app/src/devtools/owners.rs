//! Which addon a chunk belongs to, from its chunk name.

/// The addon a chunk name belongs to: the folder of an `@Interface\AddOns\<Folder>\…` file,
/// `FrameXML` for the stock interface, `benilla` for benilla's own layer. An XML handler's chunk
/// (`<frame>:<script>`) names no file, so it goes to the addon whose file was compiled just before
/// it: the loader compiles an addon's handlers while that addon loads, so an addon whose handlers
/// come before any of its `.lua` files is charged to the one before it. A chunk compiled later from
/// a string (`loadstring`, `RunScript`, a binding's body) names no addon either, so it goes to the
/// addon loaded last: an approximation the per-addon time carries.
/// Indexed by source id: `heap.sources` has `slots` slots, and `in_order` lists the live ones in
/// compile order (`Heap::sources_in_order`), which slot order is not once a slot is reused.
pub(super) fn owner_names<'a>(
    slots: usize,
    in_order: impl IntoIterator<Item = (u32, &'a [u8])>,
) -> Vec<String> {
    let mut out = vec![String::new(); slots];
    let mut current = String::from("FrameXML");
    for (id, s) in in_order {
        let name = String::from_utf8_lossy(s);
        let lower = name.to_ascii_lowercase();
        let owner = if let Some(rest) = lower.strip_prefix("@interface\\addons\\") {
            let folder_len = rest.find('\\').unwrap_or(rest.len());
            // The folder in the chunk name's own spelling.
            let start = "@interface\\addons\\".len();
            let folder = name[start..start + folder_len].to_string();
            current = folder.clone();
            folder
        } else if lower.starts_with("@interface\\framexml") || lower.starts_with("@interface\\glue")
        {
            current = "FrameXML".into();
            current.clone()
        } else if name.starts_with('@') || name.starts_with('=') {
            "benilla".into()
        } else {
            current.clone()
        };
        out[id as usize] = owner;
    }
    out
}

/// [`owner_names`] for a heap.
pub(super) fn owners_of(heap: &benilla_ui::mlua::benilla_lua::heap::Heap) -> Vec<String> {
    let in_order = heap.sources_in_order();
    owner_names(
        heap.sources.len(),
        in_order.iter().map(|(id, name)| (*id, &name[..])),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handlers_follow_the_file_compiled_before_them() {
        // Slot order differs from compile order, as once a slot is reused.
        let order: [(u32, &str); 6] = [
            (0, "=[host]"),
            (3, "@Interface\\FrameXML\\UIParent.lua"),
            (1, "UIParent:OnUpdate"),
            (5, "@Interface\\AddOns\\Bagnon\\Bagnon.lua"),
            (2, "BagnonFrame:OnUpdate"),
            (4, "@Interface\\AddOns\\!OmniCC\\OmniCC.lua"),
        ];
        let by_id = owner_names(6, order.iter().map(|&(i, n)| (i, n.as_bytes())));
        let names: Vec<&str> = order
            .iter()
            .map(|&(i, _)| by_id[i as usize].as_str())
            .collect();
        assert_eq!(
            names,
            ["benilla", "FrameXML", "FrameXML", "Bagnon", "Bagnon", "!OmniCC"]
        );
    }
}
