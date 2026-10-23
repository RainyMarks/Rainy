//! One Rainy per profile and Windows session (3.x `requestSingleInstanceLock` and `second-instance`): the first process
//! for a user-data folder holds a named mutex and listens on a named pipe that only this user can open; a later one
//! checks that this user made the pipe, hands its command line and folder over it, lets the first bring its window to
//! the front, and exits. Profiles given with `--user-data-dir` run side by side.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree, SetLastError, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenSessionId,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;

/// How long a later process keeps trying while the first one is still starting: Explorer starts one process per
/// selected file, all at once.
pub const FORWARD_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a restarted Rainy waits for the one that started it to exit.
pub const RESTART_WAIT: Duration = Duration::from_secs(15);
/// The largest message either side accepts; a command line is at most 32 767 UTF-16 units.
const MESSAGE_LIMIT: usize = 1 << 20;
const RETRY: Duration = Duration::from_millis(100);

/// A command line handed to the first process, with the folder it was started in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
}

impl Launch {
    /// `{"argv": [...], "cwd": ...}`; an argument that is not valid Unicode is sent as its UTF-16 units, so every path
    /// arrives as it was given.
    pub fn encode(&self) -> Vec<u8> {
        let message = Message { argv: self.argv.iter().map(|argument| Text::of(argument)).collect(), cwd: Text::of(self.cwd.as_os_str()) };
        serde_json::to_vec(&message).expect("a launch serializes")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let message: Message = serde_json::from_slice(bytes).context("Reading a launch from another Rainy")?;
        Ok(Self { argv: message.argv.into_iter().map(Text::into_os_string).collect(), cwd: message.cwd.into_os_string().into() })
    }
}

#[derive(Serialize, Deserialize)]
struct Message {
    argv: Vec<Text>,
    cwd: Text,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Text {
    Unicode(String),
    Wide(Vec<u16>),
}

impl Text {
    fn of(value: &std::ffi::OsStr) -> Self {
        match value.to_str() {
            Some(text) => Self::Unicode(text.to_owned()),
            None => Self::Wide(value.encode_wide().collect()),
        }
    }

