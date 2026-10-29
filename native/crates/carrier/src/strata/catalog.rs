//! Strata's model catalog as its management server reports it (`strata_models`): the families and sizes its setup
//! offers, with the download, the memory they need, whether each fits this computer and whether it is installed, and
//! the size its setup would pick here. The words about each are Strata's own; the interface shows its own in Chinese.

use serde::Serialize;
use serde_json::Value;

/// How a size fits this computer, from the catalog's `on_this_pc` words (Strata's `strata_cli/launch/catalog.verdict`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    Fits,
    /// Runs, more slowly: low-RAM mode, or part of the experts read from the SSD.
    Slower,
    /// Close: other programs should be closed, or much is read from the SSD.
    Tight,
    /// It fits in memory, but the disk lacks room for it.
    Disk,
    /// Too big; it can still be tried.
    No,
    /// This computer's memory could not be read.
    Unknown,
}

impl Fit {
    pub fn of(words: &str) -> Self {
        let low = words.trim().to_ascii_lowercase();
        let low = low.strip_prefix("experimental: ").unwrap_or(&low);
        if low.is_empty() {
            Self::Unknown
        } else if low.starts_with("does not fit") {
            Self::No
        } else if low.contains("free disk space") {
            Self::Disk
        } else if low.contains("low-ram mode") {
            Self::Slower
        } else if low.contains("part of its experts") || low.contains("read from the ssd") {
            if low.contains("slow here") || low.starts_with("tight") { Self::Tight } else { Self::Slower }
        } else if low.starts_with("tight") {
            Self::Tight
        } else if low.contains("fits") {
            Self::Fits
        } else {
            Self::Tight
        }
    }

