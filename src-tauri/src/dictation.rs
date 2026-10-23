//! The dictation state machine: hotkey, record, transcribe, paste.
//!
//! All events go through one controller thread, so the session state has a
//! single owner and events are handled in the order they were sent, including
//! progress from transcription workers. This also keeps shortcut registration
//! out of the global shortcut plugin's event handler, which holds a lock while
//! it runs.
//!
//! What user input does is decided by [`decide`], a pure function, so it can be
//! tested without a microphone or windows.
//!
//! In Hold mode, double-tapping the hotkey locks recording on, so the user can
//! let go and keep talking. The next press stops it. This works the same for
//! every kind of hotkey, since it only looks at presses and releases. Hybrid
//! mode is Hold mode where a single quick tap locks.

use std::{
    mem,
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcut, GlobalShortcutExt, ShortcutState};

use crate::{
    AppState, LockExt,
    audio::{Captured, Recording},
    cleanup, command,
    history::now_ms,
    learn, live,
    paste::Paster,
    pill::{self, PillState},
    pipeline,
    platform::{self, FrontApp, HotkeyEvent},
    playback::Playback,
    settings::RecordingMode,
    stream, tone,
};

const CANCEL_KEY: &str = "Escape";
/// Finish a recording and send it, when Finish and send is on. On macOS the
/// keypad Enter is a key of its own, and what Fn+Return gives. Elsewhere Enter
/// covers both, and registering keypad Enter as well fails.
#[cfg(target_os = "macos")]
const SEND_KEYS: &[&str] = &["Enter", "NumpadEnter"];
#[cfg(not(target_os = "macos"))]
const SEND_KEYS: &[&str] = &["Enter"];

const TRANSCRIBING: &str = "Transcribing...";
const CLEANING_UP: &str = "Cleaning up...";
const EDITING: &str = "Editing...";
const STOPPING: &str = "Stopping transcription...";

/// How long status messages stay in the pill.
const ERROR_MESSAGE: Duration = Duration::from_millis(2000);
const NO_SPEECH_MESSAGE: Duration = Duration::from_millis(1500);
const STOPPING_MESSAGE: Duration = Duration::from_millis(800);
const LEARNED_MESSAGE: Duration = Duration::from_millis(2500);

/// Hold mode: a press released this quickly is a tap, and a second press this
/// soon after a tap locks recording on.
const DOUBLE_TAP: Duration = Duration::from_millis(350);

/// How long a recording runs before media is paused. A single-modifier hotkey
/// starts a recording for every shortcut like ⌘C, which is discarded a moment
/// later; pausing straight away would stutter the music on each one.
const PAUSE_MEDIA_AFTER: Duration = Duration::from_millis(250);

/// "Learned “Karan”", or "Learned “Karan” and 2 more" for several words.
fn learned_message(words: &[String]) -> String {
    match words {
        [] => "Learned a new word".into(),
        [word] => format!("Learned “{word}”"),
        [word, rest @ ..] => format!("Learned “{word}” and {} more", rest.len()),
    }
}

/// Broadcast to the UI as `dictation-state`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum DictationState {
    Idle,
    #[serde(rename_all = "camelCase")]
    Recording {
        started_at_ms: u64,
    },
    Transcribing,
}

pub enum Event {
    HotkeyDown,
    HotkeyUp,
    /// Another key was pressed while a single-modifier hotkey was held.
    HotkeyInterrupted,
    /// The command mode shortcut, which records an edit for the selected text.
    CommandHotkeyDown,
    CommandHotkeyUp,
    /// Start or stop from the UI or tray, regardless of recording mode.
    Toggle,
    Escape,
    /// Return while recording, with Finish and send on.
    Send,
    /// Paste the latest transcript again, for when the first paste missed.
    PasteLast,
    /// A transcription worker moved on to another stage.
    Stage {
        session: u64,
        stage: pipeline::Stage,
    },
    Transcribed {
        session: u64,
        outcome: Outcome,
    },
    LiveTypingPaused {
        session: u64,
    },
    /// The time for a second tap after a quick tap in Hold mode has passed.
    TapExpired {
        session: u64,
    },
    /// A recording has run long enough to be a dictation: pause media.
    PauseMedia {
        session: u64,
    },
    MessageExpired {
        generation: u64,
    },
    /// Settings changed: the pill may need to appear, hide or move.
    RefreshPill,
    /// Words learned from a correction, to show in the pill.
    Learned {
        words: Vec<String>,
    },
}

type Outcome = Result<String, pipeline::Error>;

/// The two global shortcuts: dictation, and command mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shortcut {
    Dictation,
    Command,
}

impl Shortcut {
    fn other(self) -> Self {
        match self {
            Shortcut::Dictation => Shortcut::Command,
            Shortcut::Command => Shortcut::Dictation,
        }
    }
}

/// Input from the user, as opposed to progress from the app itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Input {
    HotkeyDown(Shortcut),
    HotkeyUp(Shortcut),
    /// Only the dictation hotkey, as a single modifier key, is interrupted.
    HotkeyInterrupted,
    Toggle,
    Escape,
    Send,
    /// No second tap came after a quick tap in Hold mode.
    TapExpired,
}

/// The part of [`Phase`] that decides what input does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Activity {
    /// Idle, or showing a status message.
    Idle,
    Recording {
        /// The shortcut that started it, or `None` from the UI or tray.
        by: Option<Shortcut>,
        /// Hold mode only: double-tapped, so it records without the shortcut held.
        locked: bool,
    },
    Transcribing,
}

/// What the shortcuts have been doing, as far as [`decide`] needs to know.
#[derive(Debug, Default, Clone, Copy)]
struct Hotkey {
    /// Each shortcut held right now, so key repeat is ignored and one
    /// shortcut's keys never end a recording the other one is holding.
    dictation_down: bool,
    command_down: bool,
    /// When the shortcut that started the recording was last pressed.
    pressed_at: Option<Instant>,
    /// Hold mode: when a quick tap was let go. A second press soon after locks.
    tapped_at: Option<Instant>,
    /// Locked: releasing this press stops recording, unless another key was
    /// pressed with it, which makes it a shortcut like ⌘C instead.
    stop_on_release: bool,
}

impl Hotkey {
    fn down(&self, shortcut: Shortcut) -> bool {
        match shortcut {
            Shortcut::Dictation => self.dictation_down,
            Shortcut::Command => self.command_down,
        }
    }

