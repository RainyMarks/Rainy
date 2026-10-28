//! The Strata card in Settings → Tools: download Strata, keep it at its newest release, choose a model from Strata's
//! own catalog by what fits this computer, download it, serve it, connect it as the default model, and remove it all
//! again. Strata's catalog words are English; this card says them in the interface language.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rainy_carrier::strata::{Catalog, Choice, Fit, Job, JobKind, Phase, Settings, Size, Source, Status};

use super::models_extra::notice;
use crate::store::Store;
use crate::ui::{tr, trf};

/// Families shown before "more models": the original model, its coding version and the smaller model.
const MAIN_FAMILIES: [&str; 3] = ["qwen", "coder", "qwen36"];

pub struct StrataCard {
    store: Entity<Store>,
    port: Entity<InputState>,
    /// All families are listed, not only the main ones.
    all: bool,
    /// The port the field was last filled with.
    shown_port: Option<u16>,
    _subscriptions: Vec<Subscription>,
}

/// `12.3 GB`, `620 MB`.
fn bytes_text(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 10.0 {
        format!("{gb:.0} GB")
    } else if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

fn gb_text(gb: f64) -> String {
    if gb >= 10.0 { format!("{gb:.0} GB") } else { format!("{gb:.1} GB") }
}

/// A context length as people say it: `128K`.
fn context_text(tokens: u64) -> String {
    if tokens >= 1024 && tokens % 1024 == 0 { format!("{}K", tokens / 1024) } else { tokens.to_string() }
}

fn elapsed_text(seconds: u64) -> String {
    if seconds < 90 { format!("{seconds} s") } else { format!("{} min", seconds / 60) }
}

/// The family's words in Chinese, else Strata's own.
fn family_about(id: &str, english: &str, zh: bool) -> String {
    let chinese = match id {
        "qwen" => "原版模型，通用，推荐",
        "swift" => "Swift 1.5：思考更短、回答更快，质量相近",
        "coder" => "只保留一半专家，专门写代码；约 32 GB 内存即可，代码以外较弱",
        "unsloth" => "Unsloth 的 4～8 位版本：质量更高，内存够大（72～136 GB）才快，否则部分从硬盘读取",
        "gyro" => "体积最小，适合内存较少的电脑；质量略低、速度较慢",
        "nvfp4" => "NVIDIA 显卡专用的 4 位版本：下载 102～128 GB，约需 80 GB 内存",
        "qwen36" => "更小的模型，32～52 GB 内存即可，速度快",
        "deepseek41" => "超大模型：需要 12 GB 以上显卡、64 GB 内存和 240 GB 以上的固态硬盘",
        _ => "",
    };
    if zh && !chinese.is_empty() { chinese.to_owned() } else { english.to_owned() }
}

/// The size's words in Chinese, else Strata's own.
fn size_about(family: &str, size: &Size, zh: bool) -> String {
    let chinese = match (family, size.model.as_str()) {
        ("qwen" | "swift", "Q2_0") => "2 位，最快，质量好",
        ("qwen" | "swift", "IQ2_XS") => "2 位，质量更好，速度接近",
        ("qwen" | "swift", "IQ3_XXS") => "3 位，质量很好，较慢",
        ("qwen" | "swift", "IQ3_S") => "3.5 位，质量最好（与完整模型相当），最慢；适合 64 GB 内存",
        ("coder", "IQ1_M") => "Coder 唯一的尺寸：一半专家，按 3.5 位存储",
        ("unsloth", "UD-IQ4_XS") => "约 4 位，质量介于 IQ3_S 和 UD-Q4_K_XL 之间",
        ("unsloth", "UD-Q4_K_XL") => "4 位，质量更高",
        ("unsloth", "UD-Q5_K_XL") => "5 位，接近 8 位的质量",
        ("unsloth", "UD-Q6_K_XL") => "6 位",
        ("unsloth", "Q8_0") => "8 位，参考质量；适合 192 GB 内存",
        ("gyro", "Gyro-S") => "最小：显存加内存共 27.6 GB；质量低于 2～3 位版本",
        ("gyro", "Gyro-M") => "约等于 IQ3_XXS 的质量，显存少用约 20%，比 Q2_0 慢 16～21%",
        ("nvfp4", "NVFP4") => "NVFP4 里质量最好（Strata 自己转换）",
        ("nvfp4", "NVFP4-NVIDIA") => "NVIDIA 官方的 NVFP4，大小和速度同上",
        ("nvfp4", "NVFP4-QAD") => "NVIDIA 为 4 位重新训练的版本，短提示更快；下载后在本机转换，需额外约 110 GB",
        ("qwen36", "UD-Q4_K_XL") => "4 位，最小",
        ("qwen36", "UD-Q6_K_XL") => "6 位，更接近完整模型",
        ("qwen36", "Q8_0") => "8 位，最接近完整模型",
        ("deepseek41", "IST-DAS-1.75bpw") => "官方最小的版本，占硬盘 239 GB",
        ("deepseek41", "IST-DAS-2.00bpw") => "官方较小的版本，占硬盘 257 GB，每秒约 5～7 个词",
        ("deepseek41", "IST-DAS-2.25bpw") => "官方较好的版本，占硬盘 274 GB，适合写代码和智能体",
        ("deepseek41", "UD-Q8_K_XL") => "Unsloth 8 位，508 GB，适合有几百 GB 内存的多显卡机器",
        _ => "",
    };
    if zh && !chinese.is_empty() { chinese.to_owned() } else { size.about.clone() }
}

fn fit_key(fit: Fit) -> &'static str {
    match fit {
        Fit::Fits => "settings.strataFits",
        Fit::Slower => "settings.strataFitsSlower",
        Fit::Tight => "settings.strataFitTight",
        Fit::Disk => "settings.strataFitDisk",
        Fit::No => "settings.strataFitNo",
        Fit::Unknown => "settings.strataFitUnknown",
    }
}

