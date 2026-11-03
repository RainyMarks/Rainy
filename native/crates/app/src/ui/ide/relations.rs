//! The file relations in the side pane: for the file open in the editor, the project files it imports and that import
//! it, the packages it uses, the AI's line on its part of the project and what the tour says about it (`map.relations`).
//! Clicking a file opens it, and the pane follows to that file.

use gpui_kit::component::button::Button;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde::Deserialize;
use serde_json::json;

use crate::store::Store;
use crate::store::ide::{Ide, IdeEvent};
use crate::ui::{tr, trf};

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
struct Linked {
    path: String,
    #[serde(default)]
    lines: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Area {
    path: String,
    text: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Mention {
    text: String,
    #[serde(default)]
    line: Option<u64>,
}

/// What the Host knows about one file.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Relations {
    known: bool,
    #[serde(default)]
    lines: u64,
    #[serde(default)]
    imports: Vec<Linked>,
    #[serde(default)]
    imported_by: Vec<Linked>,
    #[serde(default)]
    packages: Vec<String>,
    #[serde(default)]
    area: Option<Area>,
    #[serde(default)]
    mentions: Vec<Mention>,
}

enum State {
    /// No file is open, or it is outside the primary folder.
    Idle,
    Loading,
    Loaded(Relations),
    Failed(SharedString),
}

/// The file the pane is about: project and path under the primary folder.
type Subject = (String, String);

pub struct RelationsView {
    ide: Entity<Ide>,
    /// The file the state is being read for.
    subject: Option<Subject>,
    state: State,
    /// The file the state shows; reading the same file again keeps it on screen until the answer comes.
    shown: Option<Subject>,
    _subscriptions: Vec<Subscription>,
}

impl RelationsView {
    pub fn new(ide: Entity<Ide>, cx: &mut Context<Self>) -> Self {
        // Read again whenever a tab is chosen or a file saved: the file may import others now, and a tour written
        // meanwhile (often what sent the user to this file) says more about it. The Host reuses a recent scan.
        let subscriptions = vec![cx.subscribe(&ide, |this, _, event: &IdeEvent, cx| {
            if matches!(event, IdeEvent::ActiveChanged | IdeEvent::WorkspaceChanged | IdeEvent::Saved) {
                this.subject = None;
                cx.notify();
            }
        })];
        Self { ide, subject: None, state: State::Idle, shown: None, _subscriptions: subscriptions }
    }

    /// The active file, when it is under the project's primary folder; `Err(())` when one is open elsewhere.
    fn current(&self, cx: &App) -> Result<Option<Subject>, ()> {
        let ide = self.ide.read(cx);
        let (Some(workspace), Some(key)) = (ide.workspace.as_ref(), ide.active.as_ref()) else { return Ok(None) };
        match crate::store::ide::split_key(key) {
            (None, path) if !path.is_empty() => Ok(Some((workspace.workspace_id.clone(), path.to_owned()))),
            _ => Err(()),
        }
    }

    fn load(&mut self, subject: Subject, cx: &mut Context<Self>) {
        let call = Store::global(cx).read(cx).call("map.relations", json!({ "workspaceId": subject.0, "path": subject.1 }));
        self.subject = Some(subject.clone());
        if self.shown.as_ref() != Some(&subject) {
            self.state = State::Loading;
        }
        cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                if this.subject.as_ref() != Some(&subject) {
                    return;
                }
                this.state = match result {
                    Ok(value) => match serde_json::from_value::<Relations>(value) {
                        Ok(relations) => State::Loaded(relations),
                        Err(error) => State::Failed(error.to_string().into()),
                    },
                    Err(error) => State::Failed(crate::ui::error_text(cx, &error)),
                };
                this.shown = Some(subject.clone());
                cx.notify();
            });
        })
        .detach();
    }

    fn open(&self, path: String, line: Option<u64>, window: &mut Window, cx: &mut App) {
        self.ide.update(cx, |ide, cx| match line {
            Some(line) => ide.reveal(path, (line as u32).saturating_sub(1), 0, window, cx),
            None => ide.open(path, true, window, cx),
        });
    }

    fn file_list(&self, id: &'static str, files: &[Linked], cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if files.is_empty() {
            return div().px_3().text_sm().text_color(theme.muted_foreground).child(tr(cx, "ide.ideRelationsNone")).into_any_element();
        }
        v_flex()
            .children(files.iter().enumerate().map(|(index, file)| {
                let path = file.path.clone();
                let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
                let folder = path.rsplit_once('/').map(|(folder, _)| folder.to_owned()).unwrap_or_default();
                h_flex()
                    .id((id, index))
                    .px_3()
                    .py_0p5()
                    .gap_2()
                    .text_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.list_hover))
                    .tooltip({
                        let path = path.clone();
                        move |window, cx| gpui_kit::component::tooltip::Tooltip::new(path.clone()).build(window, cx)
                    })
                    .child(div().flex_none().text_color(theme.link).child(name))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(theme.muted_foreground).child(folder))
                    .when(file.lines > 0, |this| this.child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(file.lines.to_string())))
                    .on_click(cx.listener(move |this, _, window, cx| this.open(path.clone(), None, window, cx)))
            }))
            .into_any_element()
    }
}