    fn into_os_string(self) -> OsString {
        match self {
            Self::Unicode(text) => text.into(),
            Self::Wide(units) => OsString::from_wide(&units),
        }
    }
}

/// The first process's answer, once it has the launch: its process id.
#[derive(Serialize, Deserialize)]
struct Reply {
    pid: u32,
}

/// What became of this start.
pub enum Claim {
    /// This process is Rainy for the profile; `Lock::serve` takes the launches later processes hand over.
    First(Lock),
    /// The Rainy already running for the profile, this process id, took the launch.
    Forwarded(u32),
    /// The lock could not be made; this process runs on its own, as Rainy did before it had one.
    Alone,
}

/// The profile's lock, held until the process exits.
pub struct Lock {
    _mutex: OwnedHandle,
    pipe: String,
    sid: String,
}

/// Who runs this process: the user's SID, as `S-1-5-21-…`, and the Windows session.
struct User {
    sid: String,
    session: u32,
}

/// Take the profile's lock, or hand `launch` to the Rainy that holds it. A restarted Rainy passes the process it
/// replaces: it waits for that one to exit and never hands its launch to it.
pub fn claim(user_data: &Path, launch: &Launch, replaces: Option<u32>) -> Result<Claim> {
    if let Some(pid) = replaces
        && !wait_for_exit(pid, RESTART_WAIT)
    {
        log::warn!("Rainy {pid} had not exited after {} s", RESTART_WAIT.as_secs());
    }
    claim_profile(&profile_key(user_data), launch, replaces, FORWARD_TIMEOUT)
}

fn claim_profile(key: &str, launch: &Launch, replaces: Option<u32>, timeout: Duration) -> Result<Claim> {
    let user = match User::current() {
        Ok(user) => user,
        Err(error) => {
            log::warn!("Rainy runs without the single-instance lock: {error:#}");
            return Ok(Claim::Alone);
        }
    };
    let deadline = Instant::now() + timeout;
    let request = launch.encode();
    loop {
        match Lock::acquire(key, &user) {
            Ok(Some(lock)) => return Ok(Claim::First(lock)),
            Ok(None) => {}
            Err(error) => {
                log::warn!("Rainy runs without the single-instance lock: {error:#}");
                return Ok(Claim::Alone);
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match send_within(pipe_name(user.session, key), user.sid.clone(), request.clone(), replaces, left.max(RETRY)) {
            Ok(pid) => return Ok(Claim::Forwarded(pid)),
            Err(error) if Instant::now() >= deadline => return Err(error.context("The running Rainy did not take the launch")),
            Err(_) => std::thread::sleep(RETRY),
        }
    }
}

/// The profile's name for the mutex and the pipe: the first 16 hex digits of the SHA-256 of the lower-cased absolute
/// user-data folder, without a trailing separator.
pub fn profile_key(user_data: &Path) -> String {
    let folder = user_data.to_string_lossy().replace('/', "\\").to_lowercase();
    let digest = Sha256::digest(folder.trim_end_matches('\\').as_bytes());
    digest.iter().take(8).map(|byte| format!("{byte:02x}")).collect()
}

fn mutex_name(key: &str) -> String {
    format!(r"Local\Rainy-{key}")
}

/// The pipe of a profile in a Windows session: pipe names are shared by every session on the machine, the `Local\`
/// mutex is not.
fn pipe_name(session: u32, key: &str) -> String {
    format!(r"\\.\pipe\Rainy-{session}-{key}")
}

impl Lock {
    /// The lock, or `None` while another process holds it (one this user may not open counts, as a Rainy started
    /// elevated).
    fn acquire(key: &str, user: &User) -> Result<Option<Self>> {
        let security = UserOnly::new(&user.sid)?;
        let attributes = security.attributes();
        let name = wide(&mutex_name(key));
        // SAFETY: `attributes` and `name` outlive the call; the last error is cleared so it reports this call only.
        let (handle, error) = unsafe {
            SetLastError(0);
            let handle = CreateMutexW(&attributes, 0, name.as_ptr());
            (handle, std::io::Error::last_os_error())
        };
        if handle.is_null() {
            return match error.raw_os_error() {
                Some(code) if code == ERROR_ACCESS_DENIED as i32 => Ok(None),
                _ => Err(error).context("Creating the single-instance mutex"),
            };
        }
        // SAFETY: a new handle owned by nothing else.
        let mutex = unsafe { OwnedHandle::from_raw_handle(handle) };
        if error.raw_os_error() == Some(ERROR_ALREADY_EXISTS as i32) {
            return Ok(None);
        }
        Ok(Some(Self { _mutex: mutex, pipe: pipe_name(user.session, key), sid: user.sid.clone() }))
    }

    /// Answer later processes on a thread of its own for as long as this process runs, handing each launch to
    /// `on_launch` before the other process exits.
    pub fn serve(self, on_launch: impl Fn(Launch) + Send + 'static) {
        let started = std::thread::Builder::new().name("rainy-instance".into()).spawn(move || self.run(&on_launch));
        if let Err(error) = started {
            log::warn!("Launches from Explorer cannot reach this window: {error}");
        }
    }

    fn run(self, on_launch: &dyn Fn(Launch)) {
        // A Rainy that is quitting may still have its pipe for a moment after its lock is gone.
        let mut reported = false;
        let pipe = loop {
            match create_pipe(&self.pipe, &self.sid) {
                Ok(pipe) => break pipe,
                Err(error) => {
                    if !reported {
                        log::warn!("The single-instance pipe is not ready yet: {error:#}");
                        reported = true;
                    }
                    std::thread::sleep(RETRY);
                }
            }
        };
        loop {
            // SAFETY: a valid pipe handle, used synchronously.
            let connected = unsafe { ConnectNamedPipe(pipe.as_raw_handle(), std::ptr::null_mut()) } != 0
                || std::io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32);
            if connected {
                if let Err(error) = answer(&pipe, on_launch) {
                    log::warn!("A launch from another Rainy was not taken: {error:#}");
                }
            } else {
                std::thread::sleep(RETRY);
            }
            // SAFETY: as above; the next process may connect afterwards.
            unsafe { DisconnectNamedPipe(pipe.as_raw_handle()) };
        }
    }
}

fn create_pipe(name: &str, sid: &str) -> Result<File> {
    let security = UserOnly::new(sid)?;
    let attributes = security.attributes();
    let name = wide(name);
    // SAFETY: `name` and `attributes` outlive the call. One instance, so a process that took the name first is noticed.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error()).context("Creating the single-instance pipe");
    }
    // SAFETY: a new handle owned by nothing else.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle) }))
}

/// Take one launch and answer with this process's id; returns once the other process has read the answer.
fn answer(pipe: &File, on_launch: &dyn Fn(Launch)) -> Result<()> {
    let launch = Launch::decode(&read_message(pipe)?)?;
    on_launch(launch);
    write_message(pipe, &serde_json::to_vec(&Reply { pid: std::process::id() })?)?;
    // The launch is taken either way; this only keeps the answer from being cut off by the disconnect.
    let _ = pipe.sync_all();
    Ok(())
}

/// `send`, given up after `timeout` in case the process at the other end never answers.
fn send_within(pipe: String, sid: String, request: Vec<u8>, skip: Option<u32>, timeout: Duration) -> Result<u32> {
    let (done, result) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("rainy-forward".into()).spawn(move || {
        let _ = done.send(send(&pipe, &sid, &request, skip));
    })?;
    result.recv_timeout(timeout).map_err(|_| anyhow!("No answer from the running Rainy"))?
}

/// Hand a launch to the process that serves `pipe` if the user `sid` made it: allow it to take the foreground, send,
/// and wait for its answer.
fn send(pipe: &str, sid: &str, request: &[u8], skip: Option<u32>) -> Result<u32> {
    let pipe = open_pipe(pipe)?;
    // Pipe names are machine-wide, so another user may have made this one before Rainy started.
    let owner = pipe_owner(&pipe)?;
    if owner != sid {
        bail!("The single-instance pipe belongs to {owner}, not to this user");
    }
    let mut server = 0u32;
    // SAFETY: a connected pipe handle and a valid out pointer.
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut server) } != 0 {
        if skip == Some(server) {
            bail!("Rainy {server} is still quitting");
        }
        // Windows lets only the process the user just started bring a window to the front; the running Rainy may now.
        // SAFETY: a plain call with a process id.
        unsafe { AllowSetForegroundWindow(server) };
    }
    write_message(&pipe, request)?;
    let reply: Reply = serde_json::from_slice(&read_message(&pipe)?).context("Reading the running Rainy's answer")?;
    if reply.pid != server {
        // SAFETY: as above.
        unsafe { AllowSetForegroundWindow(reply.pid) };
    }
    Ok(reply.pid)
}

