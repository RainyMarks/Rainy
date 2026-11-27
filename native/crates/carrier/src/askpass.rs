//! Answers the questions `ssh` asks (host key, password, key passphrase, one-time code) in Rainy's own window
//! (`src/main/ssh-askpass.ts`).
//!
//! `ssh` runs Rainy.exe itself as its askpass program. Started that way, Rainy only sends the question to the running
//! application over a loopback socket, with a token that names one `ssh` run, and prints the answer.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

use crate::fs_util::zeroize_bytes;

const ADDRESS: &str = "RAINY_ASKPASS_ADDR";
const TOKEN: &str = "RAINY_ASKPASS_TOKEN";
const MAX_ACTIVE_HANDLERS: usize = 16;

/// What `ssh` asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionKind {
    /// Trust a server seen for the first time.
    HostKey,
    /// A password, passphrase or code.
    Secret,
}

#[derive(Debug, Clone)]
pub struct Question {
    pub kind: QuestionKind,
    pub prompt: String,
    /// Cleared once the `ssh` run that asks has ended or its askpass process is gone: nobody waits for the answer, and
    /// the window stops asking.
    pub open: Arc<AtomicBool>,
}

/// Shows one question and waits for the answer; `None` cancels. Called on a server thread.
pub type Ask = Arc<dyn Fn(Question) -> Option<String> + Send + Sync>;

/// Whether `ssh` asks to trust an unknown host key.
pub fn is_host_key_question(prompt: &str) -> bool {
    prompt.contains("continue connecting (yes/no")
}

/// Whether `prompt` asks for a one-time / 2FA verification code that must never be cached and replayed.
fn is_one_time_code_question(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    [
        "verification code",
        "one-time",
        "onetime",
        "otp",
        "2fa",
        "two-factor",
        "authenticator",
        "token code",
        "passcode",
        "security code",
        "sms code",
        "验证码",
        "动态码",
        "一次性",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn wipe_string(mut secret: String) {
    // SAFETY: zeroing the UTF-8 buffer with all-zero NUL bytes preserves valid UTF-8 before drop.
    unsafe { zeroize_bytes(secret.as_bytes_mut()) };
    drop(secret);
}

/// One `ssh` run: the server it talks to and the questions it has used an answer for.
struct Run {
    scope: String,
    used: HashSet<String>,
}

#[derive(Default)]
struct State {
    /// Answers given in this application run, by server and question, so the several connections of one start ask
    /// once. The server is part of the key: a bare "Password: " from one server is never answered for another.
    remembered: HashMap<(String, String), String>,
    /// Each run's server and the questions it has used an answer for, by token.
    sessions: HashMap<String, Run>,
    /// Runs whose question the user cancelled: `ssh` takes a failed askpass as an empty answer and asks again, up to
    /// three times, so their later questions are refused without a window.
    cancelled: HashSet<String>,
    /// The question each run is asking now, by token, closed when the run ends.
    questions: HashMap<String, Arc<AtomicBool>>,
}

/// The askpass server of one application run.
pub struct Askpass {
    address: SocketAddr,
    program: PathBuf,
    state: Mutex<State>,
    /// One question at a time; a queued one may find the answer the previous one just received.
    asking: Mutex<()>,
    ask: Ask,
    closed: AtomicBool,
}

impl Drop for Askpass {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Ok(mut state) = self.state.lock() {
            for (_, secret) in state.remembered.drain() {
                wipe_string(secret);
            }
            for (_, open) in state.questions.drain() {
                open.store(false, Ordering::SeqCst);
            }
        }
        // Wake the blocking `TcpListener::incoming()` thread so it observes `Weak::upgrade() == None` and exits cleanly.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(50));
    }
}

/// The askpass identity of one `ssh` run.
pub struct Session {
    token: String,
    env: Vec<(String, String)>,
    owner: Weak<Askpass>,
}

impl Session {
    /// Variables that make this run's `ssh` ask Rainy.
    pub fn env(&self) -> &[(String, String)] {
        &self.env
    }

    /// The run failed to log in: forget the answers it used, so the next run asks again.
    pub fn failed(&self) {
        if let Some(owner) = self.owner.upgrade() {
            let mut state = owner.state.lock().unwrap();
            let Some(run) = state.sessions.get(&self.token) else { return };
            let used: Vec<(String, String)> = run.used.iter().map(|prompt| (run.scope.clone(), prompt.clone())).collect();
            for key in used {
                if let Some(secret) = state.remembered.remove(&key) {
                    wipe_string(secret);
                }
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            let mut state = owner.state.lock().unwrap();
            state.sessions.remove(&self.token);
            state.cancelled.remove(&self.token);
            // The run's `ssh` has ended; a question it left open closes.
            if let Some(open) = state.questions.remove(&self.token) {
                open.store(false, Ordering::SeqCst);
            }
        }
    }
}

/// Clear `open` once the askpass process closes its end of `stream`; it sends nothing after its question.
fn watch(mut stream: TcpStream, open: &AtomicBool) {
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
            Err(_) => break,
        }
    }
    open.store(false, Ordering::SeqCst);
}

