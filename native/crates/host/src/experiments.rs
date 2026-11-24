//! Detached, session-independent background experiments (`experiments.*`) that outlive Host disconnects and restarts.
//! Persisted atomically in `$RAINY_HOME/experiments.v1.json` and strictly isolated from `Owner`/`Jobs`.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use rainy_protocol::experiments::{ExperimentListResponse, ExperimentRecord, ExperimentStartParams, ExperimentStatus, ExperimentTailResult};
use serde::{Deserialize, Serialize};

use crate::error::{HostError, HostResult};
use crate::locale::l;
use crate::process::scrubbed_env;
use crate::store::{create_private_dir, read_json, write_json};
use crate::text::now_ms;

pub const DEFAULT_TAIL_BYTES: u64 = 64 * 1024;
pub const MAX_TAIL_BYTES: u64 = 512 * 1024;
const MAX_EXPERIMENTS: usize = 256;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExperimentsFile {
    #[serde(default = "default_version")]
    version: u8,
    #[serde(default)]
    experiments: Vec<ExperimentRecord>,
}

fn default_version() -> u8 {
    1
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExitMarker {
    exit_code: Option<i32>,
    ended_at: Option<u64>,
}

/// Parsed fields from Linux `/proc/<pid>/stat` needed to prevent PID reuse and zombie misreads.
#[cfg(any(unix, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStatInfo {
    pub state: char,
    pub pgrp: u32,
    pub start_ticks: u64,
}

/// Result of one incremental tail read, shared between `experiments.tail` and `ide` `files.tail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailChunk {
    pub text: String,
    pub offset: u64,
    pub next_offset: u64,
    pub total_bytes: u64,
    pub truncated: bool,
    pub reset: bool,
}

/// Parse extra per-host environment variables passed by the carrier via `RAINY_SSH_EXTRA_ENV` (JSON object).
pub fn ssh_extra_env() -> BTreeMap<String, String> {
    let Ok(raw) = std::env::var("RAINY_SSH_EXTRA_ENV") else { return BTreeMap::new() };
    parse_ssh_extra_env(&raw)
}

pub fn parse_ssh_extra_env(raw: &str) -> BTreeMap<String, String> {
    let Ok(map) = serde_json::from_str::<BTreeMap<String, String>>(raw) else { return BTreeMap::new() };
    map.into_iter().filter(|(k, v)| valid_env_key(k) && !v.contains('\0')).collect()
}

fn valid_env_key(key: &str) -> bool {
    !key.is_empty() && !key.contains('=') && !key.contains('\0')
}

/// Parse `/proc/<pid>/stat`, safely handling `comm` names that contain spaces or parentheses.
#[cfg(any(unix, test))]
pub fn parse_proc_stat(text: &str) -> Option<ProcStatInfo> {
    let rparen = text.rfind(')')?;
    let after = text.get(rparen + 1..)?.trim_start();
    let fields: Vec<&str> = after.split_ascii_whitespace().collect();
    // After `(comm)`: fields[0] = state (#3), fields[2] = pgrp (#5), fields[19] = starttime (#22).
    if fields.len() <= 19 {
        return None;
    }
    let state = fields[0].chars().next()?;
    let pgrp = fields[2].parse::<u32>().ok()?;
    let start_ticks = fields[19].parse::<u64>().ok()?;
    Some(ProcStatInfo { state, pgrp, start_ticks })
}

/// Verify that a live Unix process still matches the exact `setsid()` experiment launched by this user.
#[cfg(any(unix, test))]
pub fn verify_unix_process_identity(record: &ExperimentRecord, stat: &ProcStatInfo, proc_uid: u32, caller_euid: u32) -> bool {
    if matches!(stat.state, 'Z' | 'X' | 'x') {
        return false;
    }
    let Some(pid) = record.pid.filter(|&p| p > 0 && p <= i32::MAX as u32) else { return false };
    // `setsid()` makes the wrapper both session leader and process group leader (`pgrp == pid`).
    if stat.pgrp != pid {
        return false;
    }
    let Some(expected_ticks) = record.proc_start_ticks else { return false };
    let Some(expected_uid) = record.uid else { return false };
    stat.start_ticks == expected_ticks && proc_uid == expected_uid && proc_uid == caller_euid
}

