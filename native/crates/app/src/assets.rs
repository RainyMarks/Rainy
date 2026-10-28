//! Files embedded in Rainy.exe: the Rainy mark and the Lucide icons Rainy uses beyond GPUI Kit's default set.

use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};
use gpui_kit::{AssetSource, SharedString};

icon_assets!(ExtraIcons, [
    Archive, Brain, Bug, CirclePlus, CircleStop, Clock, CodeXml, FilePen, FilePlus, FileSearch, FolderPlus, GitBranch, GitCompare, ListTree, MessageSquarePlus,
    Paperclip, Pin, PinOff, Play, Send, Shield, ShieldAlert, ShieldCheck, Sparkles, Square, SquarePen, Terminal, Trash, Wrench,
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
