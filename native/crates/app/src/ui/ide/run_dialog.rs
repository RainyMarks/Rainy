//! The run configurations (`RunConfigurationDialog.tsx`): language, program, arguments, environment, interpreter and
//! the C/C++ build of a named profile.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::{Map, Value, json};

use crate::store::ide::{Ide, RUN_LANGUAGES, run_language, split_key};
use crate::ui::{tr, trf};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Program,
    PythonModule,
    Cwd,
    Arguments,
    Environment,
    Executable,
    Flags,
    BuildDirectory,
    Target,
    BuildExecutable,
    ConfigurePreset,
    BuildPreset,
}

const FIELDS: [(Field, &str); 13] = [
    (Field::Name, "ide.ideRunProfile"),
    (Field::Program, "ide.ideProgram"),
    (Field::PythonModule, "ide.idePythonModule"),
    (Field::Cwd, "ide.ideCwd"),
    (Field::Arguments, "ide.ideArguments"),
    (Field::Environment, "ide.ideEnvironment"),
    (Field::Executable, "ide.ideExecutable"),
    (Field::Flags, "ide.ideBuildFlags"),
    (Field::BuildDirectory, "ide.ideBuildDirectory"),
    (Field::Target, "ide.ideBuildTarget"),
    (Field::BuildExecutable, "ide.ideBuildExecutable"),
    (Field::ConfigurePreset, "ide.ideConfigurePreset"),
    (Field::BuildPreset, "ide.ideBuildPreset"),
];

pub struct RunDialog {
    inputs: Vec<(Field, Entity<InputState>)>,
    language: &'static str,
    cmake: bool,
    root_id: Option<Value>,
    /// The saved profile being edited: saving under another name renames it.
    saved_name: Option<String>,
    advanced: bool,
    error: Option<SharedString>,
}