/// Read an incremental UTF-8-safe tail slice of `path` starting at `offset` (or the last `max_bytes` when `offset` is `None`).
pub fn read_tail_chunk(path: &Path, offset: Option<u64>, max_bytes: Option<u64>) -> HostResult<TailChunk> {
    let limit = max_bytes.unwrap_or(DEFAULT_TAIL_BYTES).clamp(1024, MAX_TAIL_BYTES);
    let meta = fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            HostError::new("not-found", l("日志文件不存在。", "The log file does not exist."))
        } else {
            error.into()
        }
    })?;
    if !meta.is_file() {
        return Err(HostError::new("not-file", l("目标路径不是普通文件。", "The target path is not a regular file.")));
    }
    let total_bytes = meta.len();
    let (start, truncated, reset) = match offset {
        None => {
            let start = total_bytes.saturating_sub(limit);
            (start, start > 0, false)
        }
        Some(off) if off > total_bytes => {
            let start = total_bytes.saturating_sub(limit);
            (start, start > 0, true)
        }
        Some(off) if total_bytes.saturating_sub(off) > limit => {
            let start = total_bytes - limit;
            (start, true, false)
        }
        Some(off) => (off, false, false),
    };

    let want = total_bytes.saturating_sub(start).min(limit) as usize;
    let mut buf = vec![0u8; want];
    let read_len = if want > 0 {
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(start))?;
        let mut filled = 0usize;
        while filled < want {
            match file.read(&mut buf[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        filled
    } else {
        0
    };
    buf.truncate(read_len);

    // If starting mid-file, skip up to 3 leading UTF-8 continuation bytes so we land on a character boundary.
    let mut trim_start = 0usize;
    if start > 0 {
        while trim_start < buf.len().min(3) && (buf[trim_start] & 0xC0) == 0x80 {
            trim_start += 1;
        }
    }
    // Hold back an incomplete trailing multi-byte UTF-8 sequence so the next poll completes it cleanly.
    let trim_end = utf8_complete_end(&buf, trim_start);
    let slice = &buf[trim_start..trim_end];
    let text = String::from_utf8_lossy(slice).into_owned();

    Ok(TailChunk {
        text,
        offset: start.saturating_add(trim_start as u64),
        next_offset: start.saturating_add(trim_end as u64),
        total_bytes,
        truncated,
        reset,
    })
}

/// Trim a trailing incomplete UTF-8 multi-byte sequence (1..=3 bytes) without stalling on invalid isolated bytes.
fn utf8_complete_end(buf: &[u8], start: usize) -> usize {
    let slice = &buf[start..];
    if slice.is_empty() {
        return start;
    }
    let check_from = slice.len().saturating_sub(3);
    // Find the last non-continuation byte within the trailing 3 bytes.
    let mut lead = None;
    for idx in (check_from..slice.len()).rev() {
        if (slice[idx] & 0xC0) != 0x80 {
            lead = Some(idx);
            break;
        }
    }
    let Some(idx) = lead else { return buf.len() };
    let byte = slice[idx];
    let expected = if byte < 0x80 {
        1
    } else if (0xC2..=0xDF).contains(&byte) {
        2
    } else if (0xE0..=0xEF).contains(&byte) {
        3
    } else if (0xF0..=0xF4).contains(&byte) {
        4
    } else {
        return buf.len();
    };
    let tail = &slice[idx..];
    if tail.len() < expected && tail[1..].iter().all(|b| (b & 0xC0) == 0x80) {
        start + idx
    } else {
        buf.len()
    }
}

/// Host service managing detached background experiments.
pub struct Experiments {
    registry_path: PathBuf,
    data_dir: PathBuf,
    records: Mutex<Vec<ExperimentRecord>>,
}

impl Experiments {
    pub fn open(home: &Path) -> Self {
        let registry_path = home.join("experiments.v1.json");
        let data_dir = home.join("experiments");
        let loaded = read_json::<ExperimentsFile>(&registry_path)
            .ok()
            .flatten()
            .map(|file| file.experiments)
            .unwrap_or_default();
        Self { registry_path, data_dir, records: Mutex::new(loaded) }
    }

    fn save_locked(&self, records: &[ExperimentRecord]) -> HostResult<()> {
        let file = ExperimentsFile { version: 1, experiments: records.to_vec() };
        write_json(&self.registry_path, &file).map_err(Into::into)
    }

    fn exit_marker_path(&self, id: &str) -> PathBuf {
        self.data_dir.join(format!("{id}.exit"))
    }

    /// List all tracked experiments after refreshing live process states.
    pub fn list(&self) -> HostResult<ExperimentListResponse> {
        let mut records = self.records.lock().unwrap();
        let mut changed = false;
        for record in records.iter_mut() {
            if self.refresh_record(record) {
                changed = true;
            }
        }
        if changed {
            let _ = self.save_locked(&records);
        }
        let mut out = records.clone();
        out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(ExperimentListResponse { experiments: out })
    }

    /// Start a detached experiment that survives Host disconnects and shutdown.
    pub fn start(&self, params: ExperimentStartParams) -> HostResult<ExperimentRecord> {
        let command = params.command.trim().to_owned();
        if command.is_empty() || command.contains('\0') {
            return Err(HostError::invalid(l("实验命令不能为空。", "The experiment command cannot be empty.")));
        }
        let cwd_raw = params.cwd.trim();
        if cwd_raw.is_empty() || cwd_raw.contains('\0') {
            return Err(HostError::invalid(l("实验工作目录不能为空。", "The experiment working directory cannot be empty.")));
        }
        let cwd = PathBuf::from(cwd_raw);
        if !cwd.is_dir() {
            return Err(HostError::new("not-found", l("实验工作目录不存在。", "The experiment working directory does not exist.")));
        }

        create_private_dir(&self.data_dir)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let name = params
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| derive_name(&command));

        let log_path = match params.log_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => {
                if raw.contains('\0') {
                    return Err(HostError::invalid(l("日志路径无效。", "Invalid log path.")));
                }
                let path = PathBuf::from(raw);
                if path.is_absolute() { path } else { cwd.join(path) }
            }
            None => self.data_dir.join(format!("{id}.log")),
        };
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let log_file = OpenOptions::new().create(true).append(true).open(&log_path)?;
        let exit_path = self.exit_marker_path(&id);

        let mut env = scrubbed_env();
        for (k, v) in ssh_extra_env() {
            insert_env(&mut env, &k, &v);
        }
        for (k, v) in &params.env {
            if !valid_env_key(k) || v.contains('\0') {
                return Err(HostError::invalid(l("环境变量名称或值无效。", "Invalid environment variable name or value.")));
            }
            insert_env(&mut env, k, v);
        }
        let cuda_visible_devices = params.cuda_visible_devices.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        if let Some(devices) = &cuda_visible_devices {
            if devices.contains('\0') {
                return Err(HostError::invalid(l("CUDA_VISIBLE_DEVICES 值无效。", "Invalid CUDA_VISIBLE_DEVICES value.")));
            }
            insert_env(&mut env, "CUDA_VISIBLE_DEVICES", devices);
        }

        let started_at = now_ms();
        let launched = spawn_detached(&command, &cwd, &env, log_file, &exit_path)?;

        let record = ExperimentRecord {
            id,
            name,
            command,
            cwd: cwd.display().to_string(),
            log_path: log_path.display().to_string(),
            status: ExperimentStatus::Running,
            pid: Some(launched.pid),
            pgid: launched.pgid,
            proc_start_ticks: launched.proc_start_ticks,
            uid: launched.uid,
            cuda_visible_devices,
            started_at,
            ended_at: None,
            exit_code: None,
        };

        let mut records = self.records.lock().unwrap();
        records.push(record.clone());
        if records.len() > MAX_EXPERIMENTS {
            // Trim oldest finished records first so active experiments are never evicted.
            if let Some(idx) = records.iter().position(|r| r.status != ExperimentStatus::Running) {
                records.remove(idx);
            } else {
                records.remove(0);
            }
        }
        self.save_locked(&records)?;
        Ok(record)
    }

    /// Safely stop a running experiment after strict PID + start-time (+ UID/PGID on Unix) verification.
    pub fn stop(&self, id: &str, force: bool) -> HostResult<ExperimentRecord> {
        let mut records = self.records.lock().unwrap();
        let Some(record) = records.iter_mut().find(|r| r.id == id) else {
            return Err(HostError::new("not-found", l("找不到该实验记录。", "Experiment record not found.")));
        };
        let _ = self.refresh_record(record);
        if record.status != ExperimentStatus::Running {
            let result = record.clone();
            let _ = self.save_locked(&records);
            return Ok(result);
        }
        if terminate_verified(record, force)? {
            // Poll briefly to verify the process actually exited; if it traps/ignores SIGTERM keep it Running
            // so the user can escalate to `force: true` (SIGKILL) and `remove()` cannot orphan a live GPU job.
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
            let mut exited = !is_experiment_alive(record);
            while !exited && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(40));
                exited = !is_experiment_alive(record);
            }
            if exited {
                record.status = ExperimentStatus::Stopped;
                record.ended_at = Some(now_ms());
            }
        } else {
            // Process no longer matched recorded identity (already exited or PID reused): never signal it.
            let _ = self.refresh_record(record);
            if record.status == ExperimentStatus::Running {
                record.status = ExperimentStatus::Unknown;
            }
        }
        let result = record.clone();
        self.save_locked(&records)?;
        Ok(result)
    }

    /// Remove a finished (or explicitly non-running) experiment entry from the registry.
    pub fn remove(&self, id: &str) -> HostResult<bool> {
        let mut records = self.records.lock().unwrap();
        let Some(idx) = records.iter().position(|r| r.id == id) else {
            return Err(HostError::new("not-found", l("找不到该实验记录。", "Experiment record not found.")));
        };
        let _ = self.refresh_record(&mut records[idx]);
        if records[idx].status == ExperimentStatus::Running || is_experiment_alive(&records[idx]) {
            records[idx].status = ExperimentStatus::Running;
            let _ = self.save_locked(&records);
            return Err(HostError::new("busy", l("实验仍在运行，请先停止后再移除记录。", "The experiment is still running. Stop it before removing.")));
        }
        let removed = records.remove(idx);
        let _ = fs::remove_file(self.exit_marker_path(&removed.id));
        self.save_locked(&records)?;
        Ok(true)
    }

    /// Incrementally tail the log file of a tracked experiment.
    pub fn tail(&self, id: &str, offset: Option<u64>, max_bytes: Option<u64>) -> HostResult<ExperimentTailResult> {
        let log_path = {
            let records = self.records.lock().unwrap();
            records
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.log_path.clone())
                .ok_or_else(|| HostError::new("not-found", l("找不到该实验记录。", "Experiment record not found.")))?
        };
        let chunk = read_tail_chunk(Path::new(&log_path), offset, max_bytes)?;
        Ok(ExperimentTailResult {
            id: id.to_owned(),
            log_path,
            text: chunk.text,
            offset: chunk.offset,
            next_offset: chunk.next_offset,
            total_bytes: chunk.total_bytes,
            truncated: chunk.truncated,
            reset: chunk.reset,
        })
    }

    /// Re-check one experiment's live status against process identity and `.exit` markers. Returns `true` if modified.
    fn refresh_record(&self, record: &mut ExperimentRecord) -> bool {
        // Check OS process liveness before honoring `.exit`: the wrapper writes `.exit` just before `exit $rc` tears
        // down the process, so reporting `Exited` while the process is still alive would race with `remove()`.
        if is_experiment_alive(record) {
            if record.status != ExperimentStatus::Running {
                record.status = ExperimentStatus::Running;
                record.ended_at = None;
                return true;
            }
            return false;
        }
        let marker_path = self.exit_marker_path(&record.id);
        if let Ok(Some(marker)) = read_json::<ExitMarker>(&marker_path) {
            let mut changed = false;
            if record.status == ExperimentStatus::Running || record.status == ExperimentStatus::Unknown {
                record.status = ExperimentStatus::Exited;
                changed = true;
            }
            if record.exit_code.is_none() && marker.exit_code.is_some() {
                record.exit_code = marker.exit_code;
                changed = true;
            }
            if record.ended_at.is_none() {
                record.ended_at = marker.ended_at.filter(|&ts| ts > 0).or_else(|| Some(now_ms()));
                changed = true;
            }
            return changed;
        }
        if record.status != ExperimentStatus::Running {
            return false;
        }
        // No exit marker and process is gone (or PID reused by another process).
        record.status = ExperimentStatus::Unknown;
        if record.ended_at.is_none() {
            record.ended_at = Some(now_ms());
        }
        true
    }
}

