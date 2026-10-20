//! One meeting being recorded.
//!
//! Two threads:
//! - capture: several times a second, takes what the sources recorded,
//!   resamples it to 16 kHz, lines the tracks up and writes each to its own WAV
//!   file. It hands finished windows (see `segment`) to the transcriber as
//!   sample ranges.
//! - transcriber: reads each window back from disk and transcribes it with the
//!   selected model, adding segments to the meeting as they arrive.
//!
//! Neither keeps more than a window of audio in memory, so a meeting can run
//! for hours. If transcription is slower than the meeting, windows queue up as
//! ranges, not samples, and are worked through after it ends.

use std::{
    fs::{self, File},
    io::BufWriter,
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::Duration,
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use super::{
    Meeting, MeetingState, Meetings, Segment, Shared, Source, Status,
    capture::{self, Resampler, Track},
    mixer::{self, Aligner},
    segment::{Segmenter, Window},
    store::{AUDIO_FILE, MeetingStore},
};
use crate::{
    AppState, LockExt, audio::WHISPER_SAMPLE_RATE, history, models, platform, settings::Settings,
    text, tone::AppCategory,
};

const RATE: f64 = WHISPER_SAMPLE_RATE as f64;
/// How often the capture loop takes new audio.
const POLL: Duration = Duration::from_millis(250);
/// A meeting stops by itself after this long, well before a WAV file's 4 GB limit.
const MAX_SAMPLES: u64 = WHISPER_SAMPLE_RATE as u64 * 60 * 60 * 10;

pub struct Session {
    pub id: String,
    pub meeting: Shared,
    stop_tx: Mutex<Option<Sender<()>>>,
}

impl Session {
    /// Opens the sources and starts recording.
    pub fn start(
        app: &AppHandle,
        store: &MeetingStore,
        title: &str,
    ) -> Result<(Session, MeetingState), String> {
        let state = app.state::<AppState>();
        let settings = state.settings();
        let model = models::find(&settings.selected_model)
            .filter(|m| state.models.is_downloaded(m.id))
            .ok_or("Download a speech model in AI Models first")?;

        let device = settings.input_device.clone();
        let mic = Track::start("microphone", move |buffer| {
            capture::open_microphone(&device, buffer)
        })?;
        let (system, system_error) = if settings.meetings.capture_system_audio {
            let source = settings.meetings.system_audio_source.clone();
            match Track::start("system", move |buffer| {
                platform::open_system_audio(&source, buffer)
            }) {
                Ok(track) => (Some(track), None),
                Err(e) => {
                    eprintln!("[meetings] system audio: {e}");
                    (None, Some(e))
                }
            }
        } else {
            (None, None)
        };
        start_together(&mic, system.as_ref());

        let mut sources = vec![Source::Microphone];
        if system.is_some() {
            sources.push(Source::System);
        }
        let mut meeting = Meeting::new(title, history::now_ms(), sources.clone());
        meeting.model = model.name.to_string();
        store.save(&meeting)?;

        let tracks = sources
            .iter()
            .map(|source| TrackWriter::create(store.track_path(&meeting.id, *source)))
            .collect::<Result<Vec<_>, _>>();
        let mut tracks = match tracks {
            Ok(tracks) => tracks.into_iter(),
            Err(e) => {
                // Nothing was recorded, so nothing is kept.
                let _ = store.delete(&meeting.id);
                return Err(e);
            }
        };
        let (Some(mic_writer), system_writer) = (tracks.next(), tracks.next()) else {
            return Err("Couldn't create the recording".into());
        };

        let id = meeting.id.clone();
        let state = MeetingState::Recording {
            id: id.clone(),
            title: meeting.title.clone(),
            started_at_ms: meeting.created_at,
            system_audio: system.is_some(),
            system_error,
        };
        let shared: Shared = Arc::new(Mutex::new(meeting));
        let (stop_tx, stop_rx) = mpsc::channel();
        let (window_tx, window_rx) = mpsc::channel();

        let transcriber = {
            let (app, shared, settings) = (app.clone(), shared.clone(), settings.clone());
            let folder = store.folder(&id);
            thread::Builder::new()
                .name("meeting-transcriber".into())
                .spawn(move || transcribe_windows(&app, &shared, &settings, &folder, window_rx))
                .map_err(|e| e.to_string())?
        };

        let capture = Capture {
            mic: TrackState::new(Source::Microphone, mic.sample_rate, mic_writer),
            system: system
                .as_ref()
                .zip(system_writer)
                .map(|(track, writer)| TrackState::new(Source::System, track.sample_rate, writer)),
            aligner: Aligner::default(),
            windows: window_tx,
        };
        let (app, shared_meeting, folder) = (app.clone(), shared.clone(), store.folder(&id));
        thread::Builder::new()
            .name("meeting-capture".into())
            .spawn(move || {
                let result = panic::catch_unwind(AssertUnwindSafe(|| {
                    record(capture, mic, system, &stop_rx)
                }));
                let (duration_secs, error) = match result {
                    Ok(Ok(samples)) => (samples as f64 / RATE, None),
                    Ok(Err((samples, e))) => (samples as f64 / RATE, Some(e)),
                    Err(_) => (0.0, Some("The recorder crashed".to_string())),
                };
                if let Some(e) = &error {
                    eprintln!("[meetings] recording stopped: {e}");
                }
                let meetings = app.state::<Meetings>();
                let id = shared_meeting.lock_unpoisoned().id.clone();
                meetings.set_state(&app, MeetingState::Finishing { id: id.clone() });
                save_status(
                    &meetings.store,
                    &shared_meeting,
                    Status::Processing,
                    duration_secs,
                );
                // The rest of the queue is transcribed before the meeting is done.
                let _ = transcriber.join();
                let audio = finish_audio(&folder);
                {
                    let mut meeting = shared_meeting.lock_unpoisoned();
                    meeting.audio_path = audio.map(|p| p.to_string_lossy().into_owned());
                }
                save_status(
                    &meetings.store,
                    &shared_meeting,
                    Status::Done,
                    duration_secs,
                );
                meetings.finished(&app, &id);
            })
            .map_err(|e| e.to_string())?;

        Ok((
            Session {
                id,
                meeting: shared,
                stop_tx: Mutex::new(Some(stop_tx)),
            },
            state,
        ))
    }

    /// Asks the recording to stop. False if it already was.
    pub fn stop(&self) -> bool {
        self.stop_tx
            .lock_unpoisoned()
            .take()
            .is_some_and(|tx| tx.send(()).is_ok())
    }
}

/// Drops what the microphone recorded while system audio was opening, which
/// takes a few hundred milliseconds, so both tracks start at the same moment.
/// The aligner pairs their first samples, so keeping the microphone's head
/// start would put the system track that much early.
fn start_together(mic: &Track, system: Option<&Track>) {
    if let Some(system) = system {
        mic.take();
        system.take();
    }
}

fn save_status(store: &MeetingStore, meeting: &Shared, status: Status, duration_secs: f64) {
    let mut meeting = meeting.lock_unpoisoned();
    meeting.status = status;
    meeting.duration_secs = duration_secs.max(meeting.duration_secs);
    if let Err(e) = store.save(&meeting) {
        eprintln!("[meetings] couldn't save {}: {e}", meeting.id);
    }
}

/// Records until asked to stop, then hands over the last of the audio.
/// Returns how many samples the microphone track has, with the error that
/// ended the recording early, if any.
fn record(
    mut capture: Capture,
    mic: Track,
    system: Option<Track>,
    stop_rx: &Receiver<()>,
) -> Result<u64, (u64, String)> {
    let mut failure = None;
    loop {
        match stop_rx.recv_timeout(POLL) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let system_audio = system.as_ref().map(Track::take);
        if let Err(e) = capture.feed(&mic.take(), system_audio.as_deref(), false) {
            failure = Some(e);
            break;
        }
        if capture.mic.writer.written >= MAX_SAMPLES {
            failure = Some("Meetings stop after 10 hours".into());
            break;
        }
    }
    let system_audio = system.map(Track::stop);
    let mic_audio = mic.stop();
    if failure.is_none()
        && let Err(e) = capture.feed(&mic_audio, system_audio.as_deref(), true)
    {
        failure = Some(e);
    }
    let samples = capture.mic.writer.written;
    let finalized = capture.finalize();
    match failure.or(finalized.err()) {
        None => Ok(samples),
        Some(e) => Err((samples, e)),
    }
}

/// A track's file while recording.
struct TrackWriter {
    writer: hound::WavWriter<BufWriter<File>>,
    written: u64,
}

impl TrackWriter {
    fn create(path: PathBuf) -> Result<Self, String> {
        let writer = hound::WavWriter::create(&path, mixer::wav_spec())
            .map_err(|e| format!("Couldn't create the recording: {e}"))?;
        Ok(Self { writer, written: 0 })
    }

    fn write(&mut self, samples: &[f32]) -> Result<(), String> {
        for sample in samples {
            self.writer
                .write_sample(mixer::to_i16(*sample))
                .map_err(|e| format!("Couldn't save the recording: {e}"))?;
        }
        self.written += samples.len() as u64;
        Ok(())
    }

    /// Makes what's written so far readable, header included.
    fn flush(&mut self) -> Result<(), String> {
        self.writer
            .flush()
            .map_err(|e| format!("Couldn't save the recording: {e}"))
    }
}

struct TrackState {
    source: Source,
    resampler: Resampler,
    writer: TrackWriter,
    segmenter: Segmenter,
}

impl TrackState {
    fn new(source: Source, sample_rate: u32, writer: TrackWriter) -> Self {
        Self {
            source,
            resampler: Resampler::new(sample_rate),
            writer,
            segmenter: Segmenter::default(),
        }
    }

    fn resample(&mut self, samples: &[f32], finished: bool) -> Vec<f32> {
        let mut out = self.resampler.push(samples);
        if finished {
            out.extend(self.resampler.finish());
        }
        out
    }
}

struct Capture {
    mic: TrackState,
    system: Option<TrackState>,
    aligner: Aligner,
    windows: Sender<(Source, Window)>,
}

impl Capture {
    /// Writes new audio to the tracks, and queues the windows it completes.
    fn feed(&mut self, mic: &[f32], system: Option<&[f32]>, finished: bool) -> Result<(), String> {
        let mic = self.mic.resample(mic, finished);
        let blocks = match self.system.as_mut() {
            Some(state) => {
                let system = state.resample(system.unwrap_or_default(), finished);
                self.aligner.push(&mic, &system);
                let (mut mic, system) = self.aligner.take(finished);
                mixer::suppress_bleed(&mut mic, &system);
                vec![(mic, &mut self.mic), (system, state)]
            }
            None => vec![(mic, &mut self.mic)],
        };
        for (samples, state) in blocks {
            state.writer.write(&samples)?;
            state.writer.flush()?;
            let mut windows = state.segmenter.push(&samples);
            if finished {
                windows.extend(state.segmenter.finish());
            }
            for window in windows {
                // The transcriber only stops early if its thread died; the
                // audio is still saved.
                let _ = self.windows.send((state.source, window));
            }
        }
        Ok(())
    }

    fn finalize(self) -> Result<(), String> {
        let mut result = Ok(());
        for state in std::iter::once(self.mic).chain(self.system) {
            if let Err(e) = state.writer.writer.finalize() {
                result = Err(format!("Couldn't save the recording: {e}"));
            }
        }
        result
    }
}

/// Mixes the tracks into the meeting's recording and removes them. If mixing
/// fails, the microphone track is kept as the recording instead.
fn finish_audio(folder: &Path) -> Option<PathBuf> {
    let mic = folder.join(Source::Microphone.file_name());
    let system = folder.join(Source::System.file_name());
    let out = folder.join(AUDIO_FILE);
    let result = if system.is_file() {
        mixer::mix_files(&mic, &system, &out)
    } else {
        fs::rename(&mic, &out).map_err(|e| e.to_string())
    };
    match result {
        Ok(()) => {
            for track in [&mic, &system] {
                let _ = fs::remove_file(track);
            }
            Some(out)
        }
        Err(e) => {
            eprintln!("[meetings] couldn't mix the recording: {e}");
            mic.is_file().then_some(mic)
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SegmentEvent<'a> {
    meeting_id: &'a str,
    segment: &'a Segment,
}

fn transcribe_windows(
    app: &AppHandle,
    meeting: &Shared,
    settings: &Settings,
    folder: &Path,
    windows: Receiver<(Source, Window)>,
) {
    let meetings = app.state::<Meetings>();
    // The end of what each side said, so Whisper continues their sentences.
    let mut said = [String::new(), String::new()];
    for (source, window) in windows {
        let context = &mut said[source as usize];
        let result = read_window(&folder.join(source.file_name()), window)
            .and_then(|samples| recognize(app, settings, &samples, context));
        let text = match result {
            Ok(text) if !text.is_empty() => text,
            Ok(_) => continue,
            Err(e) => {
                eprintln!(
                    "[meetings] couldn't transcribe {source:?} at {}: {e}",
                    window.start
                );
                continue;
            }
        };
        context.push(' ');
        context.push_str(&text);
        if context.len() > 2000 {
            let cut = context.len() - 1000;
            let cut = (cut..context.len())
                .find(|i| context.is_char_boundary(*i))
                .unwrap_or(0);
            context.drain(..cut);
        }
        let segment = Segment {
            start_secs: window.start as f64 / RATE,
            end_secs: window.end as f64 / RATE,
            source,
            text,
        };
        let mut meeting = meeting.lock_unpoisoned();
        meeting.insert_segment(segment.clone());
        if let Err(e) = meetings.store.save(&meeting) {
            eprintln!("[meetings] couldn't save the transcript: {e}");
        }
        let _ = app.emit(
            "meeting-segment",
            SegmentEvent {
                meeting_id: &meeting.id,
                segment: &segment,
            },
        );
    }
}

/// A window's samples, read back from its track.
fn read_window(path: &Path, window: Window) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let start = u32::try_from(window.start).map_err(|_| "The recording is too long")?;
    reader.seek(start).map_err(|e| e.to_string())?;
    reader
        .samples::<i16>()
        .take(window.len())
        .map(|s| s.map(mixer::from_i16).map_err(|e| e.to_string()))
        .collect()
}

fn recognize(
    app: &AppHandle,
    settings: &Settings,
    samples: &[f32],
    said: &str,
) -> Result<String, String> {
    let state = app.state::<AppState>();
    // The current selection, as in live dictation, so switching mid-meeting
    // doesn't load models back and forth with the warm-up.
    let model = state.selected_model().ok_or("No model selected")?;
    let mut engine = state.engine.lock_unpoisoned();
    state.load_model(app, &mut engine, model)?;
    let prompt = text::prompt(&settings.dictionary, said, AppCategory::Other);
    let raw = engine.transcribe(samples, &settings.language, &prompt)?;
    drop(engine);
    let options = text::Options {
        auto_edit: settings.auto_edit,
        // A window ending is not the end of a sentence to tidy.
        smart_trailing_punctuation: false,
        plain_punctuation: settings.plain_punctuation,
        dictionary: &settings.dictionary,
        // Not dictated into an app.
        app: AppCategory::Other,
        // Commands and other spoken rules are for dictating; a meeting
        // transcript keeps what people said.
        voice_commands: false,
        snippets: &[],
        smart_formatting: false,
        self_corrections: false,
        english: text::is_english(&settings.language),
    };
    Ok(text::finish(
        &text::normalize_transcription(&raw, options.auto_edit, options.plain_punctuation),
        &options,
    )
    .trim()
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wordflow-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn capture(dir: &Path, system: bool) -> (Capture, Receiver<(Source, Window)>) {
        let (tx, rx) = mpsc::channel();
        let writer = |source: Source| TrackWriter::create(dir.join(source.file_name())).unwrap();
        let capture = Capture {
            mic: TrackState::new(Source::Microphone, 48_000, writer(Source::Microphone)),
            system: system.then(|| TrackState::new(Source::System, 44_100, writer(Source::System))),
            aligner: Aligner::default(),
            windows: tx,
        };
        (capture, rx)
    }

    #[test]
    fn tracks_are_written_in_step_and_windows_read_back() {
        let dir = temp_dir();
        let (mut capture, rx) = capture(&dir, true);
        // The other side talks for 5 s, then the user for 5 s, with pauses.
        let block = |rate: u32, level: f32| vec![level; rate as usize / 4];
        for i in 0..80 {
            let (mic, system) = match i {
                0..20 => (0.0, 0.1),
                30..50 => (0.1, 0.0),
                _ => (0.0, 0.0),
            };
            capture
                .feed(&block(48_000, mic), Some(&block(44_100, system)), false)
                .unwrap();
            if i == 25 {
                // Written so far is readable while recording goes on.
                let window = Window {
                    start: 0,
                    end: 16_000,
                };
                let samples = read_window(&dir.join(Source::System.file_name()), window).unwrap();
                assert!(samples.iter().all(|s| (s - 0.1).abs() < 0.01));
            }
        }
        capture.feed(&[], Some(&[]), true).unwrap();
        let written = capture.mic.writer.written;
        assert_eq!(written, capture.system.as_ref().unwrap().writer.written);
        assert_eq!(written, 16_000 * 20);
        capture.finalize().unwrap();

        let windows: Vec<_> = rx.try_iter().collect();
        // Each side's speech, up to the long pause after it. The silence
        // before the user spoke is part of their window, skipped windows aren't.
        let mic = Window {
            start: 16_000 * 6,
            end: 16_000 * 14,
        };
        assert_eq!(
            windows,
            [
                (
                    Source::System,
                    Window {
                        start: 0,
                        end: 16_000 * 13 / 2
                    }
                ),
                (Source::Microphone, mic),
            ]
        );
        let samples = read_window(&dir.join(Source::Microphone.file_name()), mic).unwrap();
        assert_eq!(samples.len(), mic.len());

        let audio = finish_audio(&dir).unwrap();
        assert_eq!(audio, dir.join(AUDIO_FILE));
        assert!(!dir.join(Source::System.file_name()).exists());
        let reader = hound::WavReader::open(&audio).unwrap();
        assert_eq!(reader.len() as u64, written);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tracks_start_together_once_both_are_open() {
        let source = |samples: &'static [f32]| {
            move |buffer: &capture::Buffer| {
                buffer.lock_unpoisoned().extend_from_slice(samples);
                Ok(capture::Opened {
                    keep: Box::new(()),
                    sample_rate: 16_000,
                })
            }
        };
        // The microphone's head start while system audio was opening.
        let mic = Track::start("test-mic", source(&[0.1; 4800])).unwrap();
        let system = Track::start("test-system", source(&[0.2; 10])).unwrap();
        start_together(&mic, Some(&system));
        assert!(mic.take().is_empty());
        assert!(system.take().is_empty());

        // Without system audio there's nothing to line up with.
        let mic = Track::start("test-mic", source(&[0.1; 4800])).unwrap();
        start_together(&mic, None);
        assert_eq!(mic.take().len(), 4800);
    }

    #[test]
    fn a_microphone_only_meeting_keeps_its_track_as_the_recording() {
        let dir = temp_dir();
        let (mut capture, rx) = capture(&dir, false);
        capture.feed(&vec![0.1; 48_000 * 4], None, false).unwrap();
        capture.feed(&vec![0.0; 48_000 * 2], None, true).unwrap();
        assert_eq!(capture.mic.writer.written, 16_000 * 6);
        capture.finalize().unwrap();
        assert_eq!(rx.try_iter().count(), 1);
        assert_eq!(finish_audio(&dir), Some(dir.join(AUDIO_FILE)));
        assert!(!dir.join(Source::Microphone.file_name()).exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
