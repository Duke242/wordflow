//! Inserts text into the focused app by putting it on the clipboard and
//! simulating the paste shortcut, then puts the user's clipboard back. Also
//! copies the selection the same way, for command mode.
//!
//! One thread owns the clipboard for the app's lifetime. On Linux the clipboard
//! is served by the process that set it, so a short-lived handle would lose the
//! text before the target app reads it.

use std::{
    sync::mpsc::{self, Sender},
    thread,
    time::{Duration, Instant},
};

use arboard::{Clipboard, ImageData};
use enigo::{Enigo, Keyboard, Settings};

use crate::{platform, tone};

/// Gives the user time to let go of the hotkey's modifier keys, which would
/// otherwise combine with the simulated paste shortcut.
const BEFORE_PASTE: Duration = Duration::from_millis(250);
/// How long the target app gets to read the clipboard before it is restored.
const BEFORE_RESTORE: Duration = Duration::from_millis(350);
/// How long the focused app gets to take in pasted text before Return sends it.
const BEFORE_SEND: Duration = Duration::from_millis(150);
/// How long the focused app gets to put its selection on the clipboard.
const COPY_WAIT: Duration = Duration::from_millis(600);
const COPY_POLL: Duration = Duration::from_millis(30);

enum Job {
    Paste(PasteJob),
    /// Presses Return, after text typed some other way, as by live typing.
    Send,
    /// Copies the selection, sending it back, then restores the clipboard.
    Copy {
        terminal: bool,
        reply: Sender<Result<Option<String>, String>>,
    },
}

struct PasteJob {
    text: String,
    restore_clipboard: bool,
    /// Press Return after pasting, to send a message.
    send: bool,
}

/// Handle to the paste thread. Cloning it is cheap; the thread exits once every
/// handle is gone.
#[derive(Clone)]
pub struct Paster {
    tx: Sender<Job>,
}

enum Saved {
    Text(String),
    Image(ImageData<'static>),
    Empty,
}

impl Paster {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let spawned = thread::Builder::new().name("paste".into()).spawn(move || {
            // Both are created on first use and retried if that fails. The
            // macOS keyboard permission prompt then appears when the user
            // first dictates, not at launch.
            let mut clipboard = None;
            let mut enigo = None;
            for job in rx {
                if clipboard.is_none() {
                    clipboard = Clipboard::new()
                        .map_err(|e| eprintln!("[paste] clipboard unavailable: {e}"))
                        .ok();
                }
                let Some(clipboard) = clipboard.as_mut() else {
                    continue;
                };
                if enigo.is_none() {
                    enigo = Enigo::new(&Settings::default())
                        .map_err(|e| eprintln!("[paste] keyboard simulation unavailable: {e}"))
                        .ok();
                }
                match job {
                    Job::Paste(job) => {
                        if let Err(e) = paste(clipboard, enigo.as_mut(), &job) {
                            eprintln!("[paste] {e}");
                        }
                    }
                    Job::Send => {
                        thread::sleep(BEFORE_SEND);
                        if let Err(e) = platform::send_return_key(enigo.as_mut()) {
                            eprintln!("[paste] {e}");
                        }
                    }
                    Job::Copy { terminal, reply } => {
                        let _ = reply.send(copy(clipboard, enigo.as_mut(), terminal));
                    }
                }
            }
        });
        // Without the thread, jobs are dropped but transcripts still reach
        // History, the same as when the clipboard is unavailable.
        if let Err(e) = spawned {
            eprintln!("[paste] couldn't start the paste thread: {e}");
        }
        Self { tx }
    }

    /// Queues `text` to be pasted into the focused app, then Return pressed
    /// if `send`. Returns immediately.
    pub fn paste(&self, text: String, restore_clipboard: bool, send: bool) {
        let _ = self.tx.send(Job::Paste(PasteJob {
            text,
            restore_clipboard,
            send,
        }));
    }

    /// Queues a press of Return. Returns immediately.
    pub fn send(&self) {
        let _ = self.tx.send(Job::Send);
    }