fn derive_name(command: &str) -> String {
    let single_line: String = command.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::text::truncate_chars(&single_line, 48).to_owned()
}

fn insert_env(env: &mut BTreeMap<std::ffi::OsString, std::ffi::OsString>, name: &str, value: &str) {
    if cfg!(windows) {
        env.retain(|known, _| !known.to_string_lossy().eq_ignore_ascii_case(name));
    }
    env.insert(name.into(), value.into());
}

struct Launched {
    pid: u32,
    pgid: Option<u32>,
    proc_start_ticks: Option<u64>,
    uid: Option<u32>,
}

#[cfg(unix)]
fn spawn_detached(
    command: &str,
    cwd: &Path,
    env: &BTreeMap<std::ffi::OsString, std::ffi::OsString>,
    log_file: File,
    exit_path: &Path,
) -> HostResult<Launched> {
    use std::os::unix::process::CommandExt as _;

    let shell = if Path::new("/bin/bash").is_file() { "/bin/bash" } else { "/bin/sh" };
    // Run the user command inside a new session (`setsid`) and write exit status atomically on completion.
    let wrapper = r#"_rainy_cmd="$1"
_rainy_exit="$2"
if [ -x /bin/bash ]; then
  /bin/bash -lc "$_rainy_cmd"
else
  /bin/sh -c "$_rainy_cmd"
fi
_rc=$?
_sec=$(date +%s 2>/dev/null || echo 0)
case "$_sec" in
  ''|*[!0-9]*) _ts=0 ;;
  *) _ts="${_sec}000" ;;