fn open_pipe(name: &str) -> Result<File> {
    let wide_name = wide(name);
    // SAFETY: `wide_name` outlives the call; identification only, so the server cannot act as this user.
    let handle = unsafe {
        CreateFileW(
            wide_name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) {
            // Another process is being answered; wait for the pipe to be free, then try again.
            // SAFETY: `wide_name` outlives the call.
            unsafe { WaitNamedPipeW(wide_name.as_ptr(), 1000) };
        }
        return Err(error).context("Connecting to the running Rainy");
    }
    // SAFETY: a new handle owned by nothing else.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(handle) }))
}

/// The owner of a pipe, as `S-1-5-21-…`: the user who made it, which no other user can set without administrator rights.
fn pipe_owner(pipe: &File) -> Result<String> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: a pipe opened for reading, which includes READ_CONTROL, and valid out pointers; the owner points into the
    // descriptor, which is freed below.
    let status = unsafe {
        GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32)).context("Reading who made the single-instance pipe");
    }
    let text = if owner.is_null() { Err(anyhow!("The single-instance pipe has no owner")) } else { sid_text(owner) };
    // SAFETY: allocated by GetSecurityInfo.
    unsafe { LocalFree(descriptor) };
    text
}

/// One message: its length (32-bit little-endian), then the bytes.
fn write_message(mut pipe: &File, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MESSAGE_LIMIT {
        bail!("The launch is too long to hand over");
    }
    let mut message = (bytes.len() as u32).to_le_bytes().to_vec();
    message.extend_from_slice(bytes);
    pipe.write_all(&message).context("Writing to the pipe")
}