    /// Returns whether it was down before.
    fn set(&mut self, shortcut: Shortcut, down: bool) -> bool {
        let flag = match shortcut {
            Shortcut::Dictation => &mut self.dictation_down,
            Shortcut::Command => &mut self.command_down,
        };
        mem::replace(flag, down)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Nothing,
    Start {
        by: Option<Shortcut>,
    },
    /// Stops recording and transcribes. `cancelled` keeps the result from being pasted.
    Stop {
        cancelled: bool,
    },
    /// Stops recording and throws the audio away.
    Discard,
    /// Leaves the running transcription to finish in the background.
    CancelTranscription,
    /// Hold mode: keeps recording for a moment after a quick tap, in case a
    /// second tap follows. Ends with [`Input::TapExpired`].
    AwaitSecondTap,
    /// Hold mode: keeps recording until the hotkey is pressed again.
    Lock,
    /// Stops recording, transcribes, and presses Return once the text is in.
    Send,
}

/// Decides what `input`, arriving at `now`, does.
///
/// The dictation and command shortcuts follow the same rules, each for the
/// recordings it starts: in Hold mode only the shortcut that started a
/// recording stops it (or double-taps to lock it), and neither stops a
/// recording while the other is still held.
fn decide(
    input: Input,
    activity: Activity,
    mode: RecordingMode,
    hotkey: &mut Hotkey,
    now: Instant,
) -> Action {
    let hold = mode.holds();
    let within_tap = |since: Option<Instant>| since.is_some_and(|t| now - t <= DOUBLE_TAP);
    match input {
        Input::HotkeyDown(shortcut) => {
            if hotkey.set(shortcut, true) {
                return Action::Nothing;
            }
            match activity {
                Activity::Idle => {
                    hotkey.pressed_at = Some(now);
                    hotkey.tapped_at = None;
                    hotkey.stop_on_release = false;
                    Action::Start { by: Some(shortcut) }
                }
                // Either shortcut stops, as a second press, but not while
                // the other is still down from starting the recording.
                Activity::Recording { .. } if !hold => {
                    if hotkey.down(shortcut.other()) {
                        Action::Nothing
                    } else {
                        Action::Stop { cancelled: false }
                    }
                }
                Activity::Recording { by, .. } if by != Some(shortcut) => Action::Nothing,
                Activity::Recording { locked: true, .. } => {
                    hotkey.pressed_at = Some(now);
                    hotkey.stop_on_release = true;
                    Action::Nothing
                }
                Activity::Recording { .. } => {
                    hotkey.pressed_at = Some(now);
                    if within_tap(hotkey.tapped_at.take()) {
                        Action::Lock
                    } else {
                        Action::Nothing
                    }
                }
                Activity::Transcribing => Action::Nothing,
            }
        }
        Input::HotkeyUp(shortcut) => {
            let was_down = hotkey.set(shortcut, false);
            if !(was_down && hold) {
                return Action::Nothing;
            }
            match activity {
                // Started from the UI: either shortcut's release, once both are up.
                Activity::Recording { by: None, .. } if hotkey.down(shortcut.other()) => {
                    Action::Nothing
                }
                Activity::Recording { by: None, .. } => Action::Stop { cancelled: false },
                Activity::Recording { by, .. } if by != Some(shortcut) => Action::Nothing,
                // Releasing the second tap, which locked, does nothing.
                Activity::Recording { locked: true, .. } => {
                    if mem::take(&mut hotkey.stop_on_release) {
                        Action::Stop { cancelled: false }
                    } else {
                        Action::Nothing
                    }
                }
                // Hybrid mode: a quick tap keeps recording until the next press.
                Activity::Recording { .. }
                    if within_tap(hotkey.pressed_at) && mode == RecordingMode::Hybrid =>
                {
                    Action::Lock
                }
                Activity::Recording { .. } if within_tap(hotkey.pressed_at) => {
                    hotkey.tapped_at = Some(now);
                    Action::AwaitSecondTap
                }
                Activity::Recording { .. } => Action::Stop { cancelled: false },
                _ => Action::Nothing,
            }
        }
        Input::HotkeyInterrupted => {
            hotkey.set(Shortcut::Dictation, false);
            // The hotkey was part of a shortcut like ⌘C. A recording it started
            // wasn't a dictation, but one started from the UI or by the command
            // shortcut, or locked on, carries on.
            match activity {
                Activity::Recording {
                    by: Some(Shortcut::Dictation),
                    locked,
                } => {
                    hotkey.tapped_at = None;
                    hotkey.stop_on_release = false;
                    if hold && !locked {
                        Action::Discard
                    } else {
                        Action::Nothing
                    }
                }
                _ => Action::Nothing,
            }
        }
        // Only the timer for the latest tap counts: an older one finds it too recent.
        Input::TapExpired => match activity {
            Activity::Recording {
                by: Some(by),
                locked: false,
            } if hold
                && !hotkey.down(by)
                && hotkey.tapped_at.is_some_and(|t| now - t >= DOUBLE_TAP) =>
            {
                hotkey.tapped_at = None;
                Action::Stop { cancelled: false }
            }
            _ => Action::Nothing,
        },
        Input::Toggle => match activity {
            Activity::Idle => Action::Start { by: None },
            Activity::Recording { .. } => Action::Stop { cancelled: false },
            Activity::Transcribing => Action::Nothing,
        },
        Input::Send => match activity {
            // Also with the hotkey held: hold, speak, then Return to send.
            Activity::Recording { .. } => Action::Send,
            _ => Action::Nothing,
        },
        Input::Escape => match activity {
            // Still transcribed and saved to history, but not pasted.
            Activity::Recording { .. } => Action::Stop { cancelled: true },
            // The transcription finishes in the background and lands in history.
            Activity::Transcribing => Action::CancelTranscription,
            Activity::Idle => Action::Nothing,
        },
    }
}

enum Phase {
    Idle,
    Recording {
        recording: Recording,
        by: Option<Shortcut>,
        locked: bool,
        started_at_ms: u64,
        session: u64,
        live: Option<live::Handle>,
        /// Transcribing behind the speaker, when the text is pasted at the end.
        prefetch: Option<stream::Prefetch>,
    },
    Transcribing {
        session: u64,
        /// Escape was pressed while recording.
        cancelled: bool,
        live: Option<Arc<AtomicBool>>,
    },
    /// A short status message before returning to idle.
    Message {
        generation: u64,
    },
}

impl Phase {
    fn activity(&self) -> Activity {
        match self {
            Phase::Idle | Phase::Message { .. } => Activity::Idle,
            Phase::Recording { by, locked, .. } => Activity::Recording {
                by: *by,
                locked: *locked,
            },
            Phase::Transcribing { .. } => Activity::Transcribing,
        }
    }
}

#[derive(Clone)]
pub struct Controller {
    tx: Sender<Event>,
}

impl Controller {
    pub fn spawn(app: AppHandle) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let controller = Self { tx };
        let mut session = Session {
            app,
            controller: controller.clone(),
            paster: Paster::spawn(),
            playback: Playback::spawn(),
            phase: Phase::Idle,
            hotkey: Hotkey::default(),
            owns_cancel_key: false,
            owns_send_key: false,
            sending: None,
            next_id: 0,
            typing_paused: false,
            command_session: None,
            target: None,
        };
        thread::Builder::new()
            .name("dictation".into())
            .spawn(move || {
                for event in rx {
                    // A bug in one event shouldn't end dictation for the rest of the run.
                    let handled = panic::catch_unwind(AssertUnwindSafe(|| session.handle(event)));
                    if handled.is_err() {
                        eprintln!("[dictation] event handler panicked");
                    }
                }
            })?;
        Ok(controller)
    }

