//! The Windows save dialog with a title and one file type (3.x `dialog.showSaveDialog` with `title` and `filters`), which
//! GPUI's `prompt_for_new_path` does not offer: a name typed without the extension gets it before Windows asks about
//! replacing a file.

use std::path::PathBuf;

use anyhow::Result;
use gpui_kit::Window;
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FileSaveDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH};
use windows::core::{HRESULT, HSTRING, PCWSTR};

/// What the dialog shows.
pub struct SaveDialog {
    pub title: String,
    /// Where it opens, and the name it suggests.
    pub folder: PathBuf,
    pub name: String,
    /// The one file type offered, such as `Zip`, and its extension without the dot.
    pub type_name: String,
    pub extension: String,
}

/// Show `dialog` over `window`. It runs on a thread of its own, as GPUI's dialogs do, so the window's thread keeps
/// handling the messages the dialog sends it. The answer is the path chosen, or `None` when the dialog was cancelled.
pub fn save(window: &Window, dialog: SaveDialog) -> async_channel::Receiver<Result<Option<PathBuf>>> {
    let (sender, receiver) = async_channel::bounded(1);
    // A window handle is a number any thread may hand to the shell.
    let owner = crate::web::hwnd(window).map(|hwnd| hwnd.0 as isize);
    let shown = std::thread::Builder::new().name("save-dialog".to_owned()).spawn({
        let sender = sender.clone();
        move || {
            let _ = sender.send_blocking(in_apartment(|| ask(&dialog, owner.map(|hwnd| HWND(hwnd as *mut _)))));
        }
    });
    if let Err(error) = shown {
        let _ = sender.try_send(Err(error.into()));
    }
    receiver
}

/// Run `work` in a single-threaded COM apartment of this thread; the shell's objects are released before it ends.
fn in_apartment<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    // SAFETY: undone below; `work` releases every COM object it makes before it returns.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()?;
    let result = work();
    // SAFETY: COM was initialized on this thread above.
    unsafe { CoUninitialize() };
    result
}

/// The dialog with everything set but not shown.
fn prepared(dialog: &SaveDialog) -> windows::core::Result<IFileSaveDialog> {
    let (type_name, spec) = (HSTRING::from(dialog.type_name.as_str()), HSTRING::from(format!("*.{}", dialog.extension)));
    // SAFETY: COM calls in this thread's apartment; the strings outlive the calls, which copy them.
    unsafe {
        let file: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
        file.SetOptions(file.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT)?;
        file.SetTitle(&HSTRING::from(dialog.title.as_str()))?;
        file.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: PCWSTR(type_name.as_ptr()), pszSpec: PCWSTR(spec.as_ptr()) }])?;
        file.SetFileTypeIndex(1)?;
        file.SetDefaultExtension(&HSTRING::from(dialog.extension.as_str()))?;
        match SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dialog.folder.as_path()), None) {
            Ok(folder) => file.SetFolder(&folder)?,
            Err(error) => log::warn!("The save dialog does not open in {}: {error}", dialog.folder.display()),
        }
        file.SetFileName(&HSTRING::from(dialog.name.as_str()))?;
        Ok(file)
    }
}

fn ask(dialog: &SaveDialog, owner: Option<HWND>) -> Result<Option<PathBuf>> {
    let file = prepared(dialog)?;
    // SAFETY: COM calls in this thread's apartment; the path is freed once, after it was copied.
    unsafe {
        if let Err(error) = file.Show(owner) {
            if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
                return Ok(None);
            }
            return Err(error.into());
        }
        let path = file.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let chosen = path.to_string();
        CoTaskMemFree(Some(path.0.cast_const().cast()));
        Ok(Some(PathBuf::from(chosen?)))
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    /// The dialog is set up as 3.x's without being shown.
    #[test]
    fn offers_one_file_type_in_the_folder_with_the_name() {
        let folder = tempfile::tempdir().unwrap();
        let dialog = SaveDialog {
            title: "导出诊断信息".to_owned(),
            folder: folder.path().to_owned(),
            name: "Rainy-diagnostics-202610220930.zip".to_owned(),
            type_name: "Zip".to_owned(),
            extension: "zip".to_owned(),
        };
        let checked = std::thread::spawn(move || {
            in_apartment(|| {
                let file = prepared(&dialog)?;
                // SAFETY: COM calls in this thread's apartment; the name is freed once, after it was copied.
                unsafe {
                    assert!(file.GetOptions()?.contains(FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT));
                    assert_eq!(file.GetFileTypeIndex()?, 1);
                    let name = file.GetFileName()?;
                    let text = name.to_string();
                    CoTaskMemFree(Some(name.0.cast_const().cast()));
                    assert_eq!(text?, dialog.name);
                }
                Ok(())
            })
        });
        checked.join().unwrap().unwrap();
    }
}