esac
printf '{"exitCode":%d,"endedAt":%s}\n' "$_rc" "$_ts" > "${_rainy_exit}.tmp" && mv -f "${_rainy_exit}.tmp" "$_rainy_exit"
exit $_rc"#;

    let stderr_file = log_file.try_clone()?;
    let mut cmd = std::process::Command::new(shell);
    cmd.args(["-c", wrapper, "rainy-experiment", command, &exit_path.display().to_string()])
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr_file));
    // SAFETY: `setsid` is async-signal-safe and detaches the child into its own session and process group.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|error| HostError::other(l(&format!("无法启动后台实验：{error}"), &format!("Failed to start detached experiment: {error}"))))?;
    let pid = child.id();
    let proc_start_ticks = read_unix_proc_stat(pid).map(|stat| stat.start_ticks);
    // SAFETY: `geteuid` has no preconditions and always succeeds.
    let uid = Some(unsafe { libc::geteuid() });

    // Reap the child in the background if it finishes while this Host stays alive; never kills on Host exit.
    std::thread::Builder::new()
        .name(format!("exp-reap-{pid}"))
        .spawn(move || {
            let _ = child.wait();
        })
        .ok();

    Ok(Launched { pid, pgid: Some(pid), proc_start_ticks, uid })
}

/// The PowerShell that runs an experiment on Windows. The command and the exit-marker path come in through
/// environment variables, and the wrapper itself has no double quotes, so Windows argument quoting cannot change it.
/// It goes in with `-Command`, not `-EncodedCommand`: Windows PowerShell writes the error and progress streams of an
/// encoded command as CLIXML, which made a failing run's log unreadable (and every log start with `#< CLIXML`).
#[cfg(any(windows, test))]
const WINDOWS_WRAPPER: &str = concat!(
    "$ProgressPreference = 'SilentlyContinue'; ",
    "$c = $env:_RAINY_EXP_CMD; $e = $env:_RAINY_EXP_EXIT; ",
    "Remove-Item Env:_RAINY_EXP_CMD, Env:_RAINY_EXP_EXIT -ErrorAction SilentlyContinue; ",
    "$rc = 0; ",
    "try { Invoke-Expression $c; if ($null -ne $LASTEXITCODE) { $rc = [int]$LASTEXITCODE } } catch { Write-Error $_; $rc = 1 }; ",
    "$ts = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds(); ",
    "$json = (@{ exitCode = $rc; endedAt = $ts } | ConvertTo-Json -Compress); ",
    "$tmp = $e + '.tmp'; ",
    "[System.IO.File]::WriteAllText($tmp, $json, (New-Object System.Text.UTF8Encoding($false))); ",
    "Move-Item -Force $tmp $e; ",
    "exit $rc"
);