    pub fn send(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    /// Registers the user's hotkey, replacing `previous` if given. Single
    /// modifier keys like "Fn" use the platform's own listener.
    ///
    /// Waits on the main thread, and must not be called from a shortcut
    /// handler: the plugin holds its lock while handlers run.
    pub fn register_hotkey(
        &self,
        app: &AppHandle,
        hotkey: &str,
        previous: Option<&str>,
    ) -> Result<(), String> {
        let shortcuts = app.global_shortcut();
        if let Some(previous) = previous {
            if platform::MODIFIER_HOTKEYS.contains(&previous) {
                platform::stop_modifier_hotkey();
            } else {
                let _ = shortcuts.unregister(previous);
            }
        }

        let controller = self.clone();
        if platform::MODIFIER_HOTKEYS.contains(&hotkey) {
            return platform::start_modifier_hotkey(
                hotkey,
                Box::new(move |event| {
                    controller.send(match event {
                        HotkeyEvent::Down => Event::HotkeyDown,
                        HotkeyEvent::Up => Event::HotkeyUp,
                        HotkeyEvent::Interrupted => Event::HotkeyInterrupted,
                        HotkeyEvent::Send => Event::Send,
                    })
                }),
            );
        }
        shortcuts
            .on_shortcut(hotkey, move |_, _, event| {
                controller.send(match event.state {
                    ShortcutState::Pressed => Event::HotkeyDown,
                    ShortcutState::Released => Event::HotkeyUp,
                });
            })
            .map_err(|e| format!("Couldn't register {hotkey}: {e}"))
    }
}

struct Session {
    app: AppHandle,
    controller: Controller,
    paster: Paster,
    playback: Playback,
    phase: Phase,
    hotkey: Hotkey,
    /// Whether this session registered Escape, as opposed to the user's hotkey being Escape.
    owns_cancel_key: bool,
    /// Whether this session registered Return and keypad Enter, for Finish and send.
    owns_send_key: bool,
    /// The session finished with Return, to be sent once it's typed.
    sending: Option<u64>,
    next_id: u64,
    typing_paused: bool,
    /// The session recording or running a command, if any.
    command_session: Option<u64>,
    /// The app being dictated into, being found since recording started.
    target: Option<tone::Detection>,
}

impl Session {
    fn state(&self) -> tauri::State<'_, AppState> {
        self.app.state::<AppState>()
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::HotkeyDown => self.input(Input::HotkeyDown(Shortcut::Dictation)),
            Event::HotkeyUp => self.input(Input::HotkeyUp(Shortcut::Dictation)),
            Event::HotkeyInterrupted => self.input(Input::HotkeyInterrupted),
            // The same rules as the dictation hotkey; only what's recorded differs.
            Event::CommandHotkeyDown => self.input(Input::HotkeyDown(Shortcut::Command)),
            Event::CommandHotkeyUp => self.input(Input::HotkeyUp(Shortcut::Command)),
            Event::Toggle => self.input(Input::Toggle),
            Event::Escape => self.input(Input::Escape),
            Event::Send => self.input(Input::Send),
            Event::PasteLast => self.paste_last(),
            Event::Stage { session, stage } => self.stage(session, stage),
            Event::Transcribed { session, outcome } => self.transcribed(session, outcome),
            Event::LiveTypingPaused { session } => {
                if matches!(self.phase, Phase::Recording { session: s, .. } | Phase::Transcribing { session: s, .. } if s == session)
                {
                    self.typing_paused = true;
                }
            }
            Event::TapExpired { session } => {
                if matches!(self.phase, Phase::Recording { session: s, .. } if s == session) {
                    self.input(Input::TapExpired);
                }
            }
            Event::PauseMedia { session } => {
                if let Phase::Recording {
                    session: s,
                    recording,
                    ..
                } = &self.phase
                    && *s == session
                {
                    self.playback.pause(recording.monitor());
                }
            }
            Event::MessageExpired { generation } => {
                if matches!(self.phase, Phase::Message { generation: g } if g == generation) {
                    self.go_idle();
                }
            }
            Event::RefreshPill => {
                if matches!(self.phase, Phase::Idle) {
                    pill::place(&self.app, self.state().settings().pill_position);
                    self.go_idle();
                }
            }
            // Never interrupts a dictation; the words are in Dictionary either way.
            Event::Learned { words } => {
                if matches!(self.phase, Phase::Idle | Phase::Message { .. }) {
                    self.flash(&learned_message(&words), LEARNED_MESSAGE);
                }
            }
        }
    }

    fn input(&mut self, input: Input) {
        let mode = self.state().settings().recording_mode;
        let activity = self.phase.activity();
        match decide(input, activity, mode, &mut self.hotkey, Instant::now()) {
            Action::Nothing => {}
            Action::Start { by } => self.start(by),
            Action::Stop { cancelled } => self.stop(cancelled),
            Action::Discard => self.go_idle(),
            Action::CancelTranscription => self.flash(STOPPING, STOPPING_MESSAGE),
            Action::AwaitSecondTap => self.await_second_tap(),
            Action::Lock => self.lock(),
            Action::Send => {
                if let Phase::Recording { session, .. } = self.phase {
                    self.sending = Some(session);
                }
                self.stop(false);
            }
        }
    }

    /// Pastes the latest transcript into the focused app. Not while a
    /// dictation is under way, which pastes its own.
    fn paste_last(&mut self) {
        if !matches!(self.phase, Phase::Idle | Phase::Message { .. }) {
            return;
        }
        let (last, restore) = {
            let state = self.state();
            let last = state
                .history
                .lock_unpoisoned()
                .items()
                .first()
                .map(|item| item.transcript.clone());
            (last, state.settings().restore_clipboard)
        };
        match last {
            Some(text) => self.paster.paste(text, restore, false),
            None => self.flash("Nothing to paste yet", ERROR_MESSAGE),
        }
    }

    /// Keeps recording after a quick tap, and stops if no second tap follows.
    fn await_second_tap(&mut self) {
        let Phase::Recording { session, .. } = self.phase else {
            return;
        };
        let controller = self.controller.clone();
        let timer = thread::Builder::new()
            .name("double-tap".into())
            .spawn(move || {
                thread::sleep(DOUBLE_TAP);
                controller.send(Event::TapExpired { session });
            });
        if let Err(e) = timer {
            eprintln!("[dictation] {e}");
            self.stop(false);
        }
    }

    fn lock(&mut self) {
        let Phase::Recording {
            ref mut locked,
            started_at_ms,
            ..
        } = self.phase
        else {
            return;
        };
        *locked = true;
        self.show(PillState::Recording {
            started_at_ms,
            locked: true,
        });
    }

    fn start(&mut self, by: Option<Shortcut>) {
        let by_hotkey = by.is_some();
        let settings = self.state().settings();
        if settings.selected_model.is_empty() {
            return self.flash("No model selected", ERROR_MESSAGE);
        }
        if !self.state().models.is_downloaded(&settings.selected_model) {
            return self.flash("Model not downloaded", ERROR_MESSAGE);
        }
        let command = by == Some(Shortcut::Command);
        if command && let Some(reason) = command::unavailable(&self.app) {
            return self.flash(reason, ERROR_MESSAGE);
        }

        let app = self.app.clone();
        let recording = Recording::start(&settings.input_device, move |level| {
            let _ = app.emit_to(pill::LABEL, "pill-level", level);
        });
        match recording {
            Ok(recording) => {
                self.next_id += 1;
                let session = self.next_id;
                self.typing_paused = false;
                // Held shortcut modifiers interfere with typing on some systems.
                // Fn events are swallowed on macOS; other shortcuts use Toggle.
                let compatible =
                    !by_hotkey || !settings.recording_mode.holds() || settings.hotkey == "Fn";
                let live = if settings.live_typing && compatible && !command {
                    platform::typing_target().and_then(|target| {
                        live::Handle::spawn(
                            self.app.clone(),
                            self.controller.clone(),
                            session,
                            recording.monitor(),
                            settings.clone(),
                            target,
                        )
                        .map_err(|e| eprintln!("[live] couldn't start worker: {e}"))
                        .ok()
                    })
                } else {
                    None
                };
                let prefetch = if settings.transcribe_while_speaking && live.is_none() && !command {
                    stream::Prefetch::spawn(self.app.clone(), recording.monitor(), settings.clone())
                        .map_err(|e| eprintln!("[stream] couldn't start worker: {e}"))
                        .ok()
                } else {
                    None
                };
                self.command_session = command.then_some(session);
                self.target = Some(tone::Detection::start());
                let started_at_ms = now_ms();
                self.phase = Phase::Recording {
                    recording,
                    by,
                    locked: false,
                    started_at_ms,
                    session,
                    live,
                    prefetch,
                };
                self.set_cancel_key(true);
                // An edit replaces the selection, so there's nothing to send.
                self.set_send_key(settings.finish_and_send && !command);
                self.show(PillState::Recording {
                    started_at_ms,
                    locked: false,
                });
                if settings.pause_media {
                    let controller = self.controller.clone();
                    let timer =
                        thread::Builder::new()
                            .name("pause-media".into())
                            .spawn(move || {
                                thread::sleep(PAUSE_MEDIA_AFTER);
                                controller.send(Event::PauseMedia { session });
                            });
                    if let Err(e) = timer {
                        eprintln!("[dictation] {e}");
                    }
                }
                self.broadcast(DictationState::Recording { started_at_ms });
                // Load the models while the user speaks, so they're ready on release.
                self.state().warm_up(&self.app, &settings.selected_model);
                let state = self.state();
                state.cleaner.warm_up(&settings.cleanup, &state.models);
                cleanup::warm_up_server(&settings.cleanup, None);
            }
            Err(e) => {
                eprintln!("[dictation] {e}");
                self.flash("Microphone unavailable", ERROR_MESSAGE);
            }
        }
    }

    fn stop(&mut self, cancelled: bool) {
        let Phase::Recording {
            recording,
            session,
            live,
            prefetch,
            ..
        } = mem::replace(&mut self.phase, Phase::Idle)
        else {
            return;
        };
        let target = self.target.take();
        self.set_send_key(false);
        if cancelled && let Some(handle) = &live {
            handle.cancelled.store(true, Ordering::SeqCst);
        }
        // Show the release right away; closing the microphone takes a moment.
        let message = if cancelled { STOPPING } else { TRANSCRIBING };
        self.show(PillState::Processing {
            message: message.into(),
        });
        self.broadcast(DictationState::Transcribing);
        let finished = recording.finish();
        // Once the microphone is closed, so it doesn't hear the music start.
        self.playback.resume();
        let captured = match finished {
            Ok(captured) => captured,
            Err(e) => {
                eprintln!("[dictation] {e}");
                return self.flash("Recording failed", ERROR_MESSAGE);
            }
        };

        let token = live.as_ref().map(|handle| handle.cancelled.clone());
        self.phase = Phase::Transcribing {
            session,
            cancelled,
            live: token,
        };

        let captured = if let Some(handle) = live {
            match handle.finish(captured, cancelled) {
                Ok(_) => return,
                // If the worker crashed, preserve the full recording in history.
                // This remains a live session, so it will never paste a duplicate.
                Err(captured) => {
                    self.typing_paused = true;
                    captured
                }
            }
        } else {
            captured
        };

        // A cancelled command has nothing to keep: what was said only made sense with the edit.
        if cancelled && self.command_session == Some(session) {
            return self.go_idle();
        }
        let app = self.app.clone();
        let controller = self.controller.clone();
        let command = (self.command_session == Some(session)).then(|| self.paster.clone());
        let worker = thread::Builder::new()
            .name("transcription".into())
            .spawn(move || {
                // Waited for here, off the controller thread.
                let target = target.and_then(tone::Detection::app);
                let outcome = transcribe(
                    &app,
                    &controller,
                    session,
                    &captured,
                    target.as_ref(),
                    command.as_ref(),
                    prefetch,
                );
                controller.send(Event::Transcribed { session, outcome });
            });
        if let Err(e) = worker {
            let error = pipeline::Error::Transcribe(format!("Couldn't start transcription: {e}"));
            self.transcribed(session, Err(error));
        }
    }

    fn stage(&mut self, session: u64, stage: pipeline::Stage) {
        // After Escape or a newer dictation the pill has moved on.
        let current = matches!(
            self.phase,
            Phase::Transcribing { session: s, cancelled: false, .. } if s == session
        );
        if !current {
            return;
        }
        self.show(match stage {
            pipeline::Stage::Loading => PillState::Warming,
            pipeline::Stage::Transcribing => PillState::Processing {
                message: TRANSCRIBING.into(),
            },
            pipeline::Stage::CleaningUp => PillState::Processing {
                message: CLEANING_UP.into(),
            },
            pipeline::Stage::Editing => PillState::Processing {
                message: EDITING.into(),
            },
        });
    }

    fn transcribed(&mut self, session: u64, outcome: Outcome) {
        let Phase::Transcribing {
            session: current,
            cancelled,
            ref live,
        } = self.phase
        else {
            return;
        };
        if current != session {
            return;
        }
        let streamed = live.is_some();
        let send = self.sending.take() == Some(session);

        match outcome {
            Ok(text) => {
                let paused = self.typing_paused;
                self.go_idle();
                if !cancelled && !streamed {
                    let restore = self.state().settings().restore_clipboard;
                    // An edit isn't a dictation to learn corrections from, and
                    // a sent message leaves the field.
                    if self.command_session != Some(session) && !send {
                        learn::watch_for_corrections(&self.app, &text);
                    }
                    self.paster.paste(text, restore, send);
                }
                // Live typing has put the text in already, unless the user moved
                // to another app, where Return would go somewhere unexpected.
                if !cancelled && streamed && send && !paused {
                    self.paster.send();
                }
                if paused && !cancelled {
                    self.flash("Typing paused. Saved in History.", ERROR_MESSAGE);
                }
            }
            Err(e) => {
                if let Some(detail) = e.detail() {
                    eprintln!("[dictation] {detail}");
                }
                let duration = match e {
                    pipeline::Error::NoSpeech => NO_SPEECH_MESSAGE,
                    _ => ERROR_MESSAGE,
                };
                self.flash(e.message(), duration);
            }
        }
    }

    /// Shows a status message in the pill, then returns to idle.
    fn flash(&mut self, message: &str, duration: Duration) {
        self.cancel_live_typing();
        self.next_id += 1;
        let generation = self.next_id;
        self.phase = Phase::Message { generation };
        self.set_cancel_key(false);
        self.set_send_key(false);
        self.show(PillState::Processing {
            message: message.into(),
        });
        self.broadcast(DictationState::Idle);
        let controller = self.controller.clone();
        let timer = thread::Builder::new()
            .name("pill-message".into())
            .spawn(move || {
                thread::sleep(duration);
                controller.send(Event::MessageExpired { generation });
            });
        if let Err(e) = timer {
            eprintln!("[dictation] {e}");
            self.go_idle();
        }
    }

    /// Returns to idle. Dropping a recording stops the microphone and discards its audio.
    fn go_idle(&mut self) {
        self.cancel_live_typing();
        // After a discarded recording; does nothing otherwise.
        self.playback.resume();
        self.phase = Phase::Idle;
        self.set_cancel_key(false);
        self.set_send_key(false);
        self.show(PillState::Idle);
        self.broadcast(DictationState::Idle);
    }

    fn cancel_live_typing(&self) {
        if let Phase::Transcribing {
            live: Some(token), ..
        } = &self.phase
        {
            token.store(true, Ordering::SeqCst);
        }
    }

    fn show(&self, state: PillState) {
        let settings = self.state().settings();
        pill::update(
            &self.app,
            state,
            settings.pill_position,
            settings.always_show_pill,
        );
        // The pill takes clicks only while recording, so its controls work.
        pill::set_interactive(&self.app, matches!(self.phase, Phase::Recording { .. }));
    }

    fn broadcast(&self, state: DictationState) {
        let _ = self.app.emit("dictation-state", &state);
        crate::tray::show_state(&self.app, &state);
        *self.state().dictation_state.lock_unpoisoned() = state;
    }

    /// Escape is only claimed while a dictation is active, so other apps keep it otherwise.
    fn set_cancel_key(&mut self, active: bool) {
        if active == self.owns_cancel_key {
            return;
        }
        let shortcuts = self.app.global_shortcut();
        if !active {
            let _ = shortcuts.unregister(CANCEL_KEY);
            self.owns_cancel_key = false;
            return;
        }
        // Escape is the user's hotkey, which must stay registered.
        if shortcuts.is_registered(CANCEL_KEY) {
            return;
        }
        let controller = self.controller.clone();
        let result = shortcuts.on_shortcut(CANCEL_KEY, move |_, _, event| {
            if event.state == ShortcutState::Pressed {
                controller.send(Event::Escape);
            }
        });
        match result {
            Ok(()) => self.owns_cancel_key = true,
            Err(e) => eprintln!("[dictation] couldn't register Escape: {e}"),
        }
    }

    /// Return is only claimed while recording, so it types as usual otherwise.
    fn set_send_key(&mut self, active: bool) {
        if active == self.owns_send_key {
            return;
        }
        let shortcuts = self.app.global_shortcut();
        if !active {
            unregister_send_keys(shortcuts);
            platform::set_modifier_hotkey_sends(false);
            self.owns_send_key = false;
            return;
        }
        if SEND_KEYS.iter().any(|key| shortcuts.is_registered(*key)) {
            return;
        }
        let controller = self.controller.clone();
        let result = shortcuts.on_shortcuts(SEND_KEYS.iter().copied(), move |_, _, event| {
            if event.state == ShortcutState::Pressed {
                controller.send(Event::Send);
            }
        });
        match result {
            Ok(()) => {
                platform::set_modifier_hotkey_sends(true);
                self.owns_send_key = true;
            }
            Err(e) => {
                // One may have registered before the other failed.
                unregister_send_keys(shortcuts);
                eprintln!("[dictation] couldn't register Return: {e}");
            }
        }
    }
}

