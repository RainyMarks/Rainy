//! Slides and legacy Word documents as PDF, made by the Microsoft Office or WPS Office installed on this computer
//! (`src/main/office-convert.ts`). The viewer page asks for it when it cannot draw a format itself.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;

static STAGING_SEQ: AtomicU64 = AtomicU64::new(1);

/// Ensures a temporary staging folder is always removed on error paths unless explicitly disarmed on success.
struct StagingGuard(Option<PathBuf>);

impl StagingGuard {
    fn disarm(&mut self) -> Option<PathBuf> {
        self.0.take()
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if let Some(folder) = self.0.take() {
            ConvertedFile::cleanup(folder);
        }
    }
}

/// Largest file handed to Office.
const MAX_BYTES: usize = 200 * 1024 * 1024;
/// Time allowed for starting Office and exporting.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Which Office application opens a file, by lower-case extension.
pub fn kind(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "pptx" | "pptm" | "ppsx" | "ppsm" | "potx" | "ppt" | "pps" | "pot" | "odp" | "dps" => "slides",
        "doc" | "dot" | "wps" | "odt" | "docx" | "docm" | "dotx" | "dotm" | "rtf" => "word",
        _ => return None,
    })
}

/// Whether an Office Open XML file is password-encrypted: a zip unless a password turned it into a compound file.
pub fn encrypted_open_xml(bytes: &[u8], extension: &str) -> bool {
    const OPEN_XML: [&str; 14] = ["pptx", "pptm", "ppsx", "ppsm", "potx", "docx", "docm", "dotx", "dotm", "xlsx", "xlsm", "xltx", "xltm", "xlsb"];
    OPEN_XML.contains(&extension) && bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1])
}

/// Why there is no PDF. `code` is what the page explains in the interface language: `missing` (neither Microsoft
/// Office nor WPS Office is installed), `password`, `too-large`, `timeout`, `no-output` (Office opened the file but
/// wrote no PDF), `unsupported` or `failed`; `error` is the detail as Office or Windows put it, possibly English.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: &'static str,
    pub error: String,
    pub missing: bool,
    pub encrypted: bool,
}

impl Failure {
    fn new(code: &'static str, error: impl Into<String>) -> Self {
        Self { code, error: error.into(), missing: code == "missing", encrypted: code == "password" }
    }
}

/// A PDF and the application that made it.
#[cfg(test)]
#[allow(dead_code)]
#[derive(Debug)]
pub struct Converted {
    pub pdf: Vec<u8>,
    pub application: String,
}

/// A PDF converted onto disk under `%TEMP%\rainy-office-*` so large files never round-trip through memory/base64.
#[derive(Debug, Clone)]
pub struct ConvertedFile {
    pub folder: PathBuf,
    pub pdf_path: PathBuf,
    pub application: String,
}

impl ConvertedFile {
    /// Remove the temporary folder holding this PDF (best-effort retry if Office/Host just released it).
    pub fn cleanup(folder: PathBuf) {
        for _ in 0..5 {
            if std::fs::remove_dir_all(&folder).is_ok() || !folder.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

// PowerPoint and WPS keep one process per user, so an instance that was already running belongs to the user and is
// left open; Word.Application always starts its own instance. The password argument makes Word fail on a protected
// document (error 5408) instead of asking for the password in a hidden window. SaveAs2 can wait forever on a hidden
// prompt in current Word builds, so the fallback is the older SaveAs.
const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$in = $env:RAINY_OFFICE_IN
$out = $env:RAINY_OFFICE_OUT
$slides = $env:RAINY_OFFICE_KIND -eq 'slides'
$candidates = if ($slides) { @(@('PowerPoint.Application', 'POWERPNT', 'Microsoft PowerPoint'), @('KWPP.Application', 'wpp', 'WPS Office')) }
  else { @(@('Word.Application', 'WINWORD', 'Microsoft Word'), @('KWps.Application', 'wps', 'WPS Office')) }
$failures = @()
$unwritten = @()
$found = $false
$encrypted = $false
foreach ($candidate in $candidates) {
  $progId = $candidate[0]
  if (-not (Test-Path -LiteralPath "Registry::HKEY_CLASSES_ROOT\$progId\CLSID")) { continue }
  $found = $true
  $wasRunning = $null -ne (Get-Process -Name $candidate[1] -ErrorAction SilentlyContinue)
  $app = $null
  $file = $null
  try {
    $app = New-Object -ComObject $progId
    if ($slides) {
      $file = $app.Presentations.Open($in, -1, 0, 0)
      $file.SaveAs($out, 32)
    } else {
      try { $app.Visible = $false } catch {}
      try { $app.DisplayAlerts = 0 } catch {}
      $file = $app.Documents.Open($in, $false, $true, $false, 'rainy-no-password')
      try { $file.ExportAsFixedFormat($out, 17) } catch { $file.SaveAs([ref]$out, [ref]17) }
    }
    if (Test-Path -LiteralPath $out) { Write-Output ('OK ' + $candidate[2]); exit 0 }
    $unwritten += $candidate[2]
  } catch {
    if ($_.Exception.HResult -eq -2146822880) { $encrypted = $true }
    $failures += $candidate[2] + ': ' + $_.Exception.Message
  } finally {
    if ($null -ne $file) { try { if ($slides) { $file.Close() } else { $file.Close(0) } } catch {} }
    if ($null -ne $app -and ($progId -eq 'Word.Application' -or -not $wasRunning)) { try { $app.Quit() } catch {} }
    foreach ($object in @($file, $app)) { if ($null -ne $object) { try { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($object) } catch {} } }
  }
  if ($encrypted) { break }
}
if (-not $found) { Write-Output 'MISSING'; exit 3 }
if ($encrypted) { Write-Output 'PASSWORD'; exit 5 }
if ($failures.Count -eq 0) { Write-Output ('NOPDF ' + ($unwritten -join ', ')); exit 4 }
Write-Output ('FAILED ' + ($failures -join ' | '))
exit 4
"#;

fn powershell() -> PathBuf {
    let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    root.join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe")
}

/// Run the script; its last output line, `TIMEOUT` when Office did not finish in time, `FAILED` and the reason when
/// PowerShell could not start or wrote nothing.
fn run(input: &std::path::Path, output: &std::path::Path, kind: &str) -> String {
    let script: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(script);
    let mut command = Command::new(powershell());
    command
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encoded])
        .env("RAINY_OFFICE_IN", input)
        .env("RAINY_OFFICE_OUT", output)
        .env("RAINY_OFFICE_KIND", kind)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return format!("FAILED {error}"),
    };
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + TIMEOUT;
    let finished = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if Instant::now() >= deadline => break false,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => break true,
        }
    };
    if !finished {
        let _ = child.kill();
        let _ = child.wait();
        return "TIMEOUT".to_owned();
    }
    let text = reader.join().unwrap_or_default();
    text.lines().rev().map(str::trim).find(|line| !line.is_empty()).map_or_else(|| "FAILED".to_owned(), str::to_owned)
}

