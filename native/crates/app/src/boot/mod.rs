//! Where Rainy keeps its files, and how it starts the Host (`start()` in `src/main/main.ts`).

mod events;
mod messages;
mod paths;
mod targets;

use std::time::Duration;

pub use events::{CarrierEvents, CredentialAlert};
pub(crate) use messages::grouped;
pub use messages::{download_message, module_message, trust_message};
pub use paths::{Paths, clean_up, recheck_resources, saved_locale, user_data_dir, user_data_path, verify_resources};
pub use targets::{Prune, RuntimeChoice, reconnect_ssh, ssh_profile, start, start_target};

/// How long after the workbench is ready the background maintenance starts (3.x `BACKGROUND_CHECK_DELAY_MS`).
pub const MAINTENANCE_DELAY: Duration = Duration::from_secs(60);
/// Startup progress is shown at most this often.
pub(super) const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

/// One startup step for the window: what is happening and, when it is known, how much of it is done (0 to 1).
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub text: String,
    pub fraction: Option<f32>,
}

impl From<String> for Step {
    fn from(text: String) -> Self {
        Self { text, fraction: None }
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;
    use std::path::Path;

    use rainy_carrier::download::DownloadError;
    use rainy_carrier::modules::{ModuleError, Modules};
    use rainy_carrier::trust::TrustError;

    use super::*;
    use super::targets::runtime_files;
    use crate::i18n::{self, Locale};

    #[test]
    fn counts_files_as_3x_shows_them() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(9015), "9,015");
        assert_eq!(grouped(1_234_567), "1,234,567");
        let text = i18n::format(Locale::Zh, "native.verifying", &[("done", &grouped(1200)), ("total", &grouped(9015))]);
        assert_eq!(text, "首次启动，正在校验安装文件 1,200 / 9,015");
    }

    #[test]
    fn a_checking_build_runs_only_installed_resources() {
        let dir = tempfile::tempdir().unwrap();
        let host = |resources: &Path| std::fs::create_dir_all(resources.join("windows-host")).unwrap();
        let repo = dir.path().join("Rainy");
        let built = repo.join("native").join("target").join("release");
        let install = dir.path().join("Programs").join("RainyAgent");
        let moved = dir.path().join("Copied").join("RainyAgent");
        let named = dir.path().join("named");
        host(&repo.join("tmp").join("dev-resources"));
        host(&dir.path().join("tmp").join("dev-resources"));
        host(&named);

        host(&install.join("resources"));
        for checks in [true, false] {
            let found = paths::find_resources(&install, None, checks);
            assert_eq!(found, Some((install.join("resources"), true)));
        }
        assert_eq!(paths::find_resources(&install, Some(named.clone()), true), Some((install.join("resources"), true)));
        for root in [&built, &moved] {
            assert_eq!(paths::find_resources(root, None, true), None);
            assert_eq!(paths::find_resources(root, Some(named.clone()), true), None);
        }
        host(&built.join("resources"));
        assert_eq!(paths::find_resources(&built, None, true), None);

        assert_eq!(paths::find_resources(&install, Some(named.clone()), false), Some((named.clone(), false)));
        assert_eq!(paths::find_resources(&built, None, false), Some((built.join("resources"), false)));
        std::fs::remove_dir_all(built.join("resources")).unwrap();
        assert_eq!(paths::find_resources(&built, None, false), Some((repo.join("tmp").join("dev-resources"), false)));
        assert_eq!(paths::find_resources(&moved, None, false), None);
    }

    #[test]
    fn explains_module_and_download_failures() {
        let downloads = [
            DownloadError::Insecure("http://example.com/x".into()),
            DownloadError::Redirects,
            DownloadError::Network("WinHTTP 12029".into()),
            DownloadError::Stalled(60),
            DownloadError::Interrupted,
            DownloadError::Status(404),
            DownloadError::InvalidResume,
            DownloadError::InvalidResponse,
            DownloadError::TooLarge,
            DownloadError::Mismatch,
            DownloadError::FileMismatch,
            DownloadError::InvalidCache,
            DownloadError::Link(r"C:\Users\x\AppData".into()),
            DownloadError::Cancelled,
            DownloadError::Io("x".into()),
        ];
        let others = [
            ModuleError::Unavailable,
            ModuleError::Invalid("x".into()),
            ModuleError::NotOffered("strata".into()),
            ModuleError::Busy,
            ModuleError::Closing,
            ModuleError::RemoveBusy,
            ModuleError::Archive("x".into()),
        ];
        for error in downloads.into_iter().map(ModuleError::Download).chain(others) {
            assert_eq!(module_message(Locale::En, &error), error.to_string());
            assert!(!module_message(Locale::Zh, &error).starts_with("native."));
        }
        assert_eq!(module_message(Locale::Zh, &ModuleError::Download(DownloadError::Status(404))), "下载失败（HTTP 404），请检查网络后重试");
        assert_eq!(module_message(Locale::Zh, &ModuleError::Busy), "另一个组件正在下载，请稍候");
        let detail = i18n::format(Locale::Zh, "native.linuxDownloadDetail", &[("error", &"下载校验失败，请重试")]);
        assert_eq!(detail, "下载校验失败，请重试\nWSL 和 SSH 远程开发用的 Linux 运行环境底座有变化，需要联网下载一次，已下载的部分会保留。");
        let step = i18n::format(Locale::Zh, "native.linuxDownloading", &[("done", &messages::megabytes(26_000_000)), ("total", &messages::megabytes(52_717_256))]);
        assert_eq!(step, "正在下载 Linux 运行环境 25 / 50 MB");
    }

    #[test]
    fn a_large_host_log_moves_aside() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("host.log");
        std::fs::write(&log, b"small").unwrap();
        events::rotate_host_log(&log);
        assert!(log.exists());
        std::fs::write(&log, vec![b'x'; events::HOST_LOG_LIMIT as usize]).unwrap();
        std::fs::write(dir.path().join("host.log.1"), b"older").unwrap();
        events::rotate_host_log(&log);
        assert!(!log.exists());
        assert_eq!(std::fs::metadata(dir.path().join("host.log.1")).unwrap().len(), events::HOST_LOG_LIMIT);
    }

    #[test]
    fn development_checkouts_stage_the_linux_base() {
        let dir = tempfile::tempdir().unwrap();
        let resources = dir.path().join("resources");
        std::fs::create_dir_all(&resources).unwrap();
        let paths = Paths { user_data: dir.path().join("ud"), resources: resources.clone(), install_root: dir.path().to_path_buf(), installed: false };
        let modules = Modules::new(&resources, &paths.user_data, |_| {});
        assert_eq!(runtime_files(&paths, &modules).base, None);
        std::fs::write(resources.join("linux-runtime.tar.xz"), b"base").unwrap();
        assert_eq!(runtime_files(&paths, &modules).base, Some(resources.join("linux-runtime.tar.xz")));
    }

    #[test]
    fn explains_resources_that_were_not_accepted() {
        let path = "windows-host/node/node.exe".to_owned();
        assert_eq!(trust_message(Locale::Zh, &TrustError::Missing(path.clone())), "发行资源缺失：windows-host/node/node.exe");
        assert_eq!(trust_message(Locale::En, &TrustError::Mismatch(path.clone())), "A Rainy file failed verification: windows-host/node/node.exe");
        assert_eq!(trust_message(Locale::Zh, &TrustError::Signature), "发行清单签名无效");
        for error in [
            TrustError::Invalid("x".into()),
            TrustError::Encoding,
            TrustError::Signature,
            TrustError::DuplicatePaths,
            TrustError::Outside,
            TrustError::Missing(path.clone()),
            TrustError::Damaged(path.clone()),
            TrustError::Replaced(path.clone()),
            TrustError::Mismatch(path.clone()),
            TrustError::Io("x".into()),
        ] {
            assert_eq!(trust_message(Locale::En, &error), error.to_string());
            assert!(!trust_message(Locale::Zh, &error).starts_with("native."));
        }
    }
}
