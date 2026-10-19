//! Audio in, cleaned-up text out: shared by dictation and file transcription.

use std::{
    borrow::Cow,
    fmt,
    path::{Path, PathBuf},
    sync::MutexGuard,
    time::{Duration, Instant},
};

use tauri::{AppHandle, Emitter, Manager};

use crate::{
    AppState, LockExt,
    cleanup::{self, Cleaner},
    engine::Engine,
    gain,
    history::{self, HistoryItem},
    live, media, models,
    platform::FrontApp,
    settings::Settings,
    stream::Prefetch,
    text,
    tone::{self, AppCategory},
};

#[derive(Debug)]
pub enum Error {
    NoModel,
    ModelLoad(String),
    Transcribe(String),
    NoSpeech,
    /// Words were heard, but voice commands like "scratch that" left nothing
    /// to type. What was heard is kept in History.
    NothingLeft,
    /// Command mode couldn't edit the selection: a message for the pill, and
    /// what went wrong, for the log.
    Command(&'static str, Option<String>),
}

impl Error {
    /// Short message for the pill and the UI.
    pub fn message(&self) -> &'static str {
        match self {
            Error::NoModel => "No model selected",
            Error::ModelLoad(_) => "Model load failed",
            Error::Transcribe(_) => "Transcription failed",
            Error::NoSpeech => "No speech detected",
            Error::NothingLeft => "Nothing left to type. Saved in History.",
            Error::Command(message, _) => message,
        }
    }

    /// The underlying engine error, for logs and the Transcribe Audio screen.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Error::ModelLoad(e) | Error::Transcribe(e) => Some(e),
            Error::Command(_, detail) => detail.as_deref(),
            Error::NoModel | Error::NoSpeech | Error::NothingLeft => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.detail() {
            Some(detail) => write!(f, "{}: {detail}", self.message()),
            None => f.write_str(self.message()),
        }
    }
}

impl std::error::Error for Error {}

/// What the pipeline is busy with, for the pill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Waiting for the speech model to load.
    Loading,
    Transcribing,
    /// A local language model is tidying the transcript.
    CleaningUp,
    /// A local language model is editing the selection, in command mode.
    Editing,
}

/// Transcribes 16 kHz mono audio with the selected model, saves the recording
/// and the result to history, and returns the history item.
///
/// `on_stage` is called as the work moves along. Loading is always followed by
/// Transcribing, whether or not loading succeeded. `target` is the app being
/// dictated into, which sets the tone. `prefetch` has transcribed the start of
/// the recording while it was made, leaving only the rest to do here.
pub fn run(
    app: &AppHandle,
    samples: &[f32],
    duration_secs: f64,
    target: Option<&FrontApp>,
    prefetch: Option<Prefetch>,
    on_stage: impl Fn(Stage),
) -> Result<HistoryItem, Error> {
    let state = app.state::<AppState>();
    let settings = state.settings();
    let model = models::find(&settings.selected_model)
        .filter(|m| state.models.is_downloaded(m.id))
        .ok_or(Error::NoModel)?;
    // Before taking the engine, which the part it's finishing holds.
    let mut earlier = prefetch.map(Prefetch::finish).unwrap_or_default();

    let started = Instant::now();
    let mut engine = lock_engine(&state, &on_stage);
    if engine.loaded_model() != Some(model.id) {
        on_stage(Stage::Loading);
        let loaded = state.load_model(app, &mut engine, model);
        on_stage(Stage::Transcribing);
        loaded.map_err(Error::ModelLoad)?;
    }
    let loading = started.elapsed();
    let style = tone::Style::new(&settings.tone, target);
    // The recording is saved as it was captured.
    let heard = heard(&settings, samples);
    let rest = &heard[earlier.start.min(heard.len())..];
    // Given words to expect and only silence, Whisper may write those words.
    // Audio already heard at the end of the last part doesn't count.
    if live::has_speech(&rest[earlier.replayed.min(rest.len())..]) {
        let prompt = text::prompt(&settings.dictionary, "", style.category);
        let words = engine
            .transcribe(rest, &settings.language, &prompt)
            .map_err(Error::Transcribe)?;
        earlier.append(&words);
    } else if earlier.start == 0 {
        // As before streaming: an empty prompt lets Whisper say there's nothing.
        let words = engine
            .transcribe(rest, &settings.language, "")
            .map_err(Error::Transcribe)?;
        earlier.append(&words);
    }
    drop(engine);
    let raw = earlier.raw;
    // Only what was left after the parts transcribed while speaking.
    let transcribing = started.elapsed() - loading;

    let options = text::Options {
        auto_edit: settings.auto_edit,
        smart_trailing_punctuation: settings.smart_trailing_punctuation,
        plain_punctuation: settings.plain_punctuation,
        dictionary: &settings.dictionary,
        app: style.category,
        voice_commands: settings.voice_commands,
        self_corrections: settings.self_corrections,
        smart_formatting: settings.smart_formatting,
        snippets: &settings.snippets,
        english: text::is_english(&settings.language),
    };
    let mut text = style.format(&text::normalize_transcription(
        &raw,
        options.auto_edit,
        options.plain_punctuation,
    ));
    let mut original = None;
    let mut cleaning = None;
    if Cleaner::will_run(&settings.cleanup, &state.models, &text) {
        let started = Instant::now();
        on_stage(Stage::CleaningUp);
        original = Some(text.clone());
        let mut hint = style.hint();
        if options.plain_punctuation {
            hint = Some(cleanup::plain_punctuation(hint));
        }
        text = state
            .cleaner
            .clean_for(&settings.cleanup, &state.models, &text, hint.as_ref());
        text = style.adjust_cleaned(&text);
        cleaning = Some(started.elapsed());
    } else {
        text = style.adjust(&text);
    }
    // After cleanup, so dictionary rules and snippets always have the last word.
    let text = text::finish(&text, &options);
    log_timings(duration_secs, model.name, loading, transcribing, cleaning);
    if text.is_empty() {
        // Usually "scratch that". In case it wasn't meant, what was heard
        // stays in History rather than being lost with the recording.
        let heard =
            text::normalize_transcription(&raw, options.auto_edit, options.plain_punctuation);
        let heard = heard.trim();
        if heard.is_empty() {
            return Err(Error::NoSpeech);
        }
        save(app, samples, duration_secs, heard, model.name, None);
        return Err(Error::NothingLeft);
    }

    Ok(save(
        app,
        samples,
        duration_secs,
        &text,
        model.name,
        original.as_deref(),
    ))
}