/// Create a private `%TEMP%\rainy-office-*` staging folder for one conversion.
pub fn create_staging_dir() -> Result<PathBuf, Failure> {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_nanos());
    let seq = STAGING_SEQ.fetch_add(1, Ordering::Relaxed);
    let folder = std::env::temp_dir().join(format!("rainy-office-{}-{stamp}-{seq}", std::process::id()));
    std::fs::create_dir(&folder).map_err(|error| Failure::new("failed", error.to_string()))?;
    Ok(folder)
}

/// Convert an input file on disk to a PDF inside `staging_folder` (or a new `%TEMP%\rainy-office-*` folder).
/// Avoids loading the entire Office document or output PDF into RAM, and guarantees `staging_folder` is cleaned
/// up on every error path.
pub fn convert_file_to_pdf(input_path: &std::path::Path, extension: &str, staging_folder: Option<PathBuf>) -> Result<ConvertedFile, Failure> {
    let mut guard = StagingGuard(staging_folder);
    let Some(kind) = kind(extension) else { return Err(Failure::new("unsupported", format!(".{extension}"))) };
    let metadata = std::fs::metadata(input_path).map_err(|error| Failure::new("failed", error.to_string()))?;
    if metadata.len() > MAX_BYTES as u64 {
        return Err(Failure::new("too-large", format!("{} MB", metadata.len() / (1024 * 1024))));
    }
    let mut head = [0u8; 8];
    if let Ok(mut file) = std::fs::File::open(input_path) {
        let n = file.read(&mut head).unwrap_or(0);
        if encrypted_open_xml(&head[..n], extension) {
            return Err(Failure::new("password", ""));
        }
    }
    if guard.0.is_none() {
        guard.0 = Some(create_staging_dir()?);
    }
    let folder = guard.0.as_ref().expect("staging folder initialized").clone();
    let staged_input = if input_path.parent() == Some(folder.as_path()) {
        input_path.to_path_buf()
    } else {
        let copy = folder.join(format!("document.{extension}"));
        std::fs::copy(input_path, &copy).map_err(|error| Failure::new("failed", error.to_string()))?;
        copy
    };
    let output = folder.join("document.pdf");
    let answer = run(&staged_input, &output, kind);
    // Remove staged copy of the source document now that Office has closed it; keep only the PDF.
    let _ = std::fs::remove_file(&staged_input);
    match answer.as_str() {
        "MISSING" => Err(Failure::new("missing", "")),
        "PASSWORD" => Err(Failure::new("password", "")),
        "TIMEOUT" => Err(Failure::new("timeout", "")),
        _ => {
            if let Some(application) = answer.strip_prefix("OK ") {
                let mut magic = [0u8; 5];
                let valid = std::fs::File::open(&output).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == b"%PDF-";
                if valid {
                    let folder = guard.disarm().expect("staging folder owned");
                    return Ok(ConvertedFile { folder, pdf_path: output, application: application.to_owned() });
                }
                return Err(Failure::new("no-output", application.trim()));
            }
            if let Some(applications) = answer.strip_prefix("NOPDF") {
                return Err(Failure::new("no-output", applications.trim()));
            }
            Err(Failure::new("failed", answer.strip_prefix("FAILED").unwrap_or(&answer).trim()))
        }
    }
}