/// Registers the shortcut that pastes the latest transcript, replacing
/// `previous`. An empty shortcut turns it off.
pub fn register_paste_last_hotkey(
    app: &AppHandle,
    controller: &Controller,
    hotkey: &str,
    previous: Option<&str>,
) -> Result<(), String> {
    let shortcuts = app.global_shortcut();
    if let Some(previous) = previous.map(str::trim).filter(|p| !p.is_empty()) {
        let _ = shortcuts.unregister(previous);
    }
    let hotkey = hotkey.trim();
    if hotkey.is_empty() {
        return Ok(());
    }
    if platform::MODIFIER_HOTKEYS.contains(&hotkey) {
        return Err("The paste shortcut needs a regular key, like Ctrl+Shift+V".into());
    }
    let controller = controller.clone();
    shortcuts
        .on_shortcut(hotkey, move |_, _, event| {
            if event.state == ShortcutState::Pressed {
                controller.send(Event::PasteLast);
            }
        })
        .map_err(|e| format!("Couldn't register {hotkey}: {e}"))
}

/// One at a time, so a key that isn't registered doesn't keep the others.
fn unregister_send_keys(shortcuts: &GlobalShortcut<tauri::Wry>) {
    for key in SEND_KEYS {
        if shortcuts.is_registered(*key) {
            let _ = shortcuts.unregister(*key);
        }
    }
}