impl RunDialog {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let inputs = FIELDS.iter().map(|(field, _)| (*field, cx.new(|cx| InputState::new(window, cx)))).collect();
        let mut dialog = Self { inputs, language: "python", cmake: false, root_id: None, saved_name: None, advanced: false, error: None };
        // The pinned profile, else the open file's saved or extension-derived settings.
        let ide = Ide::global(cx);
        let (target, active) = {
            let ide = ide.read(cx);
            (ide.run_target(ide.active.as_deref()).map(|(configuration, _)| configuration), ide.active.clone())
        };
        let profile = target.unwrap_or_else(|| {
            let key = active.unwrap_or_default();
            let (root, path) = split_key(&key);
            let mut profile = json!({ "name": path, "program": path, "language": run_language(&key).unwrap_or("python"), "terminal": true });
            if let Some(root) = root {
                profile["rootId"] = json!(root);
            }
            profile
        });
        dialog.load(&profile, window, cx);
        dialog
    }

    fn input(&self, field: Field) -> &Entity<InputState> {
        &self.inputs.iter().find(|(known, _)| *known == field).expect("every field has an input").1
    }

    fn value(&self, field: Field, cx: &App) -> String {
        self.input(field).read(cx).value().trim().to_owned()
    }

    fn load(&mut self, profile: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
        let build = &profile["build"];
        let cmake = build["kind"] == "cmake";
        let values = [
            (Field::Name, text(&profile["name"])),
            (Field::Program, text(&profile["program"])),
            (Field::PythonModule, text(&profile["pythonModule"])),
            (Field::Cwd, text(&profile["cwd"])),
            (Field::Arguments, serde_json::to_string(profile.get("arguments").filter(|value| value.is_array()).unwrap_or(&json!([]))).unwrap_or_default()),
            (Field::Environment, serde_json::to_string(profile.get("environment").filter(|value| value.is_object()).unwrap_or(&json!({}))).unwrap_or_default()),
            (Field::Executable, text(&profile["executable"])),
            (Field::Flags, serde_json::to_string(if cmake { &Value::Null } else { &build["flags"] }).ok().filter(|flags| flags != "null").unwrap_or_else(|| "[]".to_owned())),
            (Field::BuildDirectory, if cmake { text(&build["buildDirectory"]) } else { "build".to_owned() }),
            (Field::Target, if cmake { text(&build["target"]) } else { String::new() }),
            (Field::BuildExecutable, if cmake { text(&build["executable"]) } else { String::new() }),
            (Field::ConfigurePreset, if cmake { text(&build["configurePreset"]) } else { String::new() }),
            (Field::BuildPreset, if cmake { text(&build["buildPreset"]) } else { String::new() }),
        ];
        for (field, value) in values {
            self.input(field).clone().update(cx, |state, cx| state.set_value(value, window, cx));
        }
        let language = profile["language"].as_str().unwrap_or("python");
        self.language = RUN_LANGUAGES.iter().find(|(id, _)| *id == language).map_or("python", |(id, _)| *id);
        self.cmake = cmake;
        self.root_id = profile.get("rootId").filter(|root| !root.is_null()).cloned();
        let name = profile["name"].as_str().unwrap_or_default();
        let known = Ide::global(cx).read(cx).execution()["profiles"].as_array().is_some_and(|profiles| profiles.iter().any(|known| known["name"] == name));
        self.saved_name = known.then(|| name.to_owned());
        self.error = None;
        cx.notify();
    }

    fn compiled(&self) -> bool {
        self.language == "c" || self.language == "cpp"
    }

    /// The profile as typed, or why it cannot be saved.
    fn profile(&self, cx: &App) -> Result<Value, SharedString> {
        let invalid = || tr(cx, "ide.ideProfileInvalid");
        let strings = |field: Field| -> Result<Vec<String>, SharedString> {
            serde_json::from_str::<Vec<String>>(&self.value(field, cx)).map_err(|_| invalid())
        };
        let arguments = strings(Field::Arguments)?;
        let environment: Map<String, Value> = serde_json::from_str::<Map<String, Value>>(&self.value(Field::Environment, cx)).map_err(|_| invalid())?;
        // The Host takes names a shell takes, and keeps RAINY_ for itself.
        let name_ok = |name: &str| {
            let mut chars = name.chars();
            chars.next().is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
                && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
                && !name.to_ascii_uppercase().starts_with("RAINY_")
        };
        if environment.iter().any(|(name, value)| !value.is_string() || !name_ok(name)) {
            return Err(invalid());
        }
        let flags = strings(Field::Flags)?;
        let (name, program) = (self.value(Field::Name, cx), self.value(Field::Program, cx));
        let (directory, target, executable) = (self.value(Field::BuildDirectory, cx), self.value(Field::Target, cx), self.value(Field::BuildExecutable, cx));
        if name.is_empty() || program.is_empty() || (self.compiled() && self.cmake && (directory.is_empty() || target.is_empty() || executable.is_empty())) {
            return Err(invalid());
        }
        let mut profile = json!({ "name": name, "language": self.language, "program": program, "terminal": true, "arguments": arguments, "environment": environment });
        if let Some(root) = &self.root_id {
            profile["rootId"] = root.clone();
        }
        let optional = |profile: &mut Value, key: &str, value: String| {
            if !value.is_empty() {
                profile[key] = json!(value);
            }
        };
        if self.language == "python" {
            optional(&mut profile, "pythonModule", self.value(Field::PythonModule, cx));
        }
        optional(&mut profile, "cwd", self.value(Field::Cwd, cx));
        optional(&mut profile, "executable", self.value(Field::Executable, cx));
        if self.compiled() {
            profile["build"] = if self.cmake {
                let mut build = json!({ "kind": "cmake", "buildDirectory": directory, "target": target, "executable": executable });
                optional(&mut build, "configurePreset", self.value(Field::ConfigurePreset, cx));
                optional(&mut build, "buildPreset", self.value(Field::BuildPreset, cx));
                build
            } else {
                json!({ "kind": "single-file", "flags": flags })
            };
        }
        Ok(profile)
    }

    /// Save; returns whether the dialog may close.
    fn save(&mut self, cx: &mut Context<Self>) -> bool {
        let profile = match self.profile(cx) {
            Ok(profile) => profile,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return false;
            }
        };
        let replacing = self.saved_name.clone();
        let saved = Ide::global(cx).update(cx, |ide, cx| ide.save_profile(profile, replacing.as_deref(), cx));
        match saved {
            Ok(()) => true,
            Err(name) => {
                self.error = Some(trf(cx, "ide.ideProfileNameTaken", &[("name", &name)]));
                cx.notify();
                false
            }
        }
    }

    fn field(&self, field: Field, cx: &App) -> impl IntoElement + use<> {
        let label = FIELDS.iter().find(|(known, _)| *known == field).map(|(_, label)| tr(cx, label)).unwrap_or_default();
        v_flex().gap_1().child(div().text_sm().child(label)).child(Input::new(self.input(field)).small())
    }
}