/// Convert one document; the input goes to a private temporary folder that is removed afterwards.
#[cfg(test)]
pub fn convert(bytes: &[u8], extension: &str) -> Result<Converted, Failure> {
    let Some(kind) = kind(extension) else { return Err(Failure::new("unsupported", format!(".{extension}"))) };
    if bytes.len() > MAX_BYTES {
        return Err(Failure::new("too-large", format!("{} MB", bytes.len() / (1024 * 1024))));
    }
    // Office would ask for the password in a window nobody sees, or in the user's own PowerPoint.
    if encrypted_open_xml(bytes, extension) {
        return Err(Failure::new("password", ""));
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_nanos());
    let folder = std::env::temp_dir().join(format!("rainy-office-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&folder).map_err(|error| Failure::new("failed", error.to_string()))?;
    let input = folder.join(format!("document.{extension}"));
    let output = folder.join("document.pdf");
    let result = std::fs::write(&input, bytes).map_err(|error| Failure::new("failed", error.to_string())).and_then(|()| failure_of(&run(&input, &output, kind), &output));
    // Office may still hold the files for a moment after it quits.
    for _ in 0..5 {
        if std::fs::remove_dir_all(&folder).is_ok() || !folder.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    result
}

/// The PDF, or why there is none, from the script's last output line.
#[cfg(test)]
fn failure_of(answer: &str, output: &std::path::Path) -> Result<Converted, Failure> {
    match answer {
        "MISSING" => Err(Failure::new("missing", "")),
        "PASSWORD" => Err(Failure::new("password", "")),
        "TIMEOUT" => Err(Failure::new("timeout", "")),
        _ => {
            if let Some(application) = answer.strip_prefix("OK ") {
                return std::fs::read(output).map(|pdf| Converted { pdf, application: application.to_owned() }).map_err(|error| Failure::new("no-output", error.to_string()));
            }
            if let Some(applications) = answer.strip_prefix("NOPDF") {
                return Err(Failure::new("no-output", applications.trim()));
            }
            Err(Failure::new("failed", answer.strip_prefix("FAILED").unwrap_or(answer).trim()))
        }
    }
}

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;

    use super::*;

    #[test]
    fn kinds_and_passwords() {
        assert_eq!(kind("pptx"), Some("slides"));
        assert_eq!(kind("doc"), Some("word"));
        assert_eq!(kind("xlsx"), None);
        let compound = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1, 0];
        assert!(encrypted_open_xml(&compound, "pptx"));
        assert!(!encrypted_open_xml(&compound, "ppt"));
        assert!(!encrypted_open_xml(b"PK\x03\x04", "pptx"));
        assert!(convert(&compound, "pptx").unwrap_err().encrypted);
        assert_eq!(convert(b"x", "txt").unwrap_err().code, "unsupported");
    }

    #[test]
    fn script_answers_become_codes() {
        let nowhere = std::path::Path::new("missing-rainy-test.pdf");
        let code = |answer: &str| failure_of(answer, nowhere).unwrap_err();
        assert!(code("MISSING").missing);
        assert!(code("PASSWORD").encrypted);
        assert_eq!(code("TIMEOUT").code, "timeout");
        assert_eq!((code("NOPDF Microsoft Word").code, code("NOPDF Microsoft Word").error.as_str()), ("no-output", "Microsoft Word"));
        assert_eq!((code("FAILED").code, code("FAILED").error.as_str()), ("failed", ""));
        assert_eq!(code("FAILED WPS Office: 0x800A").error, "WPS Office: 0x800A");
        assert_eq!(code("OK Microsoft Word").code, "no-output");
    }

    #[test]
    fn staging_dir_is_unique_and_cleaned_up_on_early_validation_failure() {
        let dir_a = create_staging_dir().expect("staging dir a");
        let dir_b = create_staging_dir().expect("staging dir b");
        assert_ne!(dir_a, dir_b);
        ConvertedFile::cleanup(dir_b);

        // Unsupported extension must still clean up caller-provided staging folder.
        let dummy = dir_a.join("document.txt");
        std::fs::write(&dummy, b"hello").unwrap();
        let err = convert_file_to_pdf(&dummy, "txt", Some(dir_a.clone())).unwrap_err();
        assert_eq!(err.code, "unsupported");
        assert!(!dir_a.exists(), "staging folder leaked on unsupported extension");

        // Encrypted OpenXML early return must also clean up caller-provided staging folder.
        let dir_c = create_staging_dir().expect("staging dir c");
        let encrypted = dir_c.join("document.pptx");
        std::fs::write(&encrypted, [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1, 0]).unwrap();
        let err = convert_file_to_pdf(&encrypted, "pptx", Some(dir_c.clone())).unwrap_err();
        assert!(err.encrypted);
        assert!(!dir_c.exists(), "staging folder leaked on encrypted file");
    }
}