/// The step's words: Strata's own sentence for a step this card does not know.
fn step_text(job: &Job, cx: &App) -> SharedString {
    let key = match job.step.as_str() {
        "terminal" => "settings.strataStepTerminal",
        "verify" => "settings.strataStepVerify",
        "feed" => "settings.strataStepFeed",
        "engine" => "settings.strataStepEngine",
        "engine-verify" => "settings.strataStepEngineVerify",
        "engine-test" => "settings.strataStepEngineTest",
        "engine-switch" => "settings.strataStepEngineSwitch",
        "control" => "settings.strataStepControl",
        "stop" => "settings.strataStepStop",
        "remove" => "settings.strataStepRemove",
        "model:check" => "settings.strataStepCheck",
        "model:helpers" => "settings.strataStepHelpers",
        "model:engine" => "settings.strataStepModelEngine",
        "model:download" => "settings.strataStepDownload",
        "model:verify" => "settings.strataStepModelVerify",
        "model:repair" => "settings.strataStepRepair",
        "model:pictures" => "settings.strataStepPictures",
        "model:draft_download" => "settings.strataStepDraftDownload",
        "model:draft" => "settings.strataStepDraft",
        "model:pack" => "settings.strataStepPack",
        "model:tokenizer" => "settings.strataStepTokenizer",
        "model:settings" => "settings.strataStepSettings",
        _ if job.step.starts_with("model:") => "settings.strataStepWorking",
        _ => return job.detail.clone().unwrap_or_default().into(),
    };
    tr(cx, key)
}