#[cfg(windows)]
fn spawn_detached(
    command: &str,
    cwd: &Path,
    env: &BTreeMap<std::ffi::OsString, std::ffi::OsString>,
    log_file: File,
    exit_path: &Path,
) -> HostResult<Launched> {
    use std::os::windows::process::CommandExt as _;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    let (pwsh, _) = crate::process::resolve_powershell(None, None);
    let stderr_file = log_file.try_clone()?;
    let mut cmd = std::process::Command::new(&pwsh);
    cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", WINDOWS_WRAPPER])
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .env("_RAINY_EXP_CMD", command)
        .env("_RAINY_EXP_EXIT", exit_path)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr_file))
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);

    // Deliberately do NOT attach to Host's ProcessJob so the experiment survives Host exit.
    let mut child = cmd.spawn().map_err(|error| HostError::other(l(&format!("无法启动后台实验：{error}"), &format!("Failed to start detached experiment: {error}"))))?;
    let pid = child.id();
    let proc_start_ticks = read_windows_process_creation_time(pid);

    std::thread::Builder::new()
        .name(format!("exp-reap-{pid}"))
        .spawn(move || {
            let _ = child.wait();
        })
        .ok();

    Ok(Launched { pid, pgid: Some(pid), proc_start_ticks, uid: None })
}

