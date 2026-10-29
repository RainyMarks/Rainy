//! Files embedded in Rainy.exe: the Rainy mark and the Lucide icons Rainy uses beyond GPUI Kit's default set.

use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{AssetSource, SharedString};

icon_assets!(ExtraIcons, [
    Archive, Brain, Bug, CirclePlus, CircleStop, Clock, CodeXml, FilePen, FilePlus, FileSearch, FolderPlus, GitBranch, GitCompare, ListTree, MessageSquarePlus,
    Paperclip, Pin, PinOff, Play, RotateCcw, Send, Shield, ShieldAlert, ShieldCheck, Sparkles, Square, SquarePen, Terminal, Trash, Wrench, Zap,
]);

const BRAND: &[(&str, &[u8])] = &[("brand/rainy-mark.svg", include_bytes!("../assets/brand/rainy-mark.svg"))];

/// Rainy's files first, then the selected icons, then GPUI Kit's component icons.
pub struct RainyAssets;

impl AssetSource for RainyAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = BRAND.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut names: Vec<SharedString> =
            BRAND.iter().map(|(name, _)| *name).filter(|name| name.starts_with(path)).map(Into::into).collect();
        names.extend(ExtraIcons.list(path)?);
        names.extend(Assets.list(path)?);
        names.sort();
        names.dedup();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Lucide icon the interface names through `gpui_kit::assets::IconName` loads: GPUI Kit bundles only its
    /// component icons, and a missing one draws nothing.
    #[test]
    fn every_icon_named_in_the_interface_is_bundled() {
        let pattern = regex::Regex::new(r"assets::IconName::([A-Za-z0-9]+)").unwrap();
        let mut missing = Vec::new();
        let mut sources = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(path) = sources.pop() {
            for entry in std::fs::read_dir(&path).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    sources.push(path);
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for found in pattern.captures_iter(&text) {
                    let name = &found[1];
                    let kebab: String = name.chars().enumerate().flat_map(|(at, c)| {
                        let dash = (at > 0 && (c.is_ascii_uppercase() || (c.is_ascii_digit() && !name[..at].ends_with(|p: char| p.is_ascii_digit())))).then_some('-');
                        dash.into_iter().chain(c.to_lowercase())
                    }).collect();
                    let file = format!("icons/{kebab}.svg");
                    if RainyAssets.load(&file).ok().flatten().is_none() {
                        missing.push(format!("{name} ({file}) in {}", path.display()));
                    }
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "icons not bundled: {missing:#?}");
    }
}