impl StrataCard {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let port = cx.new(|cx| InputState::new(window, cx).placeholder("8080"));
        let subscriptions = vec![cx.observe(&store, |_, _, cx| cx.notify())];
        store.update(cx, |store, cx| store.refresh_strata(cx));
        Self { store, port, all: false, shown_port: None, _subscriptions: subscriptions }
    }

    /// Show how a request ended: a refusal as an error notification.
    fn finish(task: Task<Result<(), String>>, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |_, cx| {
            if let Err(message) = task.await
                && !message.is_empty()
            {
                let _ = cx.update(|window, cx| window.push_notification(Notification::error(message), cx));
            }
        })
        .detach();
    }

    fn run(&mut self, window: &mut Window, cx: &mut Context<Self>, request: impl FnOnce(&mut Store, &mut Context<Store>) -> Task<Result<(), String>>) {
        let task = self.store.update(cx, request);
        Self::finish(task, window, cx);
    }

    fn save(&mut self, change: impl FnOnce(&mut Settings), window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut settings) = self.store.read(cx).strata.status.as_ref().map(|status| status.settings.clone()) else { return };
        change(&mut settings);
        self.run(window, cx, move |store, cx| store.save_strata(settings, cx));
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.store.update(cx, |store, cx| store.connect_strata(cx));
        cx.spawn_in(window, async move |_, cx| {
            let result = task.await;
            let _ = cx.update(|window, cx| match result {
                Ok(_) => window.push_notification(Notification::success(tr(cx, "settings.strataConnected").to_string()), cx),
                Err(message) if !message.is_empty() => window.push_notification(Notification::error(message), cx),
                Err(_) => {}
            });
        })
        .detach();
    }

    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let folder = rainy_carrier::strata::Manager::data_folder_for(&path);
            let _ = this.update_in(cx, |this, window, cx| this.save(move |settings| settings.data_dir = Some(folder.to_string_lossy().into_owned()), window, cx));
        })
        .detach();
    }

    fn confirm_uninstall(&mut self, status: &Status, window: &mut Window, cx: &mut Context<Self>) {
        let models: Vec<(String, f64)> = status
            .catalog
            .as_ref()
            .map(|catalog| catalog.installed().into_iter().map(|(family, size)| (format!("{} {}", family.title, size.model), size.download_gb)).collect())
            .unwrap_or_default();
        let total: f64 = models.iter().map(|(_, gb)| gb).sum();
        let store = self.store.clone();
        let remove_models = std::rc::Rc::new(std::cell::Cell::new(false));
        window.open_dialog(cx, move |dialog, _, cx| {
            let (store, flag, toggle) = (store.clone(), remove_models.clone(), remove_models.clone());
            let checked = flag.get();
            dialog
                .title(tr(cx, "settings.strataUninstallTitle"))
                .w(px(460.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().child(tr(cx, "settings.strataUninstallBody")))
                        .when(!models.is_empty(), |this| {
                            let names = models.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>().join("、");
                            this.child(Checkbox::new("strata-uninstall-models").checked(checked).label(trf(cx, "settings.strataUninstallModels", &[("models", &names), ("size", &gb_text(total))])).on_click(
                                move |checked: &bool, window, cx| {
                                    toggle.set(*checked);
                                    window.refresh();
                                    let _ = cx;
                                },
                            ))
                        }),
                )
                .footer(
                    gpui_kit::component::dialog::DialogFooter::new()
                        .child(Button::new("strata-uninstall-cancel").outline().label(tr(cx, "chat.cancel")).on_click(|_, window, cx| window.close_dialog(cx)))
                        .child(Button::new("strata-uninstall-ok").danger().label(tr(cx, "settings.strataUninstall")).on_click(move |_, window, cx| {
                            let models = flag.get();
                            let task = store.update(cx, |store, cx| store.uninstall_strata(models, cx));
                            window.close_dialog(cx);
                            window
                                .spawn(cx, async move |cx| {
                                    if let Err(message) = task.await
                                        && !message.is_empty()
                                    {
                                        let _ = cx.update(|window, cx| window.push_notification(Notification::error(message), cx));
                                    }
                                })
                                .detach();
                        })),
                )
        });
    }

    /// Ask before a download of tens of gigabytes, saying how big it is and where it goes.
    #[allow(clippy::too_many_arguments)]
    fn confirm_download(&mut self, choice: Choice, title: String, gb: f64, folder: String, free: Option<f64>, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let short = free.is_some_and(|free| free < gb + 8.0);
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (store, choice) = (store.clone(), choice.clone());
            let free_text = free.map(|free| trf(cx, "settings.strataFree", &[("size", &gb_text(free))]).to_string()).unwrap_or_default();
            let mut description = trf(cx, "settings.strataDownloadConfirm", &[("model", &title), ("size", &gb_text(gb)), ("folder", &folder), ("free", &free_text)]).to_string();
            if short {
                description.push_str(&format!("\n\n{}", tr(cx, "settings.strataDownloadShort")));
            }
            alert
                .title(tr(cx, "settings.strataDownloadTitle"))
                .description(description)
                .ok_text(trf(cx, "settings.strataDownload", &[("size", &gb_text(gb))]))
                .cancel_text(tr(cx, "chat.cancel"))
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let choice = choice.clone();
                    let task = store.update(cx, |store, cx| store.download_strata_model(choice, cx));
                    window
                        .spawn(cx, async move |cx| {
                            if let Err(message) = task.await
                                && !message.is_empty()
                            {
                                let _ = cx.update(|window, cx| window.push_notification(Notification::error(message), cx));
                            }
                        })
                        .detach();
                    true
                })
        });
    }

    fn muted(text: impl Into<SharedString>, cx: &App) -> AnyElement {
        div().text_xs().text_color(cx.theme().muted_foreground).child(text.into()).into_any_element()
    }

    /// The running job with its progress, and how to stop it.
    fn render_job(&self, job: &Job, catalog: Option<&Catalog>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mut line = step_text(job, cx).to_string();
        if job.total > 0 {
            let percent = (job.done as f64 / job.total as f64 * 100.0).min(100.0);
            line.push_str(&format!("  {} / {}（{percent:.0}%）", bytes_text(job.done), bytes_text(job.total)));
        }
        let title = match &job.kind {
            JobKind::Install => tr(cx, "settings.strataInstalling"),
            JobKind::Update => tr(cx, "settings.strataUpdating"),
            JobKind::Uninstall => tr(cx, "settings.strataUninstalling"),
            JobKind::Download { family, model } => {
                let title = catalog.and_then(|catalog| catalog.size(family, model)).map(|(family, size)| format!("{} {}", family.title, size.model)).unwrap_or_else(|| format!("{family} {model}"));
                trf(cx, "settings.strataDownloading", &[("model", &title)])
            }
            JobKind::Remove { id } => {
                let title = catalog
                    .and_then(|catalog| catalog.families.iter().find_map(|family| family.sizes.iter().find(|size| &size.id == id).map(|size| format!("{} {}", family.title, size.model))))
                    .unwrap_or_else(|| id.clone());
                trf(cx, "settings.strataRemoving", &[("model", &title)])
            }
        };
        let percent = (job.total > 0).then(|| (job.done as f32 / job.total as f32 * 100.0).min(100.0));
        v_flex()
            .gap_2()
            .p_3()
            .rounded(theme.radius)
            .bg(theme.muted)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Spinner::new().small())
                    .child(div().flex_1().min_w_0().text_sm().font_semibold().child(title))
                    .when(job.cancellable, |this| {
                        this.child(Button::new("strata-cancel").outline().xsmall().label(tr(cx, "settings.strataCancel")).on_click(cx.listener(|this, _, window, cx| this.run(window, cx, |store, cx| store.cancel_strata(cx)))))
                    }),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(line))
            .when_some(percent, |this, percent| this.child(Progress::new("strata-progress").xsmall().value(percent)))
            .when(matches!(job.kind, JobKind::Download { .. }), |this| this.child(Self::muted(tr(cx, "settings.strataDownloadResumes"), cx)))
            .into_any_element()
    }

    fn render_not_installed(&self, status: &Status, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut body = vec![Self::muted(tr(cx, "settings.strataNotInstalled"), cx)];
        body.push(
            h_flex()
                .child(
                    Button::new("strata-install")
                        .primary()
                        .small()
                        .label(tr(cx, "settings.strataInstall"))
                        .disabled(status.job.is_some())
                        .on_click(cx.listener(|this, _, window, cx| this.run(window, cx, |store, cx| store.install_strata(cx)))),
                )
                .into_any_element(),
        );
        body
    }

    fn render_version(&self, status: &Status, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let Some(installed) = &status.installed else { return Vec::new() };
        let mut text = match &installed.strata {
            Some(version) => format!("Strata {version}"),
            None => format!("Strata · {}", installed.control),
        };
        if let Some(engine) = &installed.engine {
            let backend = match engine.backend.as_str() {
                "cuda" => format!("NVIDIA {}", engine.variant.replace("cuda", "CUDA ")),
                "hip" => format!("AMD {}", engine.variant),
                other => other.to_owned(),
            };
            text.push_str(&format!(" · {} {} · {backend}", tr(cx, "settings.strataEngine"), engine.version));
        }
        let busy = status.job.is_some();
        let snapshot = status.clone();
        let mut body = vec![
            h_flex()
                .gap_2()
                .items_center()
                .child(div().flex_1().min_w_0().text_sm().child(text))
                .child(
                    Button::new("strata-check")
                        .outline()
                        .small()
                        .label(tr(cx, "settings.strataCheckUpdate"))
                        .loading(status.checking)
                        .disabled(busy || status.checking)
                        .on_click(cx.listener(|this, _, window, cx| this.run(window, cx, |store, cx| store.check_strata(cx)))),
                )
                .child(Button::new("strata-uninstall").ghost().small().label(tr(cx, "settings.strataUninstall")).disabled(busy).on_click(cx.listener(move |this, _, window, cx| this.confirm_uninstall(&snapshot, window, cx))))
                .into_any_element(),
        ];
        if let Some(update) = status.update.as_ref().filter(|update| update.available()) {
            let what = [update.control.as_ref().map(|version| format!("{} {version}", tr(cx, "settings.strataTerminal"))), update.engine.as_ref().map(|version| format!("{} {version}", tr(cx, "settings.strataEngine")))]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("、");
            let serving = matches!(status.server.phase, Phase::Starting | Phase::Running | Phase::Stopping);
            body.push(
                h_flex()
                    .gap_2()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .child(div().flex_1().min_w_0().text_sm().child(trf(cx, if serving { "settings.strataUpdateWaiting" } else { "settings.strataUpdateAvailable" }, &[("what", &what)])))
                    .child(Button::new("strata-update").primary().small().label(tr(cx, "settings.strataUpdateNow")).disabled(busy || serving).on_click(cx.listener(|this, _, window, cx| this.run(window, cx, |store, cx| store.update_strata(cx)))))
                    .into_any_element(),
            );
        } else if let Some(update) = &status.update {
            if let Some(reason) = update.no_engine.as_ref().filter(|reason| !reason.is_empty()) {
                body.push(notice(cx, trf(cx, "settings.strataNoEngine", &[("reason", reason)]), true));
            } else {
                body.push(Self::muted(tr(cx, "settings.strataUpToDate"), cx));
            }
        }
        let automatic = status.settings.auto_update;
        body.push(
            h_flex()
                .gap_2()
                .items_center()
                .child(Switch::new("strata-auto-update").checked(automatic).small().on_click(cx.listener(|this, checked: &bool, window, cx| {
                    let checked = *checked;
                    this.save(move |settings| settings.auto_update = checked, window, cx);
                })))
                .child(Self::muted(tr(cx, "settings.strataAutoUpdate"), cx))
                .into_any_element(),
        );
        body
    }

    /// The computer as Strata sees it, and the size its setup would pick.
    fn render_recommendation(&self, status: &Status, catalog: &Catalog, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let zh = crate::ui::locale(cx) == crate::i18n::Locale::Zh;
        let mut body = Vec::new();
        let card = catalog.gpus.iter().filter(|gpu| gpu.usable).max_by(|a, b| a.vram_gb.total_cmp(&b.vram_gb));
        let pc = match (card, catalog.ram_gb) {
            (Some(card), Some(ram)) => trf(cx, "settings.strataThisPc", &[("gpu", &card.name), ("vram", &gb_text(card.vram_gb)), ("ram", &gb_text(ram))]),
            (None, Some(ram)) => trf(cx, "settings.strataThisPcNoGpu", &[("ram", &gb_text(ram))]),
            _ => SharedString::default(),
        };
        if !pc.is_empty() {
            body.push(Self::muted(pc, cx));
        }
        match &catalog.recommendation {
            Some(recommendation) => {
                let Some((family, size)) = catalog.size(&recommendation.family, &recommendation.model) else { return body };
                let choice = Choice { family: family.id.clone(), model: size.model.clone() };
                body.push(
                    v_flex()
                        .gap_2()
                        .p_3()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.primary.opacity(0.5))
                        .bg(theme.primary.opacity(0.06))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(Icon::new(IconName::Star).small().text_color(theme.primary))
                                .child(div().text_sm().font_semibold().child(trf(cx, "settings.strataRecommended", &[("model", &format!("{} {}", family.title, size.model))]))),
                        )
                        .child(Self::muted(format!("{} · {}", size_about(&family.id, size, zh), trf(cx, "settings.strataSizeLine", &[("download", &gb_text(size.download_gb)), ("memory", &gb_text(size.memory_gb)), ("context", &context_text(u64::from(recommendation.context)))])), cx))
                        .child(Self::muted(tr(cx, "settings.strataRecommendedWhy"), cx))
                        .child(h_flex().gap_2().child(self.size_actions(status, &choice, size, "recommended", cx)))
                        .into_any_element(),
                );
            }
            None => {
                if let Some(why) = &catalog.no_recommendation {
                    body.push(notice(cx, trf(cx, "settings.strataNothingFits", &[("reason", why)]), true));
                }
            }
        }
        body
    }

    /// Download, or start and remove, for one size.
    fn size_actions(&self, status: &Status, choice: &Choice, size: &Size, place: &str, cx: &mut Context<Self>) -> AnyElement {
        let busy = status.job.is_some();
        let serving = status.server.choice.as_ref() == Some(choice) && matches!(status.server.phase, Phase::Starting | Phase::Running);
        let any_serving = matches!(status.server.phase, Phase::Starting | Phase::Running | Phase::Stopping);
        let id = format!("{place}-{}-{}", choice.family, choice.model);
        if !size.installed {
            let choice = choice.clone();
            let title = status.catalog.as_ref().and_then(|catalog| catalog.size(&choice.family, &choice.model)).map(|(family, size)| format!("{} {}", family.title, size.model)).unwrap_or_else(|| format!("{} {}", choice.family, choice.model));
            let (gb, folder, free) = (size.download_gb, status.data_dir.clone(), status.data_free_gb);
            return Button::new(SharedString::from(format!("strata-download-{id}")))
                .primary()
                .xsmall()
                .label(trf(cx, "settings.strataDownload", &[("size", &gb_text(size.download_gb))]))
                .disabled(busy)
                .on_click(cx.listener(move |this, _, window, cx| this.confirm_download(choice.clone(), title.clone(), gb, folder.clone(), free, window, cx)))
                .into_any_element();
        }
        let (start, remove) = (choice.clone(), choice.clone());
        h_flex()
            .gap_1()
            .child(if serving {
                Button::new(SharedString::from(format!("strata-stop-{id}"))).outline().xsmall().label(tr(cx, "settings.strataStop")).disabled(status.server.phase == Phase::Stopping).on_click(cx.listener(|this, _, window, cx| this.run(window, cx, |store, cx| store.stop_strata(cx))))
            } else {
                Button::new(SharedString::from(format!("strata-start-{id}")))
                    .primary()
                    .xsmall()
                    .label(tr(cx, "settings.strataStart"))
                    .disabled(any_serving || matches!(status.job.as_ref().map(|job| &job.kind), Some(JobKind::Update | JobKind::Uninstall | JobKind::Install)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let choice = start.clone();
                        this.run(window, cx, move |store, cx| store.start_strata(choice, cx));
                    }))
            })
            .child(Button::new(SharedString::from(format!("strata-remove-{id}"))).ghost().xsmall().label(tr(cx, "settings.strataRemoveModel")).disabled(busy || serving).on_click(cx.listener(move |this, _, window, cx| {
                let choice = remove.clone();
                this.run(window, cx, move |store, cx| store.remove_strata_model(choice, cx));
            })))
            .into_any_element()
    }

    /// Every family and size of the catalog, with what fits.
    fn render_catalog(&self, status: &Status, catalog: &Catalog, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let zh = crate::ui::locale(cx) == crate::i18n::Locale::Zh;
        let mut body = vec![div().mt_2().text_sm().font_semibold().child(tr(cx, "settings.strataAllModels")).into_any_element()];
        for family in catalog.families.iter().filter(|family| self.all || MAIN_FAMILIES.contains(&family.id.as_str()) || family.sizes.iter().any(|size| size.installed)) {
            body.push(
                v_flex()
                    .gap_0p5()
                    .mt_1()
                    .child(div().text_sm().font_semibold().child(family.title.clone()))
                    .child(Self::muted(family_about(&family.id, &family.about, zh), cx))
                    .into_any_element(),
            );
            for size in &family.sizes {
                let choice = Choice { family: family.id.clone(), model: size.model.clone() };
                let fit_color = match size.fit {
                    Fit::Fits => theme.success,
                    Fit::Slower => theme.foreground,
                    Fit::Tight | Fit::Disk => theme.warning,
                    Fit::No => theme.danger,
                    Fit::Unknown => theme.muted_foreground,
                };
                let recommended = catalog.recommendation.as_ref().is_some_and(|recommendation| recommendation.family == family.id && recommendation.model == size.model);
                body.push(
                    h_flex()
                        .id(SharedString::from(format!("strata-size-{}-{}", family.id, size.model)))
                        .gap_3()
                        .px_2()
                        .py_1p5()
                        .rounded(theme.radius)
                        .items_center()
                        .hover(|style| style.bg(theme.list_hover))
                        .tooltip({
                            let words = size.fit_words.clone();
                            move |window, cx| gpui_kit::component::tooltip::Tooltip::new(words.clone()).build(window, cx)
                        })
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .items_center()
                                        .child(div().text_sm().font_family(theme.mono_font_family.clone()).child(size.model.clone()))
                                        .when(recommended, |this| this.child(Icon::new(IconName::Star).xsmall().text_color(theme.primary)))
                                        .when(size.installed, |this| this.child(div().text_xs().px_1().rounded(theme.radius).bg(theme.success.opacity(0.15)).text_color(theme.success).child(tr(cx, "settings.strataInstalled")))),
                                )
                                .child(div().text_xs().text_color(theme.muted_foreground).overflow_hidden().text_ellipsis().child(size_about(&family.id, size, zh))),
                        )
                        .child(div().w(px(150.)).flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(trf(cx, "settings.strataSizeShort", &[("download", &gb_text(size.download_gb)), ("memory", &gb_text(size.memory_gb))])))
                        .child(div().w(px(110.)).flex_shrink_0().text_xs().text_color(fit_color).child(tr(cx, fit_key(size.fit))))
                        .child(div().flex_shrink_0().child(self.size_actions(status, &choice, size, "list", cx)))
                        .into_any_element(),
                );
            }
        }
        let hidden = catalog.families.iter().filter(|family| !MAIN_FAMILIES.contains(&family.id.as_str()) && !family.sizes.iter().any(|size| size.installed)).count();
        if hidden > 0 || self.all {
            body.push(
                h_flex()
                    .child(
                        Button::new("strata-more")
                            .ghost()
                            .xsmall()
                            .label(if self.all { tr(cx, "settings.strataFewerModels") } else { trf(cx, "settings.strataMoreModels", &[("count", &hidden)]) })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.all = !this.all;
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            );
        }
        body
    }

    /// Where downloads go and where they come from.
    fn render_download_settings(&self, status: &Status, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let busy = status.job.is_some();
        let free = status.data_free_gb.map(|free| trf(cx, "settings.strataFree", &[("size", &gb_text(free))])).unwrap_or_default();
        let source = status.settings.source;
        vec![
            div().mt_2().text_sm().font_semibold().child(tr(cx, "settings.strataDownloadSettings")).into_any_element(),
            h_flex()
                .gap_2()
                .items_center()
                .child(div().w(px(84.)).flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.strataDataDir")))
                .child(div().flex_1().min_w_0().text_xs().font_family(theme.mono_font_family.clone()).whitespace_nowrap().overflow_hidden().text_ellipsis().child(status.data_dir.clone()))
                .child(div().flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(free))
                .child(Button::new("strata-data-dir").outline().xsmall().label(tr(cx, "settings.strataChange")).disabled(busy).on_click(cx.listener(|this, _, window, cx| this.choose_folder(window, cx))))
                .into_any_element(),
            h_flex()
                .gap_2()
                .items_center()
                .child(div().w(px(84.)).flex_shrink_0().text_xs().text_color(theme.muted_foreground).child(tr(cx, "settings.strataSource")))
                .child(super::segmented(
                    "strata-source",
                    vec![("huggingface", tr(cx, "settings.strataSourceHf")), ("modelscope", tr(cx, "settings.strataSourceMs"))],
                    if source == Source::Modelscope { "modelscope" } else { "huggingface" },
                    {
                        let entity = cx.entity().downgrade();
                        move |value, window, cx| {
                            let source = if value == "modelscope" { Source::Modelscope } else { Source::Huggingface };
                            let _ = entity.update(cx, |this, cx| this.save(move |settings| settings.source = source, window, cx));
                        }
                    },
                ))
                .into_any_element(),
            Self::muted(tr(cx, "settings.strataSourceHint"), cx),
        ]
    }

    /// The server: what runs, the context and port, and connecting it.
    fn render_server(&self, status: &Status, catalog: Option<&Catalog>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let server = &status.server;
        let title = |choice: &Choice| catalog.and_then(|catalog| catalog.size(&choice.family, &choice.model)).map(|(family, size)| format!("{} {}", family.title, size.model)).unwrap_or_else(|| format!("{} {}", choice.family, choice.model));
        let line: SharedString = match server.phase {
            Phase::Stopped => tr(cx, "settings.strataNotRunning"),
            Phase::Starting => {
                let model = server.choice.as_ref().map(title).unwrap_or_default();
                let key = if server.health.is_some() { "settings.strataLoadingModel" } else { "settings.strataStartingModel" };
                trf(cx, key, &[("model", &model), ("elapsed", &elapsed_text(server.elapsed))])
            }
            Phase::Running => {
                let health = server.health.clone();
                match health {
                    Some(health) => trf(cx, "settings.strataRunningModel", &[("model", &health.model), ("context", &context_text(health.context_window)), ("url", &health.base_url)]),
                    None => tr(cx, "settings.strataRunningState"),
                }
            }
            Phase::Stopping => tr(cx, "settings.strataStopping"),
            Phase::External => {
                let model = server.health.as_ref().map(|health| health.model.clone()).unwrap_or_default();
                trf(cx, "settings.strataExternal", &[("port", &status.settings.port), ("model", &model)])
            }
        };
        let connectable = matches!(server.phase, Phase::Running | Phase::External) && server.health.as_ref().is_some_and(|health| health.loaded && !health.authentication_required);
        let connecting = self.store.read(cx).strata.connecting;
        let mut body = vec![
            div().mt_2().text_sm().font_semibold().child(tr(cx, "settings.strataServer")).into_any_element(),
            h_flex()
                .gap_2()
                .items_center()
                .when(matches!(server.phase, Phase::Starting | Phase::Stopping), |this| this.child(Spinner::new().small()))
                .when(server.phase == Phase::Running, |this| this.child(div().size(px(8.)).rounded_full().bg(theme.success)))
                .child(div().flex_1().min_w_0().text_sm().child(line))
                .when(server.phase == Phase::Starting, |this| {
                    this.child(Button::new("strata-cancel-start").outline().xsmall().label(tr(cx, "settings.strataCancel")).on_click(cx.listener(|this, _, window, cx| this.run(window, cx, |store, cx| store.stop_strata(cx)))))
                })
                .when(server.phase == Phase::Running, |this| {
                    this.child(Button::new("strata-stop").outline().xsmall().label(tr(cx, "settings.strataStop")).on_click(cx.listener(|this, _, window, cx| this.run(window, cx, |store, cx| store.stop_strata(cx)))))
                })
                .when(connectable, |this| {
                    this.child(Button::new("strata-connect").primary().xsmall().label(tr(cx, "settings.strataConnect")).loading(connecting).disabled(connecting).on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))))
                })
                .into_any_element(),
        ];
        if server.phase == Phase::Starting {
            body.push(Self::muted(tr(cx, "settings.strataStartingHint"), cx));
        }
        if server.phase == Phase::External {
            body.push(Self::muted(tr(cx, "settings.strataExternalNote"), cx));
        }
        // Context and port apply to the next start.
        let contexts: Vec<u32> = catalog.map(|catalog| catalog.contexts.clone()).unwrap_or_default();
        let current = status.settings.context;
        let recommended = catalog.and_then(|catalog| catalog.recommendation.as_ref()).map(|recommendation| recommendation.context);
        let label: SharedString = match current {
            Some(context) => context_text(u64::from(context)).into(),
            None => match recommended {
                Some(context) => trf(cx, "settings.strataContextAuto", &[("context", &context_text(u64::from(context)))]),
                None => tr(cx, "settings.strataContextAutoPlain"),
            },
        };
        let automatic = tr(cx, "settings.strataContextAutoPlain");
        let entity = cx.entity().downgrade();
        let locked = matches!(server.phase, Phase::Starting | Phase::Running | Phase::Stopping);
        let picker = Button::new("strata-context").outline().xsmall().label(label).disabled(locked || contexts.is_empty()).dropdown_menu(move |mut menu, _, _| {
            let pick = |value: Option<u32>, entity: WeakEntity<StrataCard>| {
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = entity.update(cx, |this, cx| this.save(move |settings| settings.context = value, window, cx));
                }
            };
            menu = menu.item(PopupMenuItem::new(automatic.clone()).checked(current.is_none()).on_click(pick(None, entity.clone())));
            for context in &contexts {
                menu = menu.item(PopupMenuItem::new(context_text(u64::from(*context))).checked(current == Some(*context)).on_click(pick(Some(*context), entity.clone())));
            }
            menu
        });
        let port_text = self.port.read(cx).value().trim().to_owned();
        let port_changed = port_text.parse::<u16>().ok().filter(|port| *port >= 1024).is_some_and(|port| port != status.settings.port);
        body.push(
            h_flex()
                .gap_4()
                .items_center()
                .child(h_flex().gap_2().items_center().child(Self::muted(tr(cx, "settings.strataContext"), cx)).child(picker))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(Self::muted(tr(cx, "settings.strataPort"), cx))
                        .child(div().w(px(80.)).child(Input::new(&self.port).xsmall().disabled(locked)))
                        .when(port_changed, |this| {
                            this.child(Button::new("strata-port-save").outline().xsmall().label(tr(cx, "settings.strataSave")).on_click(cx.listener(move |this, _, window, cx| {
                                if let Ok(port) = this.port.read(cx).value().trim().parse::<u16>() {
                                    this.save(move |settings| settings.port = port, window, cx);
                                }
                            })))
                        }),
                )
                .into_any_element(),
        );
        body
    }
}