    /// The text selected in the focused app, or `None` when nothing is.
    /// Simulates the copy shortcut and puts the clipboard back afterwards.
    /// Waits for any paste queued before it.
    pub fn copy_selection(&self, terminal: bool) -> Result<Option<String>, String> {
        let (reply, result) = mpsc::channel();
        self.tx
            .send(Job::Copy { terminal, reply })
            .map_err(|_| "The clipboard isn't available")?;
        result
            .recv()
            .map_err(|_| "The clipboard isn't available".to_string())?
    }
}

fn copy(
    clipboard: &mut Clipboard,
    enigo: Option<&mut Enigo>,
    terminal: bool,
) -> Result<Option<String>, String> {
    // Text and images are put back afterwards. Anything else, like copied
    // files, can't be, so it's left alone rather than cleared: copying with
    // nothing selected then keeps it, and as it has no text, any text that
    // appears is the selection.
    let saved = match save(clipboard) {
        Saved::Empty => None,
        saved => Some(saved),
    };
    if saved.is_some() {
        // Cleared first, so a selection is told apart from what was there before.
        clipboard
            .clear()
            .map_err(|e| format!("Couldn't clear clipboard: {e}"))?;
    }
    thread::sleep(BEFORE_PASTE);
    let sent = platform::send_copy_shortcut(enigo, terminal);
    let mut copied = None;
    if sent.is_ok() {
        let started = Instant::now();
        while started.elapsed() < COPY_WAIT {
            if let Ok(text) = clipboard.get_text()
                && !text.trim().is_empty()
            {
                copied = Some(text);
                break;
            }
            thread::sleep(COPY_POLL);
        }
    }
    let restored = match saved {
        Some(Saved::Text(text)) => clipboard.set_text(text),
        Some(Saved::Image(image)) => clipboard.set_image(image),
        Some(Saved::Empty) | None => Ok(()),
    };
    if let Err(e) = restored {
        eprintln!("[paste] couldn't restore clipboard: {e}");
    }
    sent.map(|()| copied)
}

fn paste(
    clipboard: &mut Clipboard,
    mut enigo: Option<&mut Enigo>,
    job: &PasteJob,
) -> Result<(), String> {
    // Google Search drops pastes after the clipboard changes, so the text is
    // typed there instead. A search box is one line, so line breaks become spaces.
    if platform::frontmost_app().is_some_and(|app| tone::is_google_search(&app)) {
        let enigo = enigo.as_deref_mut().ok_or("Keyboard simulation is unavailable")?;
        thread::sleep(BEFORE_PASTE);
        let text = job.text.split_whitespace().collect::<Vec<_>>().join(" ");
        enigo
            .text(&text)
            .map_err(|e| format!("Couldn't type the text: {e}"))?;
        if job.send {
            thread::sleep(BEFORE_SEND);
            platform::send_return_key(enigo.into())?;
        }
        return Ok(());
    }

    let saved = job.restore_clipboard.then(|| save(clipboard));
    clipboard
        .set_text(job.text.as_str())
        .map_err(|e| format!("Couldn't set clipboard: {e}"))?;

    thread::sleep(BEFORE_PASTE);
    // If this fails the text stays on the clipboard for the user to paste by hand.
    platform::send_paste_shortcut(enigo.as_deref_mut())?;
    if job.send {
        thread::sleep(BEFORE_SEND);
        // The text is in, so the clipboard is still put back if this fails.
        if let Err(e) = platform::send_return_key(enigo) {
            eprintln!("[paste] {e}");
        }
    }

    let Some(saved) = saved else {
        return Ok(());
    };
    thread::sleep(BEFORE_RESTORE);
    // If the user copied something else in the meantime, keep that instead.
    if clipboard.get_text().ok().as_deref() != Some(job.text.as_str()) {
        return Ok(());
    }
    match saved {
        Saved::Text(text) => clipboard.set_text(text),
        Saved::Image(image) => clipboard.set_image(image),
        Saved::Empty => clipboard.clear(),
    }
    .map_err(|e| format!("Couldn't restore clipboard: {e}"))
}

fn save(clipboard: &mut Clipboard) -> Saved {
    if let Ok(text) = clipboard.get_text() {
        return Saved::Text(text);
    }
    if let Ok(image) = clipboard.get_image() {
        return Saved::Image(image);
    }
    Saved::Empty
}
