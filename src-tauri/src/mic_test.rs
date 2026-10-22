//! The microphone test in Settings: records a short clip, plays it back as
//! the speech model hears it, and shows what the model made of it. Lets the
//! user judge background noise and how cleanly their voice comes through.
//!
//! Nothing here is saved to History, typed, or cleaned up.

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State, ipc::Response};

use crate::{AppState, LockExt, audio::Recording, dictation::DictationState, media, pipeline};

type CommandResult<T> = Result<T, String>;

#[derive(Default)]
pub struct MicTest {
    recording: Mutex<Option<Recording>>,
    /// The last clip as the model hears it, until it's transcribed.
    clip: Mutex<Vec<f32>>,
}

/// Runs blocking work, like opening the microphone or waiting for the engine,
/// off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> CommandResult<T> + Send + 'static,
) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

/// Starts recording from the selected microphone. Its level arrives as
/// `mic-test-level` events (0..1).
#[tauri::command]
pub async fn start_mic_test(app: AppHandle) -> CommandResult<()> {
    blocking(move || {
        let state = app.state::<AppState>();
        // One microphone user at a time, so the test hears what dictation would.
        if !matches!(
            *state.dictation_state.lock_unpoisoned(),
            DictationState::Idle
        ) {
            return Err("Finish dictating first".into());
        }
        let device = state.settings().input_device;
        let emitter = app.clone();
        let recording = Recording::start(&device, move |level| {
            let _ = emitter.emit_to("main", "mic-test-level", level);
        })?;
        // Replacing an earlier test stops its microphone.
        *app.state::<MicTest>().recording.lock_unpoisoned() = Some(recording);
        Ok(())
    })
    .await
}

/// Stops recording and returns the clip as WAV, the way the speech model
/// hears it: with Whisper mode's boost when that's on.
#[tauri::command]
pub async fn stop_mic_test(app: AppHandle) -> CommandResult<Response> {
    blocking(move || {
        let test = app.state::<MicTest>();
        let recording = test
            .recording
            .lock_unpoisoned()
            .take()
            .ok_or("The microphone test isn't recording")?;
        let samples = recording.finish()?.samples;
        let heard = pipeline::heard(&app.state::<AppState>().settings(), &samples).into_owned();
        let wav = media::wav_bytes(&heard)?;
        *test.clip.lock_unpoisoned() = heard;
        Ok(Response::new(wav))
    })
    .await
}

/// Stops a test without keeping the audio, e.g. when leaving Settings.
#[tauri::command]
pub async fn cancel_mic_test(state: State<'_, MicTest>) -> CommandResult<()> {
    state.cancel();
    Ok(())
}

impl MicTest {
    /// Stops any test and drops its audio, e.g. when the main window closes
    /// before Settings can do it.
    pub fn cancel(&self) {
        let recording = self.recording.lock_unpoisoned().take();
        self.clip.lock_unpoisoned().clear();
        // Closing the microphone can take a moment.
        tauri::async_runtime::spawn_blocking(move || drop(recording));
    }
}

/// Transcribes the last clip with the selected model, as dictation would
/// before cleanup.
#[tauri::command]
pub async fn transcribe_mic_test(app: AppHandle) -> CommandResult<String> {
    blocking(move || {
        let clip = std::mem::take(&mut *app.state::<MicTest>().clip.lock_unpoisoned());
        pipeline::transcribe_heard(&app, &clip).map_err(|e| e.message().to_string())
    })
    .await
}