/// Runs on a worker thread. Stage changes go through the controller, which
/// drops them once the session has moved on. With a `command` paster, what
/// was said is an edit for the selected text.
fn transcribe(
    app: &AppHandle,
    controller: &Controller,
    session: u64,
    captured: &Captured,
    target: Option<&FrontApp>,
    command: Option<&Paster>,
    prefetch: Option<stream::Prefetch>,
) -> Outcome {
    let on_stage = |stage| controller.send(Event::Stage { session, stage });
    // Without an outcome the session would stay transcribing until Escape.
    panic::catch_unwind(AssertUnwindSafe(|| match command {
        Some(paster) => {
            let rules = app.state::<AppState>().settings().tone.apps;
            let category = target.map_or(tone::AppCategory::Other, |t| tone::classify(t, &rules));
            command::run(
                app,
                paster,
                &captured.samples,
                captured.duration_secs,
                category,
                on_stage,
            )
        }
        None => pipeline::run(
            app,
            &captured.samples,
            captured.duration_secs,
            target,
            prefetch,
            on_stage,
        )
        .map(|item| item.transcript),
    }))
    .unwrap_or_else(|_| Err(pipeline::Error::Transcribe("The engine crashed".into())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DICTATION: Shortcut = Shortcut::Dictation;
    const COMMAND: Shortcut = Shortcut::Command;
    const STOP: Action = Action::Stop { cancelled: false };
    const START: Action = Action::Start {
        by: Some(DICTATION),
    };

    fn recording(by: Option<Shortcut>) -> Activity {
        Activity::Recording { by, locked: false }
    }

    /// Runs `inputs` from `activity`, a second apart so none of them is a
    /// quick tap, returning each action.
    fn run(activity: Activity, mode: RecordingMode, inputs: &[Input]) -> Vec<Action> {
        let mut hotkey = Hotkey::default();
        steps(activity, mode, &mut hotkey, inputs)
    }

    /// Like [`run`], with the shortcuts in `hotkey` already held.
    fn steps(
        activity: Activity,
        mode: RecordingMode,
        hotkey: &mut Hotkey,
        inputs: &[Input],
    ) -> Vec<Action> {
        let start = Instant::now();
        inputs
            .iter()
            .enumerate()
            .map(|(i, &input)| {
                let now = start + Duration::from_secs(i as u64 + 1);
                decide(input, activity, mode, hotkey, now)
            })
            .collect()
    }

    fn holding(shortcut: Shortcut) -> Hotkey {
        let mut hotkey = Hotkey::default();
        hotkey.set(shortcut, true);
        hotkey
    }

    /// Runs `inputs`, each at its time in milliseconds, from idle in Hold mode,
    /// moving between activities as the session would. Returns each action.
    fn hold(inputs: &[(u64, Input)]) -> Vec<Action> {
        timed(RecordingMode::Hold, inputs)
    }

    /// Like [`hold`], in Hybrid mode.
    fn hybrid(inputs: &[(u64, Input)]) -> Vec<Action> {
        timed(RecordingMode::Hybrid, inputs)
    }

    fn timed(mode: RecordingMode, inputs: &[(u64, Input)]) -> Vec<Action> {
        let mut hotkey = Hotkey::default();
        let mut activity = Activity::Idle;
        let start = Instant::now();
        inputs
            .iter()
            .map(|&(ms, input)| {
                let now = start + Duration::from_millis(ms);
                let action = decide(input, activity, mode, &mut hotkey, now);
                activity = match (action, activity) {
                    (Action::Start { by }, _) => recording(by),
                    (Action::Stop { .. } | Action::Send, _) => Activity::Transcribing,
                    (Action::Discard, _) => Activity::Idle,
                    (Action::Lock, Activity::Recording { by, .. }) => {
                        Activity::Recording { by, locked: true }
                    }
                    (_, activity) => activity,
                };
                action
            })
            .collect()
    }

    #[test]
    fn hold_mode_records_while_the_hotkey_is_held() {
        let mut hotkey = Hotkey::default();
        let mode = RecordingMode::Hold;
        let start = Instant::now();
        let down = Input::HotkeyDown(DICTATION);
        assert_eq!(
            decide(down, Activity::Idle, mode, &mut hotkey, start),
            START
        );
        let recording = recording(Some(DICTATION));
        // Key repeat while held does nothing.
        let later = start + Duration::from_millis(500);
        assert_eq!(
            decide(down, recording, mode, &mut hotkey, later),
            Action::Nothing
        );
        let released = start + Duration::from_secs(3);
        assert_eq!(
            decide(
                Input::HotkeyUp(DICTATION),
                recording,
                mode,
                &mut hotkey,
                released
            ),
            STOP
        );
        assert!(!hotkey.down(DICTATION) && !hotkey.down(COMMAND));
    }

    #[test]
    fn a_double_tap_locks_and_the_next_press_stops_on_release() {
        use Input::*;
        for shortcut in [DICTATION, COMMAND] {
            assert_eq!(
                hold(&[
                    (0, HotkeyDown(shortcut)),
                    (120, HotkeyUp(shortcut)),
                    (300, HotkeyDown(shortcut)),
                    (380, HotkeyUp(shortcut)),
                    // Hands free: the old tap timer finds the lock and does nothing.
                    (470, TapExpired),
                    (8_000, HotkeyDown(shortcut)),
                    (8_090, HotkeyUp(shortcut)),
                ]),
                [
                    Action::Start { by: Some(shortcut) },
                    Action::AwaitSecondTap,
                    Action::Lock,
                    Action::Nothing,
                    Action::Nothing,
                    Action::Nothing,
                    STOP
                ]
            );
        }
    }

    #[test]
    fn hybrid_mode_locks_on_a_tap_and_stops_on_the_next_press() {
        use Input::*;
        for shortcut in [DICTATION, COMMAND] {
            assert_eq!(
                hybrid(&[
                    (0, HotkeyDown(shortcut)),
                    (150, HotkeyUp(shortcut)),
                    (6_000, HotkeyDown(shortcut)),
                    (6_100, HotkeyUp(shortcut)),
                ]),
                [
                    Action::Start { by: Some(shortcut) },
                    Action::Lock,
                    Action::Nothing,
                    STOP
                ]
            );
        }
    }

    #[test]
    fn hybrid_mode_holds_to_talk_on_a_long_press() {
        use Input::*;
        assert_eq!(
            hybrid(&[(0, HotkeyDown(DICTATION)), (2_000, HotkeyUp(DICTATION))]),
            [START, STOP]
        );
        // A shortcut like ⌘C, from a single-modifier hotkey, is still discarded.
        assert_eq!(
            hybrid(&[(0, HotkeyDown(DICTATION)), (80, HotkeyInterrupted)]),
            [START, Action::Discard]
        );
        // Only the shortcut that started the recording locks it.
        assert_eq!(
            hybrid(&[
                (0, HotkeyDown(DICTATION)),
                (50, HotkeyDown(COMMAND)),
                (120, HotkeyUp(COMMAND)),
                (3_000, HotkeyUp(DICTATION)),
            ]),
            [START, Action::Nothing, Action::Nothing, STOP]
        );
    }

    #[test]
    fn a_single_quick_tap_stops_once_the_second_tap_would_be_too_late() {
        use Input::*;
        assert_eq!(
            hold(&[
                (0, HotkeyDown(DICTATION)),
                (100, HotkeyUp(DICTATION)),
                (450, TapExpired)
            ]),
            [START, Action::AwaitSecondTap, STOP]
        );
        // A timer that fires early, for an older tap, is ignored.
        assert_eq!(
            hold(&[
                (0, HotkeyDown(DICTATION)),
                (100, HotkeyUp(DICTATION)),
                (200, TapExpired),
                (460, TapExpired)
            ]),
            [START, Action::AwaitSecondTap, Action::Nothing, STOP]
        );
    }

    #[test]
    fn slow_presses_and_long_holds_dont_lock() {
        use Input::*;
        // Held too long to be a tap: an ordinary hold-to-talk.
        assert_eq!(
            hold(&[(0, HotkeyDown(DICTATION)), (400, HotkeyUp(DICTATION))]),
            [START, STOP]
        );
        // The second press comes too late after the tap.
        assert_eq!(
            hold(&[
                (0, HotkeyDown(DICTATION)),
                (100, HotkeyUp(DICTATION)),
                (500, HotkeyDown(DICTATION))
            ]),
            [START, Action::AwaitSecondTap, Action::Nothing]
        );
        // ...and it behaves as a hold from then on.
        assert_eq!(
            hold(&[
                (0, HotkeyDown(DICTATION)),
                (100, HotkeyUp(DICTATION)),
                (500, HotkeyDown(DICTATION)),
                (600, TapExpired),
                (2_000, HotkeyUp(DICTATION))
            ])[3..],
            [Action::Nothing, STOP]
        );
    }

    #[test]
    fn a_shortcut_while_locked_keeps_recording() {
        use Input::*;
        assert_eq!(
            hold(&[
                (0, HotkeyDown(DICTATION)),
                (100, HotkeyUp(DICTATION)),
                (200, HotkeyDown(DICTATION)),
                (250, HotkeyUp(DICTATION)),
                // ⌘C with ⌘ as the hotkey.
                (5_000, HotkeyDown(DICTATION)),
                (5_050, HotkeyInterrupted),
                (5_100, HotkeyUp(DICTATION)),
                (9_000, HotkeyDown(DICTATION)),
                (9_100, HotkeyUp(DICTATION)),
            ])[4..],
            [
                Action::Nothing,
                Action::Nothing,
                Action::Nothing,
                Action::Nothing,
                STOP
            ]
        );
    }

    #[test]
    fn the_other_shortcut_neither_locks_nor_stops_a_hold() {
        use Input::*;
        // A quick dictation tap, then the command shortcut within the tap window.
        assert_eq!(
            hold(&[
                (0, HotkeyDown(DICTATION)),
                (100, HotkeyUp(DICTATION)),
                (200, HotkeyDown(COMMAND)),
                (250, HotkeyUp(COMMAND)),
                (460, TapExpired),
            ]),
            [
                START,
                Action::AwaitSecondTap,
                Action::Nothing,
                Action::Nothing,
                STOP
            ]
        );
        // Locked: the other shortcut doesn't stop it.
        assert_eq!(
            hold(&[
                (0, HotkeyDown(DICTATION)),
                (100, HotkeyUp(DICTATION)),
                (200, HotkeyDown(DICTATION)),
                (250, HotkeyUp(DICTATION)),
                (3_000, HotkeyDown(COMMAND)),
                (3_100, HotkeyUp(COMMAND)),
            ])[4..],
            [Action::Nothing, Action::Nothing]
        );
    }

    #[test]
    fn taps_dont_lock_recordings_from_the_ui_or_in_toggle_mode() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut hotkey = Hotkey::default();
        let from_ui = recording(None);
        let mode = RecordingMode::Hold;
        assert_eq!(
            decide(
                Input::HotkeyDown(DICTATION),
                from_ui,
                mode,
                &mut hotkey,
                at(0)
            ),
            Action::Nothing
        );
        assert_eq!(
            decide(
                Input::HotkeyUp(DICTATION),
                from_ui,
                mode,
                &mut hotkey,
                at(100)
            ),
            STOP
        );

        let mut hotkey = Hotkey::default();
        let mode = RecordingMode::Toggle;
        assert_eq!(
            decide(
                Input::HotkeyDown(DICTATION),
                Activity::Idle,
                mode,
                &mut hotkey,
                at(0)
            ),
            START
        );
        let recording = recording(Some(DICTATION));
        assert_eq!(
            decide(
                Input::HotkeyUp(DICTATION),
                recording,
                mode,
                &mut hotkey,
                at(100)
            ),
            Action::Nothing
        );
        assert_eq!(
            decide(
                Input::HotkeyDown(DICTATION),
                recording,
                mode,
                &mut hotkey,
                at(200)
            ),
            STOP
        );
    }

    #[test]
    fn toggle_mode_stops_on_the_next_press() {
        let mode = RecordingMode::Toggle;
        for shortcut in [DICTATION, COMMAND] {
            let inputs = [Input::HotkeyDown(shortcut), Input::HotkeyUp(shortcut)];
            assert_eq!(
                run(recording(Some(shortcut)), mode, &inputs),
                [STOP, Action::Nothing]
            );
            assert_eq!(run(Activity::Idle, mode, &inputs)[1], Action::Nothing);
        }
    }

    #[test]
    fn a_release_without_a_press_is_ignored() {
        assert_eq!(
            run(
                recording(None),
                RecordingMode::Hold,
                &[Input::HotkeyUp(DICTATION)]
            ),
            [Action::Nothing]
        );
    }

    #[test]
    fn a_shortcut_discards_only_recordings_the_hotkey_started() {
        let mode = RecordingMode::Hold;
        let inputs = [
            Input::HotkeyDown(DICTATION),
            Input::HotkeyInterrupted,
            Input::HotkeyUp(DICTATION),
        ];
        assert_eq!(
            run(recording(Some(DICTATION)), mode, &inputs),
            [Action::Nothing, Action::Discard, Action::Nothing]
        );
        // Recording from the tray, then pressing ⌘Tab with ⌘ as the hotkey.
        assert_eq!(
            run(recording(None), mode, &inputs),
            [Action::Nothing, Action::Nothing, Action::Nothing]
        );
        // A command isn't the dictation hotkey's to throw away.
        assert_eq!(
            run(recording(Some(COMMAND)), mode, &inputs),
            [Action::Nothing, Action::Nothing, Action::Nothing]
        );
        assert_eq!(
            run(
                recording(Some(DICTATION)),
                RecordingMode::Toggle,
                &[Input::HotkeyInterrupted]
            ),
            [Action::Nothing]
        );
    }

    #[test]
    fn the_command_shortcut_starts_a_command() {
        for mode in [RecordingMode::Hold, RecordingMode::Toggle] {
            assert_eq!(
                run(Activity::Idle, mode, &[Input::HotkeyDown(COMMAND)]),
                [Action::Start { by: Some(COMMAND) }]
            );
        }
        assert_eq!(
            run(
                recording(Some(COMMAND)),
                RecordingMode::Hold,
                &[Input::HotkeyDown(COMMAND), Input::HotkeyUp(COMMAND)]
            ),
            [Action::Nothing, STOP]
        );
    }

    #[test]
    fn in_hold_mode_only_the_shortcut_that_started_stops() {
        let mode = RecordingMode::Hold;
        for (started, other) in [(DICTATION, COMMAND), (COMMAND, DICTATION)] {
            // The starting shortcut is held, and the other one pressed and released.
            let activity = recording(Some(started));
            assert_eq!(
                steps(
                    activity,
                    mode,
                    &mut holding(started),
                    &[
                        Input::HotkeyDown(other),
                        Input::HotkeyUp(other),
                        Input::HotkeyUp(started)
                    ]
                ),
                [Action::Nothing, Action::Nothing, STOP]
            );
            // A stray release of the other shortcut does nothing either.
            assert_eq!(
                run(activity, mode, &[Input::HotkeyUp(other)]),
                [Action::Nothing]
            );
        }
    }

    #[test]
    fn in_toggle_mode_either_shortcut_stops_unless_the_other_is_held() {
        let mode = RecordingMode::Toggle;
        for (started, other) in [(DICTATION, COMMAND), (COMMAND, DICTATION)] {
            let activity = recording(Some(started));
            // Released after starting: the other shortcut's press stops it.
            assert_eq!(run(activity, mode, &[Input::HotkeyDown(other)]), [STOP]);
            // Still held from starting: the other shortcut does nothing.
            assert_eq!(
                steps(
                    activity,
                    mode,
                    &mut holding(started),
                    &[
                        Input::HotkeyDown(other),
                        Input::HotkeyUp(other),
                        Input::HotkeyUp(started)
                    ]
                ),
                [Action::Nothing, Action::Nothing, Action::Nothing]
            );
        }
    }

    #[test]
    fn a_ui_recording_stops_on_release_once_both_shortcuts_are_up() {
        assert_eq!(
            run(
                recording(None),
                RecordingMode::Hold,
                &[
                    Input::HotkeyDown(DICTATION),
                    Input::HotkeyDown(COMMAND),
                    Input::HotkeyUp(COMMAND),
                    Input::HotkeyUp(DICTATION)
                ]
            ),
            [Action::Nothing, Action::Nothing, Action::Nothing, STOP]
        );
    }

    #[test]
    fn the_hotkey_does_nothing_while_transcribing() {
        for mode in [RecordingMode::Hold, RecordingMode::Toggle] {
            for shortcut in [DICTATION, COMMAND] {
                assert_eq!(
                    run(
                        Activity::Transcribing,
                        mode,
                        &[Input::HotkeyDown(shortcut), Input::HotkeyUp(shortcut)]
                    ),
                    [Action::Nothing, Action::Nothing]
                );
            }
        }
    }

    #[test]
    fn toggle_starts_stops_and_waits_for_transcription() {
        let mode = RecordingMode::Hold;
        assert_eq!(
            run(Activity::Idle, mode, &[Input::Toggle]),
            [Action::Start { by: None }]
        );
        assert_eq!(run(recording(None), mode, &[Input::Toggle]), [STOP]);
        assert_eq!(
            run(Activity::Transcribing, mode, &[Input::Toggle]),
            [Action::Nothing]
        );
    }

    #[test]
    fn return_sends_a_hands_free_recording() {
        use Input::*;
        let send = Action::Send;
        // Toggle mode, from the hotkey or the UI.
        for by in [Some(DICTATION), None] {
            assert_eq!(run(recording(by), RecordingMode::Toggle, &[Send]), [send]);
        }
        assert_eq!(run(recording(None), RecordingMode::Hold, &[Send]), [send]);
        // Locked on by a double tap in Hold mode.
        assert_eq!(
            hold(&[
                (0, HotkeyDown(DICTATION)),
                (100, HotkeyUp(DICTATION)),
                (250, HotkeyDown(DICTATION)),
                (320, HotkeyUp(DICTATION)),
                (3_000, Send),
            ]),
            [
                START,
                Action::AwaitSecondTap,
                Action::Lock,
                Action::Nothing,
                send
            ]
        );
    }

    #[test]
    fn return_with_the_hotkey_held_sends() {
        use Input::*;
        // Releasing the hotkey afterwards finds the recording already stopped.
        assert_eq!(
            hold(&[
                (0, HotkeyDown(DICTATION)),
                (900, Send),
                (1_000, HotkeyUp(DICTATION))
            ]),
            [START, Action::Send, Action::Nothing]
        );
        // Not while idle or transcribing either.
        for activity in [Activity::Idle, Activity::Transcribing] {
            assert_eq!(
                run(activity, RecordingMode::Toggle, &[Send]),
                [Action::Nothing]
            );
        }
    }

    #[test]
    fn escape_cancels_whatever_is_running() {
        let mode = RecordingMode::Hold;
        assert_eq!(
            run(recording(None), mode, &[Input::Escape]),
            [Action::Stop { cancelled: true }]
        );
        assert_eq!(
            run(Activity::Transcribing, mode, &[Input::Escape]),
            [Action::CancelTranscription]
        );
        assert_eq!(
            run(Activity::Idle, mode, &[Input::Escape]),
            [Action::Nothing]
        );
    }
}
