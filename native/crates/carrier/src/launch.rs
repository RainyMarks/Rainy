//! Files and folders handed to Rainy on its command line: by Explorer's "Open with Rainy", by a file whose default app
//! is Rainy, or from a terminal (`launch-paths.ts` in 3.x).

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf, Prefix, PrefixComponent};

/// The switch a restart passes with the process id of the Rainy it replaces: the new process waits for that one to
/// exit before it takes the single-instance lock.
pub const WAIT_PID: &str = "--rainy-wait-pid";
/// Switches whose value may follow as the next argument; that value is not a path.
const SWITCHES_WITH_VALUE: [&str; 2] = ["--user-data-dir", WAIT_PID];

/// Whether a launch path is a file or a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchKind {
    File,
    Directory,
}

/// One path to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchEntry {
    pub path: PathBuf,
    pub kind: LaunchKind,
}

/// The paths in a process's arguments (`argv[0]` first), resolved against the folder it was started in, in the order
/// given. Until a `--`, arguments starting with `-` are switches (Rainy's and the installer's) and are skipped, and so
/// is the value after a space-separated `--user-data-dir`; empty, blank and NUL-containing arguments are skipped too.
pub fn launch_paths(argv: &[OsString], cwd: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut positional = false;
    let mut arguments = argv.iter().skip(1);
    while let Some(argument) = arguments.next() {
        if !positional && argument == "--" {
            positional = true;
            continue;
        }
        if !positional && argument.as_encoded_bytes().starts_with(b"-") {
            if SWITCHES_WITH_VALUE.iter().any(|switch| argument == *switch) {
                arguments.next();
            }
            continue;
        }
        let text = argument.to_string_lossy();
        if text.chars().all(js_whitespace) || text.contains('\0') {
            continue;
        }
        // Explorer's `"%V"` for a drive root is `"D:\"`, whose `\"` the command line reads as a quote: `D:"`. No
        // Windows path holds a quote, so a trailing one was that backslash.
        if cfg!(windows)
            && let Some(stripped) = argument.to_str().and_then(|text| text.strip_suffix('"'))
        {
            paths.push(resolve(cwd, Path::new(&format!("{stripped}\\"))));
            continue;
        }
        paths.push(resolve(cwd, Path::new(argument)));
    }
    paths
}

/// The value of a switch given as `--name=value` or `--name value` before any `--`.
pub fn switch_value(argv: &[OsString], name: &str) -> Option<OsString> {
    let prefix = format!("{name}=");
    let mut arguments = argv.iter().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            break;
        }
        if argument == name {
            return arguments.next().cloned();
        }
        if let Some(value) = argument.as_encoded_bytes().strip_prefix(prefix.as_bytes()) {
            // SAFETY: the bytes come from an `OsStr` and are split right after an ASCII character.
            return Some(unsafe { OsStr::from_encoded_bytes_unchecked(value) }.to_owned());
        }
    }
    None
}

/// The process id given with `--rainy-wait-pid`.
pub fn wait_pid(argv: &[OsString]) -> Option<u32> {
    switch_value(argv, WAIT_PID)?.to_str()?.parse().ok()
}

/// The arguments that start Rainy again in place of process `pid`: this run's switches (with the value of a
/// space-separated `--user-data-dir`) and `--rainy-wait-pid=<pid>`, without the paths it was launched with, which are
/// open already.
pub fn relaunch_arguments(argv: &[OsString], pid: u32) -> Vec<OsString> {
    let mut kept = Vec::new();
    let mut arguments = argv.iter().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            break;
        }
        let bytes = argument.as_encoded_bytes();
        if !bytes.starts_with(b"-") {
            continue;
        }
        let with_value = SWITCHES_WITH_VALUE.iter().any(|switch| argument == *switch);
        let value = if with_value { arguments.next() } else { None };
        if argument == WAIT_PID || bytes.starts_with(format!("{WAIT_PID}=").as_bytes()) {
            continue;
        }
        kept.push(argument.clone());
        kept.extend(value.cloned());
    }
    kept.push(format!("{WAIT_PID}={pid}").into());
    kept
}

/// Which of the paths exist, as files or folders, and which do not (anything else counts as missing).
pub fn inspect(paths: &[PathBuf]) -> (Vec<LaunchEntry>, Vec<PathBuf>) {
    let mut entries = Vec::new();
    let mut missing = Vec::new();
    for path in paths {
        match std::fs::metadata(path) {
            Ok(info) if info.is_dir() => entries.push(LaunchEntry { path: path.clone(), kind: LaunchKind::Directory }),
            Ok(info) if info.is_file() => entries.push(LaunchEntry { path: path.clone(), kind: LaunchKind::File }),
            _ => missing.push(path.clone()),
        }
    }
    (entries, missing)
}