/// Transcribes audio as `run` does, without cleanup or saving it: for
/// command mode, where what was said is an instruction. Returns the text and
/// the name of the model that heard it.
pub fn transcribe_only(
    app: &AppHandle,
    samples: &[f32],
    on_stage: impl Fn(Stage),
) -> Result<(String, &'static str), Error> {
    let state = app.state::<AppState>();
    let settings = state.settings();
    let samples = &*heard(&settings, samples);
    // Whisper may hear words in silence, which mustn't become an edit.
    if !live::has_speech(samples) {
        return Err(Error::NoSpeech);
    }
    let model = models::find(&settings.selected_model)
        .filter(|m| state.models.is_downloaded(m.id))
        .ok_or(Error::NoModel)?;
    let mut engine = lock_engine(&state, &on_stage);
    if engine.loaded_model() != Some(model.id) {
        on_stage(Stage::Loading);
        let loaded = state.load_model(app, &mut engine, model);
        on_stage(Stage::Transcribing);
        loaded.map_err(Error::ModelLoad)?;
    }
    let raw = engine
        .transcribe(samples, &settings.language, "")
        .map_err(Error::Transcribe)?;
    drop(engine);
    // An instruction for the model, not text to paste, so its punctuation stays.
    let text = text::normalize_transcription(&raw, true, false);
    if text.trim().is_empty() {
        return Err(Error::NoSpeech);
    }
    Ok((text.trim().to_string(), model.name))
}

/// The audio as the speech model hears it: with Whisper mode's boost when
/// that's on.
pub fn heard<'a>(settings: &Settings, samples: &'a [f32]) -> Cow<'a, [f32]> {
    if settings.boost_quiet_speech {
        gain::boost(samples)
    } else {
        samples.into()
    }
}