fn read_message(mut pipe: &File) -> Result<Vec<u8>> {
    let mut length = [0u8; 4];
    pipe.read_exact(&mut length).context("Reading from the pipe")?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MESSAGE_LIMIT {
        bail!("The message from the pipe is too long");
    }
    let mut bytes = vec![0u8; length];
    pipe.read_exact(&mut bytes).context("Reading from the pipe")?;
    Ok(bytes)
}

/// Wait until a process has exited; `true` when it has, or was not running.
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    // SAFETY: a plain call; a null handle means there is no such process to wait for.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return true;
    }
    // SAFETY: a new handle owned by nothing else.
    let process = unsafe { OwnedHandle::from_raw_handle(handle) };
    let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: a valid process handle.
    unsafe { WaitForSingleObject(process.as_raw_handle(), milliseconds) == WAIT_OBJECT_0 }
}

/// A security descriptor owned by the user `sid` that grants everything to that user and nothing to anyone else. The
/// owner is set, as an elevated process would otherwise give the Administrators group.
struct UserOnly(PSECURITY_DESCRIPTOR);

impl UserOnly {
    fn new(sid: &str) -> Result<Self> {
        let sddl = wide(&format!("O:{sid}D:P(A;;GA;;;{sid})"));
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; the descriptor is freed with LocalFree on drop.
        if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut descriptor, std::ptr::null_mut()) } == 0 {
            return Err(std::io::Error::last_os_error()).context("Creating the single-instance security descriptor");
        }
        Ok(Self(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: self.0, bInheritHandle: 0 }
    }
}

impl Drop for UserOnly {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0) };
    }
}

impl User {
    /// The user and session of this process.
    fn current() -> Result<Self> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo-handle of this process and a valid out pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(std::io::Error::last_os_error()).context("Opening this process's token");
        }
        // SAFETY: a new handle owned by nothing else.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut size = 0u32;
        // SAFETY: asks for the size only.
        unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, std::ptr::null_mut(), 0, &mut size) };
        // Aligned for TOKEN_USER.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8).max(1)];
        // SAFETY: the buffer holds `size` bytes.
        if unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, buffer.as_mut_ptr().cast(), size, &mut size) } == 0 {
            return Err(std::io::Error::last_os_error()).context("Reading this process's user");
        }
        // SAFETY: GetTokenInformation(TokenUser) filled the buffer with a TOKEN_USER.
        let sid = sid_text(unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid }).context("Reading this process's user")?;
        let mut session = 0u32;
        // SAFETY: TokenSessionId is a DWORD.
        if unsafe { GetTokenInformation(token.as_raw_handle(), TokenSessionId, (&raw mut session).cast(), size_of::<u32>() as u32, &mut size) } == 0 {
            return Err(std::io::Error::last_os_error()).context("Reading this process's session");
        }
        Ok(Self { sid, session })
    }
}

