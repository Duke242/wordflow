//! The `wordflow-cleanup` helper program (cleanup-worker/), which runs llama.cpp
//! models in their own process: cleanup models for `cleanup`, and Granite Speech
//! for `engine`. It's separate because whisper.cpp and llama.cpp each bundle a
//! different version of ggml, and only one copy survives linking into one
//! binary. It also keeps a crash or hang in a model away from the app.
//!
//! Requests and replies are one JSON object per line on its stdin and stdout.

use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

const WORKER_NAME: &str = "wordflow-cleanup";

/// Where the helper program is: next to the app's own executable once
/// installed, or in the cleanup-worker build folder while developing.
/// `WORDFLOW_CLEANUP_WORKER` overrides both.
pub fn worker_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("WORDFLOW_CLEANUP_WORKER") {
        return Some(PathBuf::from(path));
    }
    let file = format!("{WORKER_NAME}{}", std::env::consts::EXE_SUFFIX);
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        candidates.push(dir.join(&file));
    }
    if cfg!(debug_assertions) {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../cleanup-worker/target");
        candidates.push(target.join("release").join(&file));
        candidates.push(target.join("debug").join(&file));
    }
    candidates.into_iter().find(|path| path.is_file())
}

#[derive(Deserialize)]
struct Reply {
    text: Option<String>,
    error: Option<String>,
}

/// A running helper. Dropping it stops the process.
pub struct Worker {
    pub id: u64,
    /// "cleanup" or "speech", for messages.
    role: &'static str,
    child: Child,
    stdin: ChildStdin,
    /// Lines from the helper's stdout, read on their own thread so waiting for
    /// one can time out.
    replies: Receiver<String>,
    /// What's loaded, as the caller describes it.
    pub loaded: Option<PathBuf>,
    pub last_used: Instant,
    /// Set when it stopped answering properly. An error reply from a working
    /// helper doesn't set it, so the model stays loaded.
    pub broken: bool,
}

impl Worker {
    pub fn spawn(id: u64, role: &'static str) -> Result<Self, String> {
        let path = worker_path()
            .ok_or_else(|| format!("This build of WordFlow doesn't include the {role} helper"))?;
        let mut command = Command::new(&path);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("Couldn't start the {role} helper: {e}"))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(format!("Couldn't talk to the {role} helper"));
        };

        let (sender, replies) = mpsc::channel();
        let reader = thread::Builder::new()
            .name(format!("{role}-reader"))
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });
        if let Err(e) = reader {
            let _ = child.kill();
            return Err(format!("Couldn't start the {role} helper: {e}"));
        }
        Ok(Self {
            id,
            role,
            child,
            stdin,
            replies,
            loaded: None,
            last_used: Instant::now(),
            broken: false,
        })
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn call(&mut self, request: &impl Serialize, timeout: Duration) -> Result<String, String> {
        let result = self.exchange(request, timeout);
        if let Err(Failure::Broken(_)) = result {
            self.broken = true;
        }
        result.map_err(|failure| match failure {
            Failure::Broken(e) | Failure::Refused(e) => e,
        })
    }

    fn exchange(&mut self, request: &impl Serialize, timeout: Duration) -> Result<String, Failure> {
        let role = self.role;
        let broken = Failure::Broken;
        self.last_used = Instant::now();
        let mut line = serde_json::to_vec(request).map_err(|e| Failure::Refused(e.to_string()))?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .and_then(|()| self.stdin.flush())
            .map_err(|e| broken(format!("The {role} helper stopped: {e}")))?;
        let reply = match self.replies.recv_timeout(timeout) {
            Ok(reply) => reply,
            Err(RecvTimeoutError::Timeout) => {
                return Err(broken(format!(
                    "The {role} model took longer than {timeout:.0?}"
                )));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(broken(format!("The {role} helper stopped")));
            }
        };
        self.last_used = Instant::now();
        let reply: Reply = serde_json::from_str(&reply)
            .map_err(|e| broken(format!("Unreadable reply from the {role} helper: {e}")))?;
        match (reply.text, reply.error) {
            (_, Some(error)) => Err(Failure::Refused(error)),
            (Some(text), None) => Ok(text),
            (None, None) => Err(broken(format!("Empty reply from the {role} helper"))),
        }
    }
}

enum Failure {
    /// The helper can't be trusted with another request: it hung, quit, or
    /// sent something unreadable.
    Broken(String),
    /// The helper answered with an error, and is fine for the next request.
    Refused(String),
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