impl Render for RelationsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let message = |text: SharedString| div().p_4().text_sm().text_color(theme.muted_foreground).child(text).into_any_element();
        let subject = match self.current(cx) {
            Ok(Some(subject)) => subject,
            Ok(None) => return message(tr(cx, "ide.ideRelationsEmpty")),
            Err(()) => return message(tr(cx, "ide.ideRelationsOutside")),
        };
        if self.subject.as_ref() != Some(&subject) {
            self.load(subject.clone(), cx);
        }
        let relations = match &self.state {
            State::Loaded(relations) => relations.clone(),
            State::Loading | State::Idle => return message(tr(cx, "ide.ideRelationsLoading")),
            State::Failed(error) => return message(error.clone()),
        };
        if !relations.known {
            return message(tr(cx, "ide.ideRelationsUnknown"));
        }
        let path = subject.1.clone();
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let heading = |key: &str, cx: &App| {
            div().px_3().pt_3().pb_1().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(theme.muted_foreground).child(tr(cx, key))
        };
        let ask_path = path.clone();
        let no_tour = relations.area.is_none() && relations.mentions.is_empty();
        v_flex()
            .id("relations")
            .size_full()
            .pb_3()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .px_3()
                    .pt_3()
                    .gap_0p5()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).truncate().child(name))
                    .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(format!(
                        "{} · {}",
                        path,
                        trf(cx, "ide.ideRelationsLines", &[("lines", &relations.lines)])
                    ))),
            )
            .when_some(relations.area.clone(), |this, area| {
                this.child(
                    v_flex()
                        .mx_3()
                        .mt_2()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .text_sm()
                        .child(area.text.replace('`', ""))
                        .when(area.path != path, |this| this.child(div().text_xs().text_color(theme.muted_foreground).child(format!("{}/", area.path)))),
                )
            })
            .when(!relations.mentions.is_empty(), |this| {
                let mentions = relations.mentions.clone();
                this.child(heading("ide.ideRelationsMentions", cx)).children(mentions.into_iter().enumerate().map(|(index, mention)| {
                    let (path, line) = (path.clone(), mention.line);
                    div()
                        .id(("mention", index))
                        .px_3()
                        .py_0p5()
                        .text_sm()
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.list_hover))
                        .child(match line {
                            Some(line) => format!("{} · L{line}", mention.text.replace('`', "")),
                            None => mention.text.replace('`', ""),
                        })
                        .on_click(cx.listener(move |this, _, window, cx| this.open(path.clone(), line, window, cx)))
                }))
            })
            .child(heading("ide.ideRelationsImports", cx))
            .child(self.file_list("import", &relations.imports, cx))
            .child(heading("ide.ideRelationsImportedBy", cx))
            .child(self.file_list("imported-by", &relations.imported_by, cx))
            .when(!relations.packages.is_empty(), |this| {
                this.child(heading("ide.ideRelationsPackages", cx)).child(
                    h_flex().px_3().flex_wrap().gap_1().children(relations.packages.iter().map(|name| {
                        div().px_1p5().rounded(theme.radius).border_1().border_color(theme.border).text_xs().text_color(theme.muted_foreground).child(name.clone())
                    })),
                )
            })
            .child(
                div().px_3().pt_3().child(Button::new("relations-ask").outline().xsmall().label(tr(cx, "ide.ideRelationsAsk")).on_click(move |_, window, cx| {
                    crate::ui::ask_ai(trf(cx, "ide.ideRelationsAskText", &[("path", &ask_path)]).to_string(), window, cx);
                })),
            )
            .when(no_tour, |this| this.child(div().px_3().pt_3().text_xs().text_color(theme.muted_foreground).child(tr(cx, "ide.ideRelationsNoTour"))))
            .into_any_element()
    }
}
