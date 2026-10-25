//! Slides and legacy Word documents as PDF, made by the Microsoft Office or WPS Office installed on this computer
//! (`src/main/office-convert.ts`). The viewer page asks for it when it cannot draw a format itself.

use std::collections::VecDeque;
use std::hash::{Hash as _, Hasher as _};
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;

/// Largest file handed to Office.
const MAX_BYTES: usize = 200 * 1024 * 1024;
/// Time allowed for starting Office and exporting.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Conversions remembered, so switching back to a tab does not start Office again.
const KEEP: usize = 6;

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
#[derive(Debug)]
pub struct Converted {
    pub pdf: Vec<u8>,
    pub application: String,
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

/// Convert one document; the input goes to a private temporary folder that is removed afterwards.
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

type Remembered = VecDeque<((String, usize, u64), Arc<Converted>)>;

/// Convert, or return a recent conversion of the same file; a failure is not remembered.
pub fn to_pdf(bytes: &[u8], extension: &str) -> Result<Arc<Converted>, Failure> {
    static RECENT: Mutex<Remembered> = Mutex::new(VecDeque::new());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let key = (extension.to_owned(), bytes.len(), hasher.finish());
    {
        let mut recent = RECENT.lock().unwrap();
        if let Some(index) = recent.iter().position(|(known, _)| *known == key) {
            let entry = recent.remove(index).expect("index found above");
            let converted = entry.1.clone();
            recent.push_back(entry);
            return Ok(converted);
        }
    }
    let converted = Arc::new(convert(bytes, extension)?);
    let mut recent = RECENT.lock().unwrap();
    recent.push_back((key, converted.clone()));
    while recent.len() > KEEP {
        recent.pop_front();
    }
    Ok(converted)
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
}