fn random_token() -> String {
    let mut bytes = [0u8; 24];
    crate::text::fill_random(&mut bytes).expect("system random source");
    crate::text::hex(&bytes)
}

struct HandlerPermit(Arc<AtomicUsize>);

impl Drop for HandlerPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Askpass {
    /// Listen on a loopback port; `program` is what `ssh` starts (Rainy.exe).
    pub fn start(program: PathBuf, ask: Ask) -> Result<Arc<Self>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let active_handlers = Arc::new(AtomicUsize::new(0));
        let server = Arc::new(Self {
            address: listener.local_addr()?,
            program,
            state: Mutex::default(),
            asking: Mutex::new(()),
            ask,
            closed: AtomicBool::new(false),
        });
        let weak = Arc::downgrade(&server);
        thread::Builder::new().name("askpass".into()).spawn(move || {
            for stream in listener.incoming() {
                let Some(server) = weak.upgrade() else { return };
                if server.closed.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                let prev = active_handlers.fetch_add(1, Ordering::AcqRel);
                if prev >= MAX_ACTIVE_HANDLERS {
                    active_handlers.fetch_sub(1, Ordering::AcqRel);
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let permit = HandlerPermit(active_handlers.clone());
                // If spawn fails, dropping the closure drops `permit`, which decrements `active_handlers` once.
                let _ = thread::Builder::new().name("askpass-conn".into()).spawn(move || {
                    let _permit = permit;
                    server.serve(stream);
                });
            }
        })?;
        Ok(server)
    }