/// A SID as `S-1-5-21-…`.
fn sid_text(sid: PSID) -> Result<String> {
    let mut text = std::ptr::null_mut();
    // SAFETY: a valid SID; the string is freed with LocalFree below.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(std::io::Error::last_os_error()).context("Reading a SID");
    }
    // SAFETY: a NUL-terminated string from ConvertSidToStringSidW.
    let length = unsafe { (0..).take_while(|&index| *text.add(index) != 0).count() };
    // SAFETY: `length` units precede the NUL.
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    // SAFETY: allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(text.cast()) };
    Ok(sid)
}

use crate::text::wide;

#[cfg(test)]
mod tests {
    use ::core::prelude::v1::test;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A profile key no running Rainy uses.
    fn test_key() -> String {
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).unwrap();
        format!("test-{}", bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>())
    }

    #[test]
    fn encodes_launches_exactly() {
        let launch = Launch { argv: vec![r"C:\Rainy\RainyAgent.exe".into(), "--updated".into(), r"笔记\说明 1.md".into()], cwd: r"C:\Work".into() };
        let bytes = launch.encode();
        assert_eq!(String::from_utf8(bytes.clone()).unwrap(), r#"{"argv":["C:\\Rainy\\RainyAgent.exe","--updated","笔记\\说明 1.md"],"cwd":"C:\\Work"}"#);
        assert_eq!(Launch::decode(&bytes).unwrap(), launch);
        // A lone surrogate, which no Unicode string holds, goes as UTF-16 units.
        let odd = OsString::from_wide(&[0x61, 0xd800, 0x2e, 0x6d, 0x64]);
        let launch = Launch { argv: vec!["Rainy.exe".into(), odd.clone()], cwd: PathBuf::from(OsString::from_wide(&[0x43, 0x3a, 0xdc00])) };
        let bytes = launch.encode();
        assert_eq!(String::from_utf8(bytes.clone()).unwrap(), r#"{"argv":["Rainy.exe",[97,55296,46,109,100]],"cwd":[67,58,56320]}"#);
        assert_eq!(Launch::decode(&bytes).unwrap(), launch);
        // Fields a later version adds are ignored; a missing one is not.
        assert_eq!(Launch::decode(br#"{"argv":[],"cwd":"C:\\","more":1}"#).unwrap(), Launch { argv: Vec::new(), cwd: r"C:\".into() });
        assert!(Launch::decode(br#"{"argv":[]}"#).is_err());
        assert!(Launch::decode(br#"{"argv":[1.5],"cwd":"C:\\"}"#).is_err());
        assert!(Launch::decode(b"").is_err());
    }

    #[test]
    fn names_each_profile_by_its_folder() {
        let key = profile_key(Path::new(r"C:\Users\me\AppData\Roaming\RainyAgent"));
        assert_eq!(key.len(), 16);
        assert!(key.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
        for same in [r"c:\users\ME\appdata\roaming\rainyagent", r"C:\Users\me\AppData\Roaming\RainyAgent\", "C:/Users/me/AppData/Roaming/RainyAgent"] {
            assert_eq!(profile_key(Path::new(same)), key, "{same}");
        }
        assert_ne!(profile_key(Path::new(r"C:\qa\profile")), key);
        assert_eq!(mutex_name("0123456789abcdef"), r"Local\Rainy-0123456789abcdef");
        assert_eq!(pipe_name(2, "0123456789abcdef"), r"\\.\pipe\Rainy-2-0123456789abcdef");
        assert!(User::current().unwrap().sid.starts_with("S-1-5-"));
    }

    /// Open a pipe that may still be answering another process.
    fn connect(name: &str) -> File {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match open_pipe(name) {
                Ok(pipe) => return pipe,
                Err(error) if Instant::now() >= deadline => panic!("{error:#}"),
                Err(_) => std::thread::sleep(RETRY),
            }
        }
    }

    #[test]
    fn hands_a_launch_only_to_a_pipe_this_user_made() {
        let key = test_key();
        let user = User::current().unwrap();
        let launch = Launch { argv: vec!["Rainy.exe".into(), "notes.md".into()], cwd: r"C:\Work".into() };
        let Claim::First(lock) = claim_profile(&key, &launch, None, Duration::from_secs(2)).unwrap() else { panic!("the first process takes the lock") };
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        lock.serve(move |launch| sink.lock().unwrap().push(launch));
        assert!(matches!(claim_profile(&key, &launch, None, Duration::from_secs(5)), Ok(Claim::Forwarded(_))));
        // The pipe is named for this session and owned by this user.
        let name = pipe_name(user.session, &key);
        assert_eq!(pipe_owner(&connect(&name)).unwrap(), user.sid);
        // A pipe someone else made is left before anything is written to it.
        let deadline = Instant::now() + Duration::from_secs(5);
        let refused = loop {
            let error = format!("{:#}", send(&name, "S-1-5-18", &launch.encode(), None).map(|_| ()).unwrap_err());
            if error.contains("belongs to") || Instant::now() >= deadline {
                break error;
            }
            std::thread::sleep(RETRY);
        };
        assert!(refused.contains("belongs to"), "{refused}");
        assert_eq!(received.lock().unwrap().len(), 1);
    }

    #[test]
    fn hands_a_launch_to_the_first_process() {
        let key = test_key();
        let launch = Launch { argv: vec!["Rainy.exe".into(), "notes.md".into()], cwd: r"C:\Work".into() };
        let Claim::First(lock) = claim_profile(&key, &launch, None, Duration::from_secs(2)).unwrap() else { panic!("the first process takes the lock") };
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        lock.serve(move |launch| sink.lock().unwrap().push(launch));
        // Later processes, one after another and two at once, hand theirs over; the pipe may still be starting.
        let second = Launch { argv: vec!["Rainy.exe".into(), r"D:\b.md".into()], cwd: r"D:\".into() };
        let Claim::Forwarded(pid) = claim_profile(&key, &second, None, Duration::from_secs(5)).unwrap() else { panic!("the lock is taken") };
        assert_eq!(pid, std::process::id());
        let together: Vec<_> = (0..2)
            .map(|index| {
                let (key, launch) = (key.clone(), Launch { argv: vec!["Rainy.exe".into(), format!("{index}.md").into()], cwd: r"E:\".into() });
                std::thread::spawn(move || matches!(claim_profile(&key, &launch, None, Duration::from_secs(5)), Ok(Claim::Forwarded(_))))
            })
            .collect();
        assert!(together.into_iter().all(|thread| thread.join().unwrap()));
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 3);
        assert_eq!(received[0], second);
        // A restart never hands its launch to the process it replaces, here the one that holds the lock.
        assert!(claim_profile(&key, &launch, Some(std::process::id()), Duration::from_millis(500)).is_err());
        assert_eq!(received.len(), 3);
    }

    #[test]
    fn a_profile_whose_holder_does_not_answer_times_out() {
        let key = test_key();
        let launch = Launch { argv: vec!["Rainy.exe".into()], cwd: r"C:\".into() };
        // Held, but never served.
        let Claim::First(_lock) = claim_profile(&key, &launch, None, Duration::from_secs(1)).unwrap() else { panic!("the first process takes the lock") };
        let started = Instant::now();
        assert!(claim_profile(&key, &launch, None, Duration::from_millis(400)).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn waits_for_a_process_to_exit() {
        assert!(!wait_for_exit(std::process::id(), Duration::from_millis(50)));
        let mut child = std::process::Command::new("cmd.exe").args(["/c", "exit 0"]).spawn().unwrap();
        let pid = child.id();
        assert!(wait_for_exit(pid, Duration::from_secs(10)));
        child.wait().unwrap();
        assert!(wait_for_exit(pid, Duration::from_millis(10)));
    }
}