impl Render for RunDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let profiles: Vec<Value> = Ide::global(cx).read(cx).execution()["profiles"].as_array().cloned().unwrap_or_default();
        let current = self.saved_name.clone();
        let this = cx.entity();
        let languages = h_flex().gap_1().flex_wrap().children(RUN_LANGUAGES.iter().map(|(id, name)| {
            Button::new(SharedString::from(format!("run-language-{id}")))
                .map(|button| if self.language == *id { button.primary() } else { button.outline() })
                .xsmall()
                .label(*name).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.language = id;
                    cx.notify();
                },
            ))
        }));
        let compiled = self.compiled();
        v_flex()
            .id("run-dialog")
            .gap_3()
            .max_h(px(520.))
            .overflow_y_scroll()
            .when(!profiles.is_empty(), |this_el| {
                let this = this.clone();
                this_el.child(
                    h_flex().gap_2().items_center().child(div().text_sm().child(tr(cx, "ide.ideRunProfiles"))).child(
                        Button::new("run-profiles")
                            .outline()
                            .small()
                            .label(current.clone().unwrap_or_else(|| "—".to_owned()))
                            .dropdown_menu(move |mut menu, _, _| {
                                for profile in &profiles {
                                    let name = profile["name"].as_str().unwrap_or_default().to_owned();
                                    let (this, profile) = (this.clone(), profile.clone());
                                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(current.as_deref() == Some(name.as_str())).on_click(move |_, window, cx| {
                                        this.update(cx, |this, cx| this.load(&profile, window, cx));
                                    }));
                                }
                                menu
                            }),
                    ),
                )
            })
            .child(self.field(Field::Name, cx))
            .child(v_flex().gap_1().child(div().text_sm().child(tr(cx, "ide.ideLanguage"))).child(languages))
            .child(self.field(Field::Program, cx))
            .when(compiled, |this| {
                this.child(
                    v_flex().gap_1().child(div().text_sm().child(tr(cx, "ide.ideBuild"))).child(
                        h_flex()
                            .gap_1()
                            .child(Button::new("build-single")
                                .map(|button| if self.cmake { button.outline() } else { button.primary() })
                                .xsmall()
                                .label(tr(cx, "ide.ideSingleFile")).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.cmake = false;
                                    cx.notify();
                                },
                            )))
                            .child(Button::new("build-cmake")
                                .map(|button| if self.cmake { button.primary() } else { button.outline() })
                                .xsmall()
                                .label(tr(cx, "ide.ideCmake")).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.cmake = true;
                                    cx.notify();
                                },
                            ))),
                    ),
                )
                .when(self.cmake, |this| {
                    this.child(self.field(Field::BuildDirectory, cx)).child(self.field(Field::Target, cx)).child(self.field(Field::BuildExecutable, cx))
                })
            })
            .child(
                h_flex()
                    .id("run-advanced")
                    .gap_1()
                    .items_center()
                    .text_sm()
                    .cursor_pointer()
                    .text_color(theme.muted_foreground)
                    .child(gpui_kit::component::Icon::new(if self.advanced { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
                    .child(tr(cx, "ide.ideAdvanced"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.advanced = !this.advanced;
                        cx.notify();
                    })),
            )
            .when(self.advanced, |this| {
                this.when(self.language == "python", |this| this.child(self.field(Field::PythonModule, cx)))
                    .child(self.field(Field::Cwd, cx))
                    .child(self.field(Field::Arguments, cx))
                    .child(self.field(Field::Environment, cx))
                    .child(self.field(Field::Executable, cx))
                    .when(compiled && !self.cmake, |this| this.child(self.field(Field::Flags, cx)))
                    .when(compiled && self.cmake, |this| this.child(self.field(Field::ConfigurePreset, cx)).child(self.field(Field::BuildPreset, cx)))
            })
            .when_some(self.error.clone(), |this, error| this.child(div().text_sm().text_color(theme.danger).font_medium().child(error)))
    }
}

/// Open the run configurations of the shown project.
pub fn open(window: &mut Window, cx: &mut App) {
    if Ide::global(cx).read(cx).workspace.is_none() {
        return;
    }
    let view = cx.new(|cx| RunDialog::new(window, cx));
    window.open_dialog(cx, move |dialog, _, cx| {
        let (shown, saving) = (view.clone(), view.clone());
        dialog
            .title(tr(cx, "ide.ideRunProfiles"))
            .w(px(560.))
            .child(shown)
            .footer(
                DialogFooter::new()
                    .child(Button::new("run-cancel").outline().label(tr(cx, "ide.ideCancel")).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("run-save").primary().label(tr(cx, "ide.ideSave")).on_click(move |_, window, cx| {
                        if saving.update(cx, |dialog, cx| dialog.save(cx)) {
                            window.close_dialog(cx);
                        }
                    })),
            )
    });
}