    /// A new identity for one `ssh` run to the server `scope` names (see `ssh::askpass_scope`). Answers are shared only
    /// between runs to the same server.
    pub fn session(self: &Arc<Self>, scope: &str) -> Session {
        let token = random_token();
        self.state.lock().unwrap().sessions.insert(token.clone(), Run { scope: scope.to_owned(), used: HashSet::new() });
        let env = vec![
            ("SSH_ASKPASS".to_owned(), self.program.display().to_string()),
            ("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned()),
            ("DISPLAY".to_owned(), "rainy".to_owned()),
            (ADDRESS.to_owned(), self.address.to_string()),
            (TOKEN.to_owned(), token.clone()),
        ];
        Session { token, env, owner: Arc::downgrade(self) }
    }

    fn serve(&self, stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
        let mut line = String::new();
        if BufReader::new((&stream).take(16384)).read_line(&mut line).is_err() {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let token = request.get("token").and_then(Value::as_str).unwrap_or_default();
        let prompt: String = request.get("prompt").and_then(Value::as_str).unwrap_or_default().chars().take(4096).collect();
        let known = self.state.lock().unwrap().sessions.contains_key(token);
        let reply = if known {
            // `ssh` may end while the window asks, killing its askpass process with it.
            let open = Arc::new(AtomicBool::new(true));
            if let Ok(watched) = stream.try_clone() {
                let open = open.clone();
                let _ = thread::Builder::new().name("askpass-watch".into()).spawn(move || watch(watched, &open));
            }
            self.answer(token, &prompt, &open)
        } else {
            None
        };
        let body = reply.map_or_else(|| json!({}), |answer| json!({ "answer": answer }));
        let mut serialized = body.to_string();
        let mut stream = stream;
        let _ = stream.write_all(serialized.as_bytes());
        let _ = stream.shutdown(Shutdown::Both);
        // Zeroize serialized reply containing credentials before freeing.
        unsafe { zeroize_bytes(serialized.as_bytes_mut()) };
    }

    fn answer(&self, token: &str, prompt: &str, open: &Arc<AtomicBool>) -> Option<String> {
        let _turn = self.asking.lock().unwrap();
        let kind = if is_host_key_question(prompt) { QuestionKind::HostKey } else { QuestionKind::Secret };
        let cacheable = kind == QuestionKind::Secret && !is_one_time_code_question(prompt);
        {
            let mut state = self.state.lock().unwrap();
            let State { remembered, sessions, cancelled, questions } = &mut *state;
            if cancelled.contains(token) {
                return None;
            }
            let run = sessions.get_mut(token)?;
            let key = (run.scope.clone(), prompt.to_owned());
            if cacheable
                && !run.used.contains(prompt)
                && let Some(answer) = remembered.get(&key)
            {
                run.used.insert(prompt.to_owned());
                return Some(answer.clone());
            }
            // A question asked twice in one run means its earlier answer did not work.
            if run.used.contains(prompt)
                && let Some(secret) = remembered.remove(&key)
            {
                wipe_string(secret);
            }
            questions.insert(token.to_owned(), open.clone());
        }
        let reply = (self.ask)(Question { kind, prompt: prompt.to_owned(), open: open.clone() });
        let mut state = self.state.lock().unwrap();
        state.questions.remove(token);
        // Nobody waits for an answer the run gave up on; it is neither remembered nor taken as a cancel.
        if !open.load(Ordering::SeqCst) {
            if let Some(secret) = reply {
                wipe_string(secret);
            }
            return None;
        }
        match &reply {
            Some(answer) if cacheable => {
                if let Some(run) = state.sessions.get_mut(token) {
                    run.used.insert(prompt.to_owned());
                    let key = (run.scope.clone(), prompt.to_owned());
                    if let Some(previous) = state.remembered.insert(key, answer.clone()) {
                        wipe_string(previous);
                    }
                }
            }
            Some(_) => {}
            None => {
                if state.sessions.contains_key(token) {
                    state.cancelled.insert(token.to_owned());
                }
            }
        }
        reply
    }
}

/// When `ssh` started this process as its askpass program: ask the running application and print the answer.
/// Returns the exit code, or `None` for an ordinary start.
pub fn client() -> Option<i32> {
    let address = std::env::var(ADDRESS).ok()?;
    let token = std::env::var(TOKEN).ok()?;
    let prompt = std::env::args_os().nth(1).map(|prompt| prompt.to_string_lossy().into_owned()).unwrap_or_default();
    let ask = || -> Option<String> {
        let mut stream = TcpStream::connect(address.parse::<SocketAddr>().ok()?).ok()?;
        stream.write_all(format!("{}\n", json!({ "token": token, "prompt": prompt })).as_bytes()).ok()?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).ok()?;
        let answer = serde_json::from_str::<Value>(&reply).ok()?.get("answer")?.as_str().map(str::to_owned);
        wipe_string(reply);
        answer
    };
    Some(match ask() {
        Some(answer) => {
            let mut stdout = std::io::stdout();
            let status = match writeln!(stdout, "{answer}").and_then(|()| stdout.flush()) {
                Ok(()) => 0,
                Err(_) => 1,
            };
            wipe_string(answer);
            status
        }
        None => 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ask_once(server: &Askpass, session: &Session, prompt: &str) -> Option<String> {
        let mut stream = TcpStream::connect(server.address).unwrap();
        stream.write_all(format!("{}\n", json!({ "token": session.token, "prompt": prompt })).as_bytes()).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        serde_json::from_str::<Value>(&reply).unwrap().get("answer").and_then(Value::as_str).map(str::to_owned)
    }

    #[test]
    fn remembers_answers_for_one_run() {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let server = Askpass::start(PathBuf::from("Rainy.exe"), Arc::new(move |question: Question| {
            counter.fetch_add(1, Ordering::SeqCst);
            (question.kind == QuestionKind::Secret).then(|| "secret".to_owned())
        }))
        .unwrap();
        let first = server.session("alice@one:22");
        assert!(first.env().iter().any(|(name, value)| name == "SSH_ASKPASS_REQUIRE" && value == "force"));
        assert_eq!(ask_once(&server, &first, "Password: ").as_deref(), Some("secret"));
        let second = server.session("alice@one:22");
        assert_eq!(ask_once(&server, &second, "Password: ").as_deref(), Some("secret"));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(ask_once(&server, &second, "Password: ").as_deref(), Some("secret"));
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        assert_eq!(ask_once(&server, &first, "Are you sure you want to continue connecting (yes/no/[fingerprint])? "), None);
        let stale = Session { token: "stale".into(), env: Vec::new(), owner: Weak::new() };
        assert_eq!(ask_once(&server, &stale, "Password: "), None);
    }

    #[test]
    fn never_caches_one_time_verification_codes() {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let server = Askpass::start(
            PathBuf::from("Rainy.exe"),
            Arc::new(move |_question: Question| {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                Some(format!("code-{n}"))
            }),
        )
        .unwrap();
        for prompt in ["Verification code: ", "Enter OTP passcode:", "请输入动态验证码："] {
            let s1 = server.session("alice@mfa:22");
            let s2 = server.session("alice@mfa:22");
            let first = ask_once(&server, &s1, prompt);
            let second = ask_once(&server, &s2, prompt);
            assert_ne!(first, second, "OTP prompt must not replay cached answer: {prompt}");
        }
        assert_eq!(asked.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn an_answer_is_never_given_to_another_server() {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let server = Askpass::start(PathBuf::from("Rainy.exe"), Arc::new(move |_question: Question| {
            counter.fetch_add(1, Ordering::SeqCst);
            Some("secret-for-one".to_owned())
        }))
        .unwrap();
        let one = server.session("alice@one:22");
        assert_eq!(ask_once(&server, &one, "Password: ").as_deref(), Some("secret-for-one"));
        let other = server.session("alice@other:22");
        assert_eq!(ask_once(&server, &other, "Password: ").as_deref(), Some("secret-for-one"));
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        let same_host_other_port = server.session("alice@one:2222");
        ask_once(&server, &same_host_other_port, "Password: ");
        assert_eq!(asked.load(Ordering::SeqCst), 3);
        other.failed();
        let again = server.session("alice@one:22");
        ask_once(&server, &again, "Password: ");
        assert_eq!(asked.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_cancelled_question_ends_the_run_questions() {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        let server = Askpass::start(PathBuf::from("Rainy.exe"), Arc::new(move |_: Question| (counter.fetch_add(1, Ordering::SeqCst) > 0).then(|| "pw".to_owned()))).unwrap();
        let run = server.session("alice@one:22");
        assert_eq!(ask_once(&server, &run, "Password: "), None);
        assert_eq!(ask_once(&server, &run, "Password: "), None);
        assert_eq!(ask_once(&server, &run, "Enter passphrase for key: "), None);
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        let next = server.session("alice@one:22");
        assert_eq!(ask_once(&server, &next, "Password: ").as_deref(), Some("pw"));
        drop(run);
        assert!(server.state.lock().unwrap().cancelled.is_empty());
    }

    fn until_closed() -> Ask {
        Arc::new(|question: Question| {
            while question.open.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(10));
            }
            Some("too late".to_owned())
        })
    }

    #[test]
    fn a_question_closes_when_its_run_ends() {
        let server = Askpass::start(PathBuf::from("Rainy.exe"), until_closed()).unwrap();
        let run = server.session("alice@one:22");
        let (token, address) = (run.token.clone(), server.address);
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(format!("{}\n", json!({ "token": token, "prompt": "Password: " })).as_bytes()).unwrap();
            let mut reply = String::new();
            stream.read_to_string(&mut reply).unwrap();
            reply
        });
        while !server.state.lock().unwrap().questions.contains_key(&run.token) {
            thread::sleep(Duration::from_millis(10));
        }
        drop(run);
        assert_eq!(client.join().unwrap(), "{}");
        let state = server.state.lock().unwrap();
        assert!(state.remembered.is_empty() && state.cancelled.is_empty() && state.questions.is_empty());
    }

    #[test]
    fn a_question_closes_when_its_askpass_process_goes() {
        let server = Askpass::start(PathBuf::from("Rainy.exe"), until_closed()).unwrap();
        let run = server.session("alice@one:22");
        let mut stream = TcpStream::connect(server.address).unwrap();
        stream.write_all(format!("{}\n", json!({ "token": run.token, "prompt": "Password: " })).as_bytes()).unwrap();
        while !server.state.lock().unwrap().questions.contains_key(&run.token) {
            thread::sleep(Duration::from_millis(10));
        }
        drop(stream);
        while !server.state.lock().unwrap().questions.is_empty() {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(server.state.lock().unwrap().remembered.is_empty());
    }
}