impl Render for StrataCard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (status, available) = {
            let store = self.store.read(cx);
            (store.strata.status.clone(), store.strata.manager.is_some())
        };
        let theme = cx.theme().clone();
        let mut body: Vec<AnyElement> = vec![
            h_flex()
                .gap_2()
                .items_center()
                .child(div().text_sm().font_semibold().child(tr(cx, "settings.strataTitle")))
                .child(div().flex_1())
                .when_some(status.as_ref().and_then(|status| status.installed.as_ref()).and_then(|installed| installed.strata.clone()), |this, version| {
                    this.child(div().text_xs().text_color(theme.muted_foreground).child(version))
                })
                .into_any_element(),
            Self::muted(tr(cx, "settings.strataLead"), cx),
        ];
        let Some(status) = status.filter(|_| available) else {
            body.push(h_flex().child(Spinner::new().small()).into_any_element());
            return v_flex().gap_3().p_4().rounded(theme.radius_lg).border_1().border_color(theme.border).bg(theme.group_box).children(body);
        };
        if self.shown_port != Some(status.settings.port) {
            let port = status.settings.port.to_string();
            self.port.update(cx, |state, cx| state.set_value(port, window, cx));
            self.shown_port = Some(status.settings.port);
        }
        if let Some(job) = &status.job {
            body.push(self.render_job(job, status.catalog.as_ref(), cx));
        }
        if let Some(error) = &status.error {
            body.push(notice(cx, error.clone().into(), true));
        }
        if let Some(text) = &status.notice {
            body.push(notice(cx, text.clone().into(), false));
        }
        if status.installed.is_none() {
            if !matches!(status.job.as_ref().map(|job| &job.kind), Some(JobKind::Install | JobKind::Uninstall)) {
                body.extend(self.render_not_installed(&status, cx));
            }
        } else {
            body.extend(self.render_version(&status, cx));
            match &status.catalog {
                Some(catalog) => {
                    body.extend(self.render_recommendation(&status, catalog, cx));
                    body.extend(self.render_server(&status, Some(catalog), cx));
                    body.extend(self.render_catalog(&status, catalog, cx));
                }
                None if status.catalog_loading => body.push(
                    h_flex().gap_2().items_center().child(Spinner::new().small()).child(Self::muted(tr(cx, "settings.strataReadingCatalog"), cx)).into_any_element(),
                ),
                None => {
                    if let Some(error) = &status.catalog_error {
                        body.push(notice(cx, error.clone().into(), true));
                    }
                    body.push(
                        h_flex()
                            .child(Button::new("strata-catalog").outline().small().label(tr(cx, "settings.strataShowModels")).on_click(cx.listener(|this, _, window, cx| this.run(window, cx, |store, cx| store.load_strata_catalog(cx)))))
                            .into_any_element(),
                    );
                    body.extend(self.render_server(&status, None, cx));
                }
            }
            body.extend(self.render_download_settings(&status, cx));
        }
        if status.legacy_bytes > 0 {
            body.push(
                h_flex()
                    .gap_2()
                    .items_center()
                    .mt_1()
                    .child(div().flex_1().min_w_0().child(Self::muted(trf(cx, "settings.strataLegacy", &[("size", &bytes_text(status.legacy_bytes))]), cx)))
                    .child(Button::new("strata-legacy").ghost().xsmall().label(tr(cx, "settings.strataDeleteLegacy")).on_click(cx.listener(|this, _, window, cx| this.run(window, cx, |store, cx| store.remove_strata_legacy(cx)))))
                    .into_any_element(),
            );
        }
        v_flex().gap_3().p_4().rounded(theme.radius_lg).border_1().border_color(theme.border).bg(theme.group_box).children(body)
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn sizes_and_contexts_read_as_people_say_them() {
        assert_eq!(bytes_text(620_430_878), "620 MB");
        assert_eq!(bytes_text(12_300_000_000), "12 GB");
        assert_eq!(bytes_text(1_500_000_000), "1.5 GB");
        assert_eq!(context_text(131_072), "128K");
        assert_eq!(context_text(8192), "8K");
        assert_eq!(context_text(1000), "1000");
        assert_eq!(elapsed_text(45), "45 s");
        assert_eq!(elapsed_text(600), "10 min");
    }

    #[test]
    fn every_size_of_the_catalog_has_chinese_words() {
        let size = |model: &str| Size { model: model.into(), id: String::new(), about: "english".into(), download_gb: 1.0, memory_gb: 1.0, fit: Fit::Fits, fit_words: String::new(), installed: false, experimental: false, images: true, disk_gb: None };
        for (family, model) in [("qwen", "Q2_0"), ("swift", "IQ3_S"), ("coder", "IQ1_M"), ("unsloth", "Q8_0"), ("gyro", "Gyro-M"), ("nvfp4", "NVFP4-QAD"), ("qwen36", "UD-Q4_K_XL"), ("deepseek41", "IST-DAS-2.25bpw")] {
            assert_ne!(size_about(family, &size(model), true), "english", "{family} {model}");
            assert_ne!(family_about(family, "english", true), "english", "{family}");
        }
        // A size a later Strata adds keeps Strata's words, and English keeps them too.
        assert_eq!(size_about("qwen", &size("Q9_9"), true), "english");
        assert_eq!(size_about("qwen", &size("Q2_0"), false), "english");
    }
}
