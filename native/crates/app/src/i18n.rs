//! Chinese and English interface strings, exported from `src/renderer/**/messages.ts` by
//! `native/scripts/export-messages.mts`. Keys are `module.key`, such as `chat.send`; `{name}` placeholders are filled
//! from the variables given.

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Locale {
    #[default]
    Zh,
    En,
}

impl Locale {
    pub fn from_code(code: &str) -> Self {
        if code == "en" { Self::En } else { Self::Zh }
    }
}

type Table = HashMap<String, String>;

/// Strings that Electron main used to show and the page never had: `(key, Chinese, English)`.
const NATIVE: &[(&str, &str, &str)] = &[
    ("native.starting", "正在启动执行后端…", "Starting the execution backend…"),
    ("native.quit", "退出", "Quit"),
    ("native.hostStopped", "执行后端已停止", "The execution backend stopped."),
    ("native.switchTarget", "切换执行环境", "Switch execution environment"),
    ("native.switchConfirm", "切换到 {target} 后 Rainy 会重新启动。已有对话保留在原来的执行环境。", "Rainy restarts in {target}. Existing chats stay in their execution environment."),
    ("native.switch", "切换", "Switch"),
    ("native.busyBeforeSwitch", "执行环境里还有任务在运行，请先结束再切换。", "Tasks are still running in this execution environment. Finish them before you switch."),
    ("native.retry", "重试", "Retry"),
    ("native.useWindows", "改用 Windows 原生运行", "Run on Windows instead"),
    ("native.keepMine", "保留我的修改并覆盖", "Keep my edits and overwrite"),
    ("native.projects", "项目", "Projects"),
];

fn tables() -> &'static (Table, Table) {
    static TABLES: OnceLock<(Table, Table)> = OnceLock::new();
    TABLES.get_or_init(|| {
        let modules: HashMap<String, HashMap<String, HashMap<String, String>>> =
            serde_json::from_str(include_str!("../assets/messages.json")).expect("valid messages.json");
        let (mut zh, mut en) = (Table::new(), Table::new());
        for (module, languages) in modules {
            for (language, target) in [("zh", &mut zh), ("en", &mut en)] {
                for (key, text) in &languages[language] {
                    target.insert(format!("{module}.{key}"), text.clone());
                }
            }
        }
        for (key, chinese, english) in NATIVE {
            zh.insert((*key).to_owned(), (*chinese).to_owned());
            en.insert((*key).to_owned(), (*english).to_owned());
        }
        (zh, en)
    })
}

/// Look up one message; an unknown key shows itself, so a missing string is visible rather than blank.
pub fn text(locale: Locale, key: &str) -> &'static str {
    let (zh, en) = tables();
    let table = if locale == Locale::En { en } else { zh };
    table.get(key).map(String::as_str).unwrap_or_else(|| Box::leak(key.to_owned().into_boxed_str()))
}

/// Look up one message and fill its `{name}` placeholders.
pub fn format(locale: Locale, key: &str, vars: &[(&str, &dyn std::fmt::Display)]) -> String {
    let template = text(locale, key);
    if vars.is_empty() {
        return template.to_owned();
    }
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) if after[..close].chars().all(|c| c.is_alphanumeric() || c == '_') => {
                let name = &after[..close];
                match vars.iter().find(|(var, _)| *var == name) {
                    Some((_, value)) => out.push_str(&value.to_string()),
                    None => out.push_str(&rest[open..open + close + 2]),
                }
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_and_formats() {
        assert_eq!(text(Locale::Zh, "chat.send"), "发送");
        assert_eq!(text(Locale::En, "chat.send"), "Send");
        assert_eq!(format(Locale::Zh, "chat.exitCode", &[("code", &3)]), "退出码 3");
        assert_eq!(format(Locale::Zh, "chat.toolRead", &[]), "读取 {path}");
        assert_eq!(text(Locale::Zh, "chat.noSuchKey"), "chat.noSuchKey");
    }
}