#[cfg(windows)]
fn read_windows_process_creation_time(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    const STILL_ACTIVE: u32 = 259;

    if pid == 0 {
        return None;
    }
    // SAFETY: queries process liveness and creation FILETIME with minimal read-only permission, then closes the handle.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut code: u32 = 0;
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let active = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE;
        let has_time = active && GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) != 0;
        CloseHandle(handle);
        has_time.then(|| (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }
}

#[cfg(unix)]
fn read_unix_proc_stat(pid: u32) -> Option<ProcStatInfo> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_proc_stat(&text)
}

#[cfg(unix)]
fn read_unix_proc_uid(pid: u32) -> Option<u32> {
    let text = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    crate::sysinfo::parse_proc_status_uid(&text)
}

#[cfg(unix)]
fn is_experiment_alive(record: &ExperimentRecord) -> bool {
    let Some(pid) = record.pid else { return false };
    let Some(stat) = read_unix_proc_stat(pid) else { return false };
    let Some(proc_uid) = read_unix_proc_uid(pid) else { return false };
    // SAFETY: `geteuid` has no preconditions.
    let caller_euid = unsafe { libc::geteuid() };
    verify_unix_process_identity(record, &stat, proc_uid, caller_euid)
}

#[cfg(windows)]
fn is_experiment_alive(record: &ExperimentRecord) -> bool {
    let Some(pid) = record.pid.filter(|&p| p > 0) else { return false };
    let Some(expected_creation) = record.proc_start_ticks else { return false };
    read_windows_process_creation_time(pid).is_some_and(|actual| actual == expected_creation)
}

#[cfg(unix)]
fn terminate_verified(record: &ExperimentRecord, force: bool) -> HostResult<bool> {
    let Some(pid) = record.pid.filter(|&p| p > 0 && p <= i32::MAX as u32) else { return Ok(false) };
    let Some(stat) = read_unix_proc_stat(pid) else { return Ok(false) };
    let Some(proc_uid) = read_unix_proc_uid(pid) else { return Ok(false) };
    // SAFETY: `geteuid` has no preconditions.
    let caller_euid = unsafe { libc::geteuid() };
    if !verify_unix_process_identity(record, &stat, proc_uid, caller_euid) {
        return Ok(false);
    }
    let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
    // SAFETY: verified `stat.pgrp == pid`, `pid <= i32::MAX`, matching starttime and caller UID; never trusts persisted `pgid`.
    unsafe {
        if libc::kill(-(pid as i32), sig) != 0 {
            libc::kill(pid as i32, sig);
        }
    }
    Ok(true)
}

