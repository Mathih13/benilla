//! Which addon a chunk belongs to, from its chunk name.

/// The addon a chunk name belongs to: the folder of an `@Interface\AddOns\<Folder>\…` file,
/// `FrameXML` for the stock interface, `benilla` for benilla's own layer. An XML handler's chunk
/// (`<frame>:<script>`) names no file, so it goes to the addon whose file was compiled just before
/// it: the loader compiles an addon's handlers while that addon loads.
pub(super) fn owner_names(sources: &[std::rc::Rc<[u8]>]) -> Vec<String> {
    let mut out = Vec::with_capacity(sources.len());
    let mut current = String::from("FrameXML");
    for s in sources {
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
        out.push(owner);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handlers_follow_the_file_compiled_before_them() {
        let s = |x: &str| std::rc::Rc::from(x.as_bytes());
        let names = owner_names(&[
            s("=[host]"),
            s("@Interface\\FrameXML\\UIParent.lua"),
            s("UIParent:OnUpdate"),
            s("@Interface\\AddOns\\Bagnon\\Bagnon.lua"),
            s("BagnonFrame:OnUpdate"),
            s("@Interface\\AddOns\\!OmniCC\\OmniCC.lua"),
        ]);
        assert_eq!(
            names,
            ["benilla", "FrameXML", "FrameXML", "Bagnon", "Bagnon", "!OmniCC"]
        );
    }
}