/// `path.win32.resolve(cwd, argument)`: an absolute path with `.`, `..`, repeated and trailing separators folded.
fn resolve(cwd: &Path, argument: &Path) -> PathBuf {
    let mut parts = argument.components();
    let joined = match parts.next() {
        // `D:notes.md` is relative to the caller's folder when that is on drive D:, otherwise to the drive's root.
        Some(Component::Prefix(drive)) if !argument.has_root() => {
            let folder = match cwd.components().next() {
                Some(Component::Prefix(current)) if current.kind() == drive.kind() => cwd.strip_prefix(current.as_os_str()).unwrap_or(cwd),
                _ => Path::new("\\"),
            };
            PathBuf::from(drive.as_os_str()).join(folder).join(parts.as_path())
        }
        _ => cwd.join(argument),
    };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix_text(prefix)),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// A drive, share or device with backslashes, as Node writes it (`//server/share` becomes `\\server\share`).
fn prefix_text(prefix: PrefixComponent<'_>) -> OsString {
    let (start, first, second) = match prefix.kind() {
        Prefix::UNC(server, share) => (r"\\", server, Some(share)),
        Prefix::DeviceNS(device) => (r"\\.\", device, None),
        _ => return prefix.as_os_str().to_owned(),
    };
    let mut text = OsString::from(start);
    text.push(first);
    if let Some(second) = second {
        text.push(r"\");
        text.push(second);
    }
    text
}

/// Whether JavaScript's `trim()` removes the character: Unicode white space without U+0085, plus U+FEFF.
fn js_whitespace(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

#[cfg(all(test, windows))]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    fn argv(arguments: &[&str]) -> Vec<OsString> {
        arguments.iter().map(OsString::from).collect()
    }

    fn paths(arguments: &[&str], cwd: &str) -> Vec<String> {
        launch_paths(&argv(arguments), Path::new(cwd)).iter().map(|path| path.display().to_string()).collect()
    }

    #[test]
    fn takes_the_paths_after_the_executable_and_skips_switches() {
        assert_eq!(
            paths(&[r"C:\Rainy\RainyAgent.exe", "--updated", r"C:\Notes\a b.md", r"--user-data-dir=C:\x", r"D:\Project"], r"C:\"),
            [r"C:\Notes\a b.md", r"D:\Project"]
        );
        // `--` ends the switches.
        assert_eq!(paths(&["RainyAgent.exe", "--", "-notes.md"], r"C:\Work"), [r"C:\Work\-notes.md"]);
        assert_eq!(paths(&["RainyAgent.exe", "."], r"C:\Users\me\project"), [r"C:\Users\me\project"]);
        assert!(paths(&["RainyAgent.exe"], r"C:\").is_empty());
        // The space-separated user-data folder is not a path; after `--` the switch itself is one.
        assert_eq!(paths(&["Rainy.exe", "--user-data-dir", r"C:\qa", "notes.md"], r"C:\Work"), [r"C:\Work\notes.md"]);
        assert_eq!(paths(&["Rainy.exe", "--", "--user-data-dir", "x"], r"C:\Work"), [r"C:\Work\--user-data-dir", r"C:\Work\x"]);
        assert!(paths(&["Rainy.exe", "", "  ", "\u{3000}\u{feff}", "a\0b"], r"C:\Work").is_empty());
        assert_eq!(paths(&["Rainy.exe", "\u{85}"], r"C:\Work"), ["C:\\Work\\\u{85}"]);
        // "Open with Rainy" on the empty part of a drive root: Explorer runs `Rainy.exe "D:\"`, which arrives as `D:"`.
        assert_eq!(paths(&["Rainy.exe", "D:\""], r"C:\Work"), [r"D:\"]);
        assert_eq!(paths(&["Rainy.exe", "D:\\项目\""], r"C:\Work"), [r"D:\项目"]);
    }

    #[test]
    fn resolves_like_node_win32_resolve() {
        // Each expected value is what `path.win32.resolve(cwd, argument)` returns in Node 24.
        let cases = [
            (r"C:\Work", r"..\x/y/./z\", r"C:\x\y\z"),
            (r"C:\Work", r"\root\f.md", r"C:\root\f.md"),
            (r"C:\Work", "c:foo", r"c:\Work\foo"),
            (r"C:\Work", r"D:foo\bar", r"D:\foo\bar"),
            (r"C:\Work", "D:", r"D:\"),
            (r"C:\Work", r"D:\", r"D:\"),
            (r"C:\", "C:", r"C:\"),
            (r"C:\Work", "//srv/share/a/../b", r"\\srv\share\b"),
            (r"C:\Work", "//./COM1/x", r"\\.\COM1\x"),
            (r"\\srv\share\dir", "x.md", r"\\srv\share\dir\x.md"),
            (r"\\srv\share\dir", r"\y", r"\\srv\share\y"),
            (r"\\srv\share", r"..\..\x", r"\\srv\share\x"),
            (r"C:\Work", r"..\..\..", r"C:\"),
            (r"C:\Work", r"a.\b ", r"C:\Work\a.\b "),
            (r"C:\Work", r"\\?\C:\x\..\y", r"\\?\C:\y"),
            (r"C:\Work\", r"sub\\\dbl", r"C:\Work\sub\dbl"),
            (r"C:\Work", r"C:\Data\..\Notes\.\b.md", r"C:\Notes\b.md"),
            (r"C:\Work", r"笔记\说明.md", r"C:\Work\笔记\说明.md"),
            (r"C:\Work", "C:\"", r#"C:\Work\""#),
        ];
        for (cwd, argument, expected) in cases {
            assert_eq!(resolve(Path::new(cwd), Path::new(argument)).display().to_string(), expected, "{cwd} + {argument}");
        }
    }

    #[test]
    fn keeps_arguments_that_are_not_unicode() {
        use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
        // A lone surrogate, which `std::env::args` would panic on.
        let name = OsString::from_wide(&[0x61, 0xd800, 0x2e, 0x6d, 0x64]);
        let found = launch_paths(&[OsString::from("Rainy.exe"), name.clone()], Path::new(r"C:\Work"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].as_os_str().encode_wide().collect::<Vec<_>>(), [r"C:\Work\".encode_utf16().collect(), name.encode_wide().collect::<Vec<_>>()].concat());
        let mut switch = OsString::from("--user-data-dir=");
        switch.push(&name);
        assert_eq!(switch_value(&[OsString::from("Rainy.exe"), switch], "--user-data-dir"), Some(name));
    }

    #[test]
    fn reads_switch_values() {
        let given = argv(&["Rainy.exe", "--updated", r"--user-data-dir=C:\qa", "--", "--rainy-wait-pid=4"]);
        assert_eq!(switch_value(&given, "--user-data-dir"), Some(OsString::from(r"C:\qa")));
        assert_eq!(switch_value(&given, "--rainy-wait-pid"), None);
        assert_eq!(switch_value(&argv(&["Rainy.exe", "--user-data-dir", "qa"]), "--user-data-dir"), Some(OsString::from("qa")));
        assert_eq!(switch_value(&argv(&["Rainy.exe", "--user-data-dir"]), "--user-data-dir"), None);
        assert_eq!(switch_value(&argv(&["--user-data-dir=argv0"]), "--user-data-dir"), None);
    }

    #[test]
    fn restarts_without_the_launch_paths() {
        let strings = |arguments: Vec<OsString>| arguments.into_iter().map(|argument| argument.into_string().unwrap()).collect::<Vec<_>>();
        let given = argv(&["Rainy.exe", "--updated", r"C:\Notes\a.md", "--user-data-dir", r"C:\qa", r"D:\Project", "--", "-b.md"]);
        assert_eq!(strings(relaunch_arguments(&given, 4242)), ["--updated", "--user-data-dir", r"C:\qa", "--rainy-wait-pid=4242"]);
        // The process an earlier restart waited for is replaced by this one.
        let restarted = argv(&["Rainy.exe", r"--user-data-dir=C:\qa", "--rainy-wait-pid=17", "--rainy-wait-pid", "18"]);
        assert_eq!(strings(relaunch_arguments(&restarted, 99)), [r"--user-data-dir=C:\qa", "--rainy-wait-pid=99"]);
        assert_eq!(strings(relaunch_arguments(&argv(&["Rainy.exe"]), 1)), ["--rainy-wait-pid=1"]);
        // A restart opens nothing.
        assert!(launch_paths(&relaunch_arguments(&given, 4242), Path::new(r"C:\Work")).is_empty());
        assert!(launch_paths(&argv(&["Rainy.exe", "--rainy-wait-pid", "18"]), Path::new(r"C:\Work")).is_empty());
        assert_eq!(wait_pid(&relaunch_arguments(&given, 4242)), Some(4242));
        assert_eq!(wait_pid(&argv(&["Rainy.exe", "--rainy-wait-pid", "18"])), Some(18));
        assert_eq!(wait_pid(&argv(&["Rainy.exe", "--rainy-wait-pid=x"])), None);
        assert_eq!(wait_pid(&argv(&["Rainy.exe", "--", "--rainy-wait-pid=5"])), None);
    }

    #[test]
    fn tells_files_from_folders_and_reports_what_is_missing() {
        let folder = tempfile::tempdir().unwrap();
        let notes = folder.path().join("notes.md");
        std::fs::write(&notes, "# Notes\n").unwrap();
        let gone = folder.path().join("gone.md");
        let (entries, missing) = inspect(&[notes.clone(), folder.path().to_path_buf(), gone.clone()]);
        assert_eq!(
            entries,
            [
                LaunchEntry { path: notes, kind: LaunchKind::File },
                LaunchEntry { path: folder.path().to_path_buf(), kind: LaunchKind::Directory }
            ]
        );
        assert_eq!(missing, [gone]);
    }
}