    /// A size worth offering first on this computer.
    pub fn good(self) -> bool {
        matches!(self, Self::Fits | Self::Slower)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Size {
    /// `Q2_0`, `IQ1_M`, `IST-DAS-2.00bpw`: what `--model` takes.
    pub model: String,
    /// The run config's tag (`strata-<id>.json`): what removing and starting an installed model name.
    pub id: String,
    pub about: String,
    pub download_gb: f64,
    /// RAM and graphics memory the weights take together.
    pub memory_gb: f64,
    pub fit: Fit,
    /// Strata's words for the fit.
    pub fit_words: String,
    pub installed: bool,
    pub experimental: bool,
    pub images: bool,
    /// The free disk a set needs beside its download (DeepSeek).
    pub disk_gb: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Family {
    /// What `--family` takes: `qwen`, `coder`, `deepseek41`.
    pub id: String,
    pub title: String,
    pub about: String,
    pub experimental: bool,
    pub sizes: Vec<Size>,
}

/// The size Strata's setup picks for this computer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Recommendation {
    pub family: String,
    pub model: String,
    pub title: String,
    pub context: u32,
    pub why: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gpu {
    pub name: String,
    pub vram_gb: f64,
    pub usable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Catalog {
    pub families: Vec<Family>,
    /// The context lengths setup offers.
    pub contexts: Vec<u32>,
    pub recommendation: Option<Recommendation>,
    /// Why nothing is recommended, when nothing fits.
    pub no_recommendation: Option<String>,
    pub ram_gb: Option<f64>,
    pub gpus: Vec<Gpu>,
}

fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn number(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

impl Catalog {
    /// The catalog of a `strata_models` answer; sizes without a name are left out.
    pub fn parse(answer: &Value) -> Self {
        let families = answer
            .get("families")
            .and_then(Value::as_array)
            .map(|families| {
                families
                    .iter()
                    .filter_map(|family| {
                        let sizes = family.get("sizes").and_then(Value::as_array).map(|sizes| {
                            sizes
                                .iter()
                                .filter(|size| !text(size, "model").is_empty())
                                .map(|size| {
                                    let words = text(size, "on_this_pc");
                                    Size {
                                        model: text(size, "model"),
                                        id: text(size, "id"),
                                        about: text(size, "about"),
                                        download_gb: number(size, "download_gb"),
                                        memory_gb: number(size, "memory_gb"),
                                        fit: Fit::of(&words),
                                        fit_words: words,
                                        installed: flag(size, "installed"),
                                        experimental: flag(size, "experimental"),
                                        images: flag(size, "images"),
                                        disk_gb: size.get("disk_gb").and_then(Value::as_f64),
                                    }
                                })
                                .collect::<Vec<_>>()
                        })?;
                        let id = text(family, "family");
                        (!id.is_empty()).then(|| Family { id, title: text(family, "title"), about: text(family, "about"), experimental: flag(family, "experimental"), sizes })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let contexts = answer.get("contexts").and_then(Value::as_array).map(|contexts| contexts.iter().filter_map(|context| context.as_u64()).filter_map(|context| u32::try_from(context).ok()).collect()).unwrap_or_default();
        let recommendation = answer.get("recommendation");
        let chosen = recommendation.filter(|value| !text(value, "model").is_empty()).map(|value| Recommendation {
            family: text(value, "family"),
            model: text(value, "model"),
            title: text(value, "title"),
            context: value.get("context").and_then(Value::as_u64).and_then(|context| u32::try_from(context).ok()).unwrap_or(32768),
            why: text(value, "why"),
            notes: value.get("notes").and_then(Value::as_array).map(|notes| notes.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default(),
        });
        let no_recommendation = if chosen.is_none() { recommendation.map(|value| text(value, "why")).filter(|why| !why.is_empty()) } else { None };
        let pc = answer.get("this_pc");
        Self {
            families,
            contexts,
            recommendation: chosen,
            no_recommendation,
            ram_gb: pc.and_then(|pc| pc.get("ram_gb")).and_then(Value::as_f64),
            gpus: pc
                .and_then(|pc| pc.get("gpus"))
                .and_then(Value::as_array)
                .map(|gpus| gpus.iter().map(|gpu| Gpu { name: text(gpu, "name"), vram_gb: number(gpu, "vram_gb"), usable: gpu.get("usable").and_then(Value::as_bool).unwrap_or(true) }).collect())
                .unwrap_or_default(),
        }
    }

    pub fn size(&self, family: &str, model: &str) -> Option<(&Family, &Size)> {
        let family = self.families.iter().find(|candidate| candidate.id == family)?;
        Some((family, family.sizes.iter().find(|size| size.model.eq_ignore_ascii_case(model))?))
    }

    /// The installed sizes, with their family.
    pub fn installed(&self) -> Vec<(&Family, &Size)> {
        self.families.iter().flat_map(|family| family.sizes.iter().filter(|size| size.installed).map(move |size| (family, size))).collect()
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn reads_fit_words_as_strata_does() {
        assert_eq!(Fit::of("fits"), Fit::Fits);
        assert_eq!(Fit::of("does not fit: about 225 GB of its experts would be read"), Fit::No);
        assert_eq!(Fit::of("fits in the low-RAM mode (slower: the GPU holds part of the experts)"), Fit::Slower);
        assert_eq!(Fit::of("part of its experts in RAM, the rest read from the SSD"), Fit::Slower);
        assert_eq!(Fit::of("slow here: part of its experts in RAM, the rest read from the SSD (setup asks first)"), Fit::Tight);
        assert_eq!(Fit::of("tight: runs, with the experts RAM cannot hold read from the SSD"), Fit::Tight);
        assert_eq!(Fit::of("tight: it needs about 270 GB of free disk space"), Fit::Disk);
        assert_eq!(Fit::of("experimental: fits (all of its experts in RAM)"), Fit::Fits);
        assert_eq!(Fit::of(""), Fit::Unknown);
    }

    #[test]
    fn parses_the_management_answer() {
        let answer = serde_json::json!({
            "families": [
                {"family": "qwen", "title": "Qwen3.8-Flash-Next", "about": "the original model", "experimental": false, "sizes": [
                    {"model": "Q2_0", "id": "q2_0", "about": "2-bit, the fastest", "download_gb": 66.4, "memory_gb": 35.0, "on_this_pc": "fits", "installed": true, "images": true},
                    {"model": "", "id": "x"}
                ]},
                {"family": "deepseek41", "title": "DeepSeek-V4.1-Flash", "sizes": [{"model": "IST-DAS-2.00bpw", "id": "deepseek-v4.1-flash-ist-das-2.00bpw", "download_gb": 256.8, "memory_gb": 40.0, "on_this_pc": "tight: runs", "disk_gb": 266.8}]}
            ],
            "contexts": [8192, 32768, 131072],
            "recommendation": {"family": "qwen", "model": "IQ3_S", "title": "Qwen3.8-Flash-Next IQ3_S", "context": 131072, "why": "the 64 GB tier", "notes": ["n"]},
            "this_pc": {"ram_gb": 63.4, "gpus": [{"name": "RTX 5090", "vram_gb": 23.9, "usable": true}]}
        });
        let catalog = Catalog::parse(&answer);
        assert_eq!(catalog.families.len(), 2);
        assert_eq!(catalog.families[0].sizes.len(), 1);
        assert_eq!(catalog.size("qwen", "q2_0").unwrap().1.id, "q2_0");
        assert_eq!(catalog.installed().len(), 1);
        assert_eq!(catalog.families[1].sizes[0].fit, Fit::Tight);
        assert_eq!(catalog.families[1].sizes[0].disk_gb, Some(266.8));
        let recommendation = catalog.recommendation.unwrap();
        assert_eq!((recommendation.model.as_str(), recommendation.context), ("IQ3_S", 131072));
        assert_eq!(catalog.contexts, vec![8192, 32768, 131072]);
        assert_eq!(catalog.gpus[0].vram_gb, 23.9);
        let none = Catalog::parse(&serde_json::json!({"recommendation": {"family": null, "model": null, "why": "16 GB of RAM: Strata needs 32 GB or more"}}));
        assert!(none.recommendation.is_none());
        assert_eq!(none.no_recommendation.as_deref(), Some("16 GB of RAM: Strata needs 32 GB or more"));
    }
}