#[cfg(windows)]
fn terminate_verified(record: &ExperimentRecord, force: bool) -> HostResult<bool> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = force;

    if !is_experiment_alive(record) {
        return Ok(false);
    }
    let Some(pid) = record.pid else { return Ok(false) };
    // Detached console / PowerShell trees require `/F` for deterministic teardown on Windows.
    let args = ["/PID", &pid.to_string(), "/T", "/F"];
    let _ = std::process::Command::new("taskkill.exe")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_stat_including_spaces_and_parens_in_comm() {
        let sample = "4321 (python (train) v2) S 1 4321 4321 0 -1 4194304 100 0 0 0 12 3 0 0 20 0 4 0 987654321 123456 500\n";
        let stat = parse_proc_stat(sample).unwrap();
        assert_eq!(stat, ProcStatInfo { state: 'S', pgrp: 4321, start_ticks: 987654321 });

        let zombie = "4321 (bash) Z 1 4321 4321 0 -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 987654321 0 0\n";
        let zstat = parse_proc_stat(zombie).unwrap();
        assert_eq!(zstat.state, 'Z');
    }

    #[test]
    fn verifies_unix_process_identity_against_pid_reuse_and_other_users() {
        let record = ExperimentRecord {
            id: "e1".into(),
            pid: Some(4321),
            pgid: Some(4321),
            proc_start_ticks: Some(555000),
            uid: Some(1000),
            ..Default::default()
        };
        let matching = ProcStatInfo { state: 'S', pgrp: 4321, start_ticks: 555000 };
        assert!(verify_unix_process_identity(&record, &matching, 1000, 1000));

        // Different starttime -> PID was reused by a new process.
        let reused = ProcStatInfo { state: 'S', pgrp: 4321, start_ticks: 999999 };
        assert!(!verify_unix_process_identity(&record, &reused, 1000, 1000));

        // Different UID -> another user's process in shared container.
        assert!(!verify_unix_process_identity(&record, &matching, 1001, 1000));
        assert!(!verify_unix_process_identity(&record, &matching, 1000, 0));

        // Zombie process -> already dead.
        let zombie = ProcStatInfo { state: 'Z', pgrp: 4321, start_ticks: 555000 };
        assert!(!verify_unix_process_identity(&record, &zombie, 1000, 1000));

        // Non-group-leader process -> never signal an unrelated process group.
        let non_leader = ProcStatInfo { state: 'S', pgrp: 100, start_ticks: 555000 };
        assert!(!verify_unix_process_identity(&record, &non_leader, 1000, 1000));

        // PID exceeding i32::MAX -> rejected before casting to pid_t.
        let mut huge_pid = record.clone();
        huge_pid.pid = Some((i32::MAX as u32) + 1);
        assert!(!verify_unix_process_identity(&huge_pid, &matching, 1000, 1000));
    }

    #[test]
    fn tails_incrementally_handles_rotation_and_utf8_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("train.log");
        // Write >2 KiB of Chinese + ASCII text to exercise mid-file UTF-8 boundary skipping.
        let line = "训练步骤 step=001 loss=0.1234\n";
        let initial: String = std::iter::repeat_n(line, 100).collect();
        fs::write(&log, &initial).unwrap();

        let first = read_tail_chunk(&log, None, Some(1024)).unwrap();
        assert!(first.truncated);
        assert!(!first.reset);
        assert_eq!(first.next_offset, initial.len() as u64);
        assert!(!first.text.contains('\u{FFFD}'), "UTF-8 boundary must stay clean: {:?}", &first.text[..first.text.len().min(32)]);

        // Append more output and poll from next_offset.
        use std::io::Write as _;
        let mut f = OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all("新日志 epoch=2\n".as_bytes()).unwrap();
        drop(f);

        let second = read_tail_chunk(&log, Some(first.next_offset), Some(1024)).unwrap();
        assert!(!second.truncated);
        assert!(!second.reset);
        assert_eq!(second.text, "新日志 epoch=2\n");

        // Truncate/rotate log file: offset > total_bytes triggers reset.
        fs::write(&log, "rotated\n").unwrap();
        let rotated = read_tail_chunk(&log, Some(second.next_offset), Some(1024)).unwrap();
        assert!(rotated.reset);
        assert_eq!(rotated.text, "rotated\n");
    }

    #[test]
    fn holds_back_incomplete_trailing_utf8_character() {
        // "中" is [0xE4, 0xB8, 0xAD]; write only first two bytes at the end.
        let buf = [b'a', b'b', 0xE4, 0xB8];
        assert_eq!(utf8_complete_end(&buf, 0), 2);
        let full = [b'a', b'b', 0xE4, 0xB8, 0xAD];
        assert_eq!(utf8_complete_end(&full, 0), 5);
    }

    #[test]
    fn parses_ssh_extra_env_and_filters_invalid_keys() {
        let parsed = parse_ssh_extra_env(r#"{"HF_ENDPOINT":"https://hf-mirror.com","BAD=KEY":"1","":"empty"}"#);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed.get("HF_ENDPOINT").map(String::as_str), Some("https://hf-mirror.com"));
        assert!(parse_ssh_extra_env("not json").is_empty());
    }

    #[test]
    fn runs_detached_experiment_lifecycle_and_safe_stop() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let work = dir.path().join("work");
        fs::create_dir_all(&work).unwrap();
        let mgr = Experiments::open(&home);

        // Start a quick experiment that prints (an error too) and exits cleanly.
        assert!(!WINDOWS_WRAPPER.contains('"'), "Windows argument quoting would rewrite the wrapper");
        let cmd = if cfg!(windows) { "Write-Output 'exp-hello-42'; Write-Error 'exp-oops'" } else { "echo exp-hello-42; echo exp-oops >&2" };
        let rec = mgr
            .start(ExperimentStartParams {
                name: Some("quick-check".into()),
                cwd: work.display().to_string(),
                command: cmd.into(),
                cuda_visible_devices: Some("0".into()),
                env: BTreeMap::from([("RAINY_TEST_VAR".into(), "ok".into())]),
                log_path: None,
            })
            .unwrap();
        assert_eq!(rec.name, "quick-check");
        assert!(rec.pid.is_some());

        // Wait up to 8 seconds for the quick experiment to finish and write its exit marker.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut final_rec = rec.clone();
        while std::time::Instant::now() < deadline {
            let list = mgr.list().unwrap();
            if let Some(found) = list.experiments.into_iter().find(|r| r.id == rec.id) {
                final_rec = found;
                if final_rec.status != ExperimentStatus::Running {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let log_preview = fs::read_to_string(&final_rec.log_path).unwrap_or_default();
        assert_eq!(final_rec.status, ExperimentStatus::Exited, "expected exited, got {final_rec:?}; log={log_preview:?}");
        assert_eq!(final_rec.exit_code, Some(0));

        let tail = mgr.tail(&rec.id, Some(0), None).unwrap();
        assert!(tail.text.contains("exp-hello-42"), "unexpected tail: {:?}", tail.text);
        // Errors reach the log as text, not as PowerShell's CLIXML.
        assert!(tail.text.contains("exp-oops") && !tail.text.contains("CLIXML"), "unexpected tail: {:?}", tail.text);
        assert!(mgr.remove(&rec.id).unwrap());

        // Start a longer experiment and verify safe stop + removal.
        let sleep_cmd = if cfg!(windows) { "Start-Sleep -Seconds 30" } else { "sleep 30" };
        let running = mgr
            .start(ExperimentStartParams {
                name: Some("sleep-stop".into()),
                cwd: work.display().to_string(),
                command: sleep_cmd.into(),
                cuda_visible_devices: None,
                env: BTreeMap::new(),
                log_path: None,
            })
            .unwrap();
        assert!(running.proc_start_ticks.is_some(), "creation/start timestamp must be recorded to guard against PID reuse");
        // Tampered start ticks must fail liveness verification so reused PIDs are never killed.
        let mut tampered = running.clone();
        tampered.proc_start_ticks = Some(running.proc_start_ticks.unwrap().wrapping_add(1));
        assert!(!is_experiment_alive(&tampered));
        // Running experiment cannot be removed while alive.
        assert_eq!(mgr.remove(&running.id).unwrap_err().code, "busy");
        let stopped = mgr.stop(&running.id, false).unwrap();
        assert_eq!(stopped.status, ExperimentStatus::Stopped);
        assert!(mgr.remove(&running.id).unwrap());
    }
}
