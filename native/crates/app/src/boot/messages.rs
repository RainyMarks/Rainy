//! Localized messages and formatting helpers for startup, integrity verification and module downloads.

use std::sync::Mutex;
use std::time::Instant;

use rainy_carrier::download::DownloadError;
use rainy_carrier::modules::ModuleError;
use rainy_carrier::trust::TrustError;

use super::PROGRESS_INTERVAL;
use crate::i18n::{self, Locale};

/// Whole megabytes, for progress text.
pub(super) fn megabytes(bytes: u64) -> u64 {
    (bytes as f64 / 1_048_576.0).round() as u64
}

/// Whether progress is due: at most every [`PROGRESS_INTERVAL`], and always the last step.
pub(super) fn due(shown: &Mutex<Option<Instant>>, last: bool) -> bool {
    let mut shown = shown.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if !last && shown.is_some_and(|at| at.elapsed() < PROGRESS_INTERVAL) {
        return false;
    }
    *shown = Some(Instant::now());
    true
}

/// What the window says about resources that were not accepted.
pub fn trust_message(locale: Locale, error: &TrustError) -> String {
    let with = |key: &str, name: &str, value: &str| i18n::format(locale, key, &[(name, &value)]);
    match error {
        TrustError::Invalid(detail) => with("native.trustInvalid", "detail", detail),
        TrustError::Encoding => i18n::text(locale, "native.trustEncoding").to_owned(),
        TrustError::Signature => i18n::text(locale, "native.trustSignature").to_owned(),
        TrustError::DuplicatePaths => i18n::text(locale, "native.trustDuplicates").to_owned(),
        TrustError::Outside => i18n::text(locale, "native.trustOutside").to_owned(),
        TrustError::Missing(path) => with("native.trustMissing", "path", path),
        TrustError::Damaged(path) => with("native.trustDamaged", "path", path),
        TrustError::Replaced(path) => with("native.trustReplaced", "path", path),
        TrustError::Mismatch(path) => with("native.trustMismatch", "path", path),
        TrustError::Io(detail) => with("native.trustUnreadable", "detail", detail),
    }
}

/// What the window says about a module operation that failed.
pub fn module_message(locale: Locale, error: &ModuleError) -> String {
    let with = |key: &str, name: &str, value: &str| i18n::format(locale, key, &[(name, &value)]);
    match error {
        ModuleError::Unavailable => i18n::text(locale, "native.modulesUnavailable").to_owned(),
        ModuleError::Invalid(detail) => with("native.modulesInvalid", "detail", detail),
        ModuleError::NotOffered(id) => with("native.moduleNotOffered", "id", id),
        ModuleError::Busy => i18n::text(locale, "native.moduleBusy").to_owned(),
        ModuleError::Closing => i18n::text(locale, "native.moduleClosing").to_owned(),
        ModuleError::RemoveBusy => i18n::text(locale, "native.moduleRemoveBusy").to_owned(),
        ModuleError::Archive(detail) => with("native.moduleArchive", "detail", detail),
        ModuleError::Download(error) => download_message(locale, error),
    }
}

/// What the window says about a download that failed.
pub fn download_message(locale: Locale, error: &DownloadError) -> String {
    let with = |key: &str, name: &str, value: &dyn std::fmt::Display| i18n::format(locale, key, &[(name, value)]);
    let text = |key: &str| i18n::text(locale, key).to_owned();
    match error {
        DownloadError::Insecure(url) => with("native.downloadInsecure", "url", url),
        DownloadError::Redirects => text("native.downloadRedirects"),
        DownloadError::Network(detail) => with("native.downloadNetwork", "detail", detail),
        DownloadError::Stalled(seconds) => with("native.downloadStalled", "seconds", seconds),
        DownloadError::Interrupted => text("native.downloadInterrupted"),
        DownloadError::Status(status) => with("native.downloadHttp", "status", status),
        DownloadError::InvalidResume => text("native.downloadResume"),
        DownloadError::InvalidResponse => text("native.downloadResponse"),
        DownloadError::TooLarge => text("native.downloadTooLarge"),
        DownloadError::Mismatch => text("native.downloadMismatch"),
        DownloadError::FileMismatch => text("native.downloadFileMismatch"),
        DownloadError::InvalidCache => text("native.downloadCache"),
        DownloadError::Link(path) => with("native.downloadLink", "path", path),
        DownloadError::Cancelled => text("native.downloadCancelled"),
        DownloadError::Io(detail) => with("native.downloadIo", "detail", detail),
    }
}

/// A count with its thousands separated, as `toLocaleString` writes it in Chinese and in English.
pub(crate) fn grouped(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}