/// Transcribes audio already passed through `heard` the way dictation does,
/// with the dictionary as a hint, but without tone, cleanup, voice commands
/// or replacements: for the microphone test, to show what the model hears.
pub fn transcribe_heard(app: &AppHandle, heard: &[f32]) -> Result<String, Error> {
    let state = app.state::<AppState>();
    let settings = state.settings();
    if !live::has_speech(heard) {
        return Err(Error::NoSpeech);
    }
    let model = models::find(&settings.selected_model)
        .filter(|m| state.models.is_downloaded(m.id))
        .ok_or(Error::NoModel)?;
    let mut engine = state.engine.lock_unpoisoned();
    if engine.loaded_model() != Some(model.id) {
        state
            .load_model(app, &mut engine, model)
            .map_err(Error::ModelLoad)?;
    }
    let prompt = text::prompt(&settings.dictionary, "", AppCategory::Other);
    let raw = engine
        .transcribe(heard, &settings.language, &prompt)
        .map_err(Error::Transcribe)?;
    drop(engine);
    let text = text::normalize_transcription(&raw, settings.auto_edit, settings.plain_punctuation);
    match text.trim() {
        "" => Err(Error::NoSpeech),
        text => Ok(text.to_string()),
    }
}

/// Save one history entry for the entire live session, rather than one per phrase.
pub fn save(
    app: &AppHandle,
    samples: &[f32],
    duration_secs: f64,
    text: &str,
    model_name: &str,
    original: Option<&str>,
) -> HistoryItem {
    let state = app.state::<AppState>();

    // The WAV is written without holding the history lock, so the UI can still
    // read history meanwhile.
    let audio_path = if state.settings().save_recordings {
        let recordings_dir = state
            .history
            .lock_unpoisoned()
            .recordings_dir()
            .to_path_buf();
        save_recording(&recordings_dir, samples)
    } else {
        None
    };
    let added =
        state
            .history
            .lock_unpoisoned()
            .add(text, duration_secs, model_name, audio_path, original);
    let _ = app.emit("history-changed", ());

    // Saving is best effort: the text is still returned if the disk write failed.
    let item = match added {
        Ok(item) => item,
        Err(e) => {
            eprintln!("[history] couldn't save transcript: {e}");
            None
        }
    };
    item.unwrap_or_else(|| HistoryItem {
        id: String::new(),
        created_at: history::now_ms(),
        word_count: text.split_whitespace().count(),
        transcript: text.to_string(),
        duration_secs,
        model: model_name.into(),
        audio_path: None,
        original: None,
    })
}

/// How long each stage took, to see where the wait after speaking goes.
fn log_timings(
    duration_secs: f64,
    model: &str,
    loading: Duration,
    transcribing: Duration,
    cleaning: Option<Duration>,
) {
    let mut line = format!("[pipeline] {duration_secs:.1}s of audio:");
    // Waiting for the engine counts as loading: a warm-up holds it while loading.
    if loading >= Duration::from_millis(10) {
        line += &format!(" loaded in {loading:.2?},");
    }
    line += &format!(" {model} in {transcribing:.2?}");
    if let Some(cleaning) = cleaning {
        line += &format!(", cleaned up in {cleaning:.2?}");
    }
    eprintln!("{line}");
}

/// If a warm-up holds the engine, the model is still loading, so report that while waiting.
fn lock_engine<'a>(state: &'a AppState, on_stage: &impl Fn(Stage)) -> MutexGuard<'a, Engine> {
    if let Some(engine) = state.engine.try_lock_unpoisoned() {
        return engine;
    }
    on_stage(Stage::Loading);
    let engine = state.engine.lock_unpoisoned();
    on_stage(Stage::Transcribing);
    engine
}

fn save_recording(dir: &Path, samples: &[f32]) -> Option<PathBuf> {
    let path = dir.join(format!("recording-{}.wav", history::now_ms()));
    match media::write_wav(&path, samples) {
        Ok(()) => Some(path),
        Err(e) => {
            eprintln!("[history] couldn't save recording: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn errors_show_their_detail_when_they_have_one() {
        assert_eq!(Error::NoModel.to_string(), "No model selected");
        assert_eq!(Error::NoSpeech.to_string(), "No speech detected");
        assert_eq!(
            Error::ModelLoad("file is corrupt".into()).to_string(),
            "Model load failed: file is corrupt"
        );
        assert_eq!(
            Error::Transcribe("out of memory".into()).to_string(),
            "Transcription failed: out of memory"
        );
    }

    #[test]
    fn short_messages_leave_out_the_detail() {
        let error = Error::Transcribe("out of memory".into());
        assert_eq!(error.message(), "Transcription failed");
        assert_eq!(error.detail(), Some("out of memory"));
        assert_eq!(Error::NoSpeech.detail(), None);
        assert_eq!(
            Error::NothingLeft.to_string(),
            "Nothing left to type. Saved in History."
        );
    }
}
