//! Answers the questions `ssh` asks (host key, password, key passphrase, one-time code) in Rainy's own window
//! (`src/main/ssh-askpass.ts`).
//!
//! `ssh` runs Rainy.exe itself as its askpass program. Started that way, Rainy only sends the question to the running
//! application over a loopback socket, with a token that names one `ssh` run, and prints the answer.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

const ADDRESS: &str = "RAINY_ASKPASS_ADDR";
const TOKEN: &str = "RAINY_ASKPASS_TOKEN";

/// What `ssh` asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionKind {
    /// Trust a server seen for the first time.
    HostKey,
    /// A password, passphrase or code.
    Secret,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub kind: QuestionKind,
    pub prompt: String,
}

/// Shows one question and waits for the answer; `None` cancels. Called on a server thread.
pub type Ask = Arc<dyn Fn(Question) -> Option<String> + Send + Sync>;

/// Whether `ssh` asks to trust an unknown host key.
pub fn is_host_key_question(prompt: &str) -> bool {
    prompt.contains("continue connecting (yes/no")
}

#[derive(Default)]
struct State {
    /// Answers given in this application run, by question, so the several connections of one start ask once.
    remembered: HashMap<String, String>,
    /// Questions each run has used an answer for, by token.
    sessions: HashMap<String, HashSet<String>>,
    /// Runs whose question the user cancelled: `ssh` takes a failed askpass as an empty answer and asks again, up to
    /// three times, so their later questions are refused without a window.
    cancelled: HashSet<String>,
}

/// The askpass server of one application run.
pub struct Askpass {
    address: SocketAddr,
    program: PathBuf,
    state: Mutex<State>,
    /// One question at a time; a queued one may find the answer the previous one just received.
    asking: Mutex<()>,
    ask: Ask,
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
            let used: Vec<String> = state.sessions.get(&self.token).map(|used| used.iter().cloned().collect()).unwrap_or_default();
            for prompt in used {
                state.remembered.remove(&prompt);
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
        }
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).expect("system random source");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Askpass {
    /// Listen on a loopback port; `program` is what `ssh` starts (Rainy.exe).
    pub fn start(program: PathBuf, ask: Ask) -> Result<Arc<Self>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let server = Arc::new(Self { address: listener.local_addr()?, program, state: Mutex::default(), asking: Mutex::new(()), ask });
        let weak = Arc::downgrade(&server);
        thread::Builder::new().name("askpass".into()).spawn(move || {
            for stream in listener.incoming() {
                let Some(server) = weak.upgrade() else { return };
                let Ok(stream) = stream else { continue };
                thread::spawn(move || server.serve(stream));
            }
        })?;
        Ok(server)
    }

    /// A new identity for one `ssh` run.
    pub fn session(self: &Arc<Self>) -> Session {
        let token = random_token();
        self.state.lock().unwrap().sessions.insert(token.clone(), HashSet::new());
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
        let mut line = String::new();
        if BufReader::new((&stream).take(16384)).read_line(&mut line).is_err() {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let token = request.get("token").and_then(Value::as_str).unwrap_or_default();
        let prompt: String = request.get("prompt").and_then(Value::as_str).unwrap_or_default().chars().take(4096).collect();
        let reply = if self.state.lock().unwrap().sessions.contains_key(token) { self.answer(token, &prompt) } else { None };
        let body = reply.map_or_else(|| json!({}), |answer| json!({ "answer": answer }));
        let mut stream = stream;
        let _ = stream.write_all(body.to_string().as_bytes());
    }

    fn answer(&self, token: &str, prompt: &str) -> Option<String> {
        let _turn = self.asking.lock().unwrap();
        let kind = if is_host_key_question(prompt) { QuestionKind::HostKey } else { QuestionKind::Secret };
        {
            let mut state = self.state.lock().unwrap();
            let State { remembered, sessions, cancelled } = &mut *state;
            if cancelled.contains(token) {
                return None;
            }
            let used = sessions.get_mut(token)?;
            if kind == QuestionKind::Secret
                && !used.contains(prompt)
                && let Some(answer) = remembered.get(prompt)
            {
                used.insert(prompt.to_owned());
                return Some(answer.clone());
            }
            // A question asked twice in one run means its earlier answer did not work.
            if used.contains(prompt) {
                remembered.remove(prompt);
            }
        }
        let reply = (self.ask)(Question { kind, prompt: prompt.to_owned() });
        let mut state = self.state.lock().unwrap();
        match &reply {
            Some(answer) if kind == QuestionKind::Secret => {
                state.remembered.insert(prompt.to_owned(), answer.clone());
                if let Some(used) = state.sessions.get_mut(token) {
                    used.insert(prompt.to_owned());
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
        serde_json::from_str::<Value>(&reply).ok()?.get("answer")?.as_str().map(str::to_owned)
    };
    Some(match ask() {
        Some(answer) => {
            let mut stdout = std::io::stdout();
            match writeln!(stdout, "{answer}").and_then(|()| stdout.flush()) {
                Ok(()) => 0,
                Err(_) => 1,
            }
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
        let first = server.session();
        assert!(first.env().iter().any(|(name, value)| name == "SSH_ASKPASS_REQUIRE" && value == "force"));
        assert_eq!(ask_once(&server, &first, "Password: ").as_deref(), Some("secret"));
        // Another connection of the same start gets the answer without asking.
        let second = server.session();
        assert_eq!(ask_once(&server, &second, "Password: ").as_deref(), Some("secret"));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        // Asked again in the same run: the answer was wrong, so the user is asked anew.
        assert_eq!(ask_once(&server, &second, "Password: ").as_deref(), Some("secret"));
        assert_eq!(asked.load(Ordering::SeqCst), 2);
        // Host keys are never remembered, and unknown tokens get nothing.
        assert_eq!(ask_once(&server, &first, "Are you sure you want to continue connecting (yes/no/[fingerprint])? "), None);
        let stale = Session { token: "stale".into(), env: Vec::new(), owner: Weak::new() };
        assert_eq!(ask_once(&server, &stale, "Password: "), None);
    }

    #[test]
    fn a_cancelled_question_ends_the_run_questions() {
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = asked.clone();
        // The user cancels the first question and would answer any later one.
        let server = Askpass::start(PathBuf::from("Rainy.exe"), Arc::new(move |_: Question| (counter.fetch_add(1, Ordering::SeqCst) > 0).then(|| "pw".to_owned()))).unwrap();
        let run = server.session();
        assert_eq!(ask_once(&server, &run, "Password: "), None);
        // ssh asks again after the empty answer; no window opens for it.
        assert_eq!(ask_once(&server, &run, "Password: "), None);
        assert_eq!(ask_once(&server, &run, "Enter passphrase for key: "), None);
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        // The next run asks again.
        let next = server.session();
        assert_eq!(ask_once(&server, &next, "Password: ").as_deref(), Some("pw"));
        drop(run);
        assert!(server.state.lock().unwrap().cancelled.is_empty());
    }
}
