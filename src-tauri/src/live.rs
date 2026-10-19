//! Incremental, local dictation using the existing offline engines. Finalize
//! short phrases at quiet boundaries instead of repeatedly replacing unstable
//! hypotheses in another app. Forced cuts retain a little audio context, and history
//! contains exactly the combined phrases, not a second, different transcription.

use std::{
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::Duration,
};

use tauri::{AppHandle, Manager};

use crate::{
    AppState, LockExt,
    audio::{Captured, Monitor, WHISPER_SAMPLE_RATE},
    dictation::{Controller, Event},
    gain::{self, Booster},
    learn, models, pipeline, platform,
    settings::Settings,
    text,
    tone::{self, AppCategory},
};

const RATE: usize = WHISPER_SAMPLE_RATE as usize;
const FRAME: usize = RATE / 50; // 20 ms
const POLL: Duration = Duration::from_millis(100);

/// Where audio arriving in a stream is cut into phrases, in samples.
pub struct Bounds {
    /// Pauses don't end a phrase shorter than this.
    pub min: usize,
    /// Longer speech is cut without a pause, near its quietest point.
    pub max: usize,
    /// A pause this long ends a phrase.
    pub quiet: usize,
    /// Audio replayed after a cut without a pause, so a word split by it is heard whole.
    pub overlap: usize,
}

/// Typing live: short phrases, so words appear soon after they're said.
const PHRASES: Bounds = Bounds {
    min: RATE,
    max: RATE * 4,
    quiet: RATE * 3 / 10,
    overlap: RATE / 2,
};

pub struct Handle {
    finish: Option<Sender<Captured>>,
    pub cancelled: Arc<AtomicBool>,
}

impl Handle {
    pub fn spawn(
        app: AppHandle,
        controller: Controller,
        session: u64,
        monitor: Monitor,
        settings: Settings,
        target: u64,
    ) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = cancelled.clone();
        thread::Builder::new()
            .name("live-dictation".into())
            .spawn(move || {
                let result = panic::catch_unwind(AssertUnwindSafe(|| {
                    run(
                        &app,
                        &controller,
                        session,
                        monitor,
                        settings,
                        target,
                        cancel,
                        rx,
                    )
                }));
                match result {
                    Ok(Some(outcome)) => controller.send(Event::Transcribed { session, outcome }),
                    Err(_) => controller.send(Event::Transcribed {
                        session,
                        outcome: Err(pipeline::Error::Transcribe(
                            "The live engine crashed".into(),
                        )),
                    }),
                    Ok(None) => {} // Recording was discarded.
                }
            })?;
        Ok(Self {
            finish: Some(tx),
            cancelled,
        })
    }

    pub fn finish(mut self, captured: Captured, cancelled: bool) -> Result<(), Captured> {
        if cancelled {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        let tx = self.finish.take().expect("live handle finishes once");
        tx.send(captured).map_err(|error| error.0)?;
        Ok(())
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.finish.is_some() {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }
}

struct Writer {
    target: u64,
    cancelled: Arc<AtomicBool>,
    paused: bool,
    enigo: Option<enigo::Enigo>,
}

impl Writer {
    fn insert(&mut self, text: &str) -> Result<(), String> {
        if self.cancelled.load(Ordering::SeqCst) || self.paused {
            return Ok(());
        }
        if platform::typing_target() != Some(self.target) {
            self.paused = true;
            return Err("The active app changed".into());
        }
        if let Err(error) = platform::send_live_text(&mut self.enigo, text) {
            self.paused = true;
            return Err(error);
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    app: &AppHandle,
    controller: &Controller,
    session: u64,
    monitor: Monitor,
    settings: Settings,
    target: u64,
    cancelled: Arc<AtomicBool>,
    rx: Receiver<Captured>,
) -> Option<Result<String, pipeline::Error>> {
    let model = models::find(&settings.selected_model)?;
    // Picks the dictionary entries for this kind of app. Found before the
    // first phrase, which needs at least a second of speech anyway.
    let category = tone::Detection::start()
        .app()
        .map_or(AppCategory::Other, |app| {
            tone::classify(&app, &settings.tone.apps)
        });
    let mut pending = Vec::new();
    let mut offset = 0;
    let mut transcript = String::new();
    let mut replayed = 0;
    let mut writer = Writer {
        target,
        cancelled,
        paused: false,
        enigo: None,
    };
    let mut failure = None;
    // Whisper mode: pauses and speech are judged on amplified audio, which
    // lowers the silence thresholds by the same factor.
    let mut booster = Booster::default();
    let current_gain = |booster: &Booster| {
        if settings.boost_quiet_speech {
            booster.gain()
        } else {
            1.0
        }
    };

    let captured = loop {
        match rx.recv_timeout(POLL) {
            Ok(captured) => break captured,
            Err(RecvTimeoutError::Disconnected) => return None,
            Err(RecvTimeoutError::Timeout) => {}
        }
        // A failed engine is retried on release using the full remaining audio.
        if failure.is_some() {
            continue;
        }
        let samples = monitor.read(offset);
        offset += samples.len();
        booster.push(&samples);
        pending.extend(samples);
        let gain = current_gain(&booster);
        loop {
            let heard = gain::amplify(&pending, gain);
            let Some(end) = phrase_end(&PHRASES, &heard, false) else {
                break;
            };
            let carry = overlap_at(&PHRASES, &heard, end);
            let result = recognize(
                app,
                &settings,
                category,
                &heard[..end],
                replayed,
                &transcript,
                false,
                carry > 0,
            );
            drop(heard);
            match result {
                Ok(text) => {
                    append(
                        &mut transcript,
                        &text,
                        replayed > 0,
                        &mut writer,
                        controller,
                        session,
                    );
                    pending.drain(..end - carry);
                    replayed = carry;
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
    };

    let rest = &captured.samples[offset.min(captured.samples.len())..];
    booster.push(rest);
    pending.extend_from_slice(rest);
    let gain = current_gain(&booster);
    // If a worker was slower than the speaker, drain bounded phrases before
    // saving. Escape disables insertion immediately but keeps the transcript.
    loop {
        let heard = gain::amplify(&pending, gain);
        let Some(end) = phrase_end(&PHRASES, &heard, true) else {
            break;
        };
        if pending.len() <= replayed {
            break;
        }
        let carry = if end == pending.len() {
            0
        } else {
            overlap_at(&PHRASES, &heard, end)
        };
        let result = recognize(
            app,
            &settings,
            category,
            &heard[..end],
            replayed,
            &transcript,
            end == pending.len(),
            carry > 0,
        );
        drop(heard);
        match result {
            Ok(text) => {
                append(
                    &mut transcript,
                    &text,
                    replayed > 0,
                    &mut writer,
                    controller,
                    session,
                );
                pending.drain(..end - carry);
                replayed = carry;
                failure = None;
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }

    if !transcript.is_empty() {
        pipeline::save(
            app,
            &captured.samples,
            captured.duration_secs,
            &transcript,
            model.name,
            None,
        );
        // Only when every phrase reached the app, so the field holds the whole transcript.
        if !writer.paused && !writer.cancelled.load(Ordering::SeqCst) {
            learn::watch_for_corrections(app, &transcript);
        }
    }
    if let Some(error) = failure {
        return Some(Err(error));
    }
    Some(if transcript.is_empty() {
        Err(pipeline::Error::NoSpeech)
    } else {
        Ok(transcript)
    })
}

/// `said` is the transcript so far. Whisper reads its end as context, so each
/// phrase continues the sentence instead of being heard as a fresh start.
#[allow(clippy::too_many_arguments)]
fn recognize(
    app: &AppHandle,
    settings: &Settings,
    category: AppCategory,
    samples: &[f32],
    replayed: usize,
    said: &str,
    final_phrase: bool,
    continues: bool,
) -> Result<String, pipeline::Error> {
    // Silence never reaches the recognizer (Whisper can hallucinate on silence).
    if !has_speech(&samples[replayed.min(samples.len())..]) {
        return Ok(String::new());
    }
    let state = app.state::<AppState>();
    let model = models::find(&settings.selected_model).ok_or(pipeline::Error::NoModel)?;
    let mut engine = state.engine.lock_unpoisoned();
    state
        .load_model(app, &mut engine, model)
        .map_err(pipeline::Error::ModelLoad)?;
    let prompt = text::prompt(&settings.dictionary, said, category);
    let raw = engine
        .transcribe(samples, &settings.language, &prompt)
        .map_err(pipeline::Error::Transcribe)?;
    let options = phrase_options(settings, category, final_phrase);
    // No language-model cleanup here: it needs whole sentences, and would hold
    // up every phrase. Typing live trades it for words appearing as you speak.
    let processed = text::finish(
        &text::normalize_transcription(&raw, options.auto_edit, options.plain_punctuation),
        &options,
    );
    // Offline engines often add a period because the audio buffer ended. A
    // forced streaming boundary is not a sentence ending.
    Ok(if continues {
        processed.trim_end_matches('.').to_string()
    } else {
        processed
    })
}

/// How one phrase typed live is tidied.
fn phrase_options(
    settings: &Settings,
    category: AppCategory,
    final_phrase: bool,
) -> text::Options<'_> {
    text::Options {
        auto_edit: settings.auto_edit,
        // Preserve sentence punctuation within the stream.
        smart_trailing_punctuation: final_phrase && settings.smart_trailing_punctuation,
        plain_punctuation: settings.plain_punctuation,
        dictionary: &settings.dictionary,
        app: category,
        // Voice commands only apply to dictation pasted at the end. Typed live,
        // "new line" would press Return on Windows (sending a chat message or
        // running a terminal command) and type a stray character on macOS, and
        // "scratch that" could only take back the phrase it's in.
        voice_commands: false,
        self_corrections: settings.self_corrections,
        smart_formatting: settings.smart_formatting,
        snippets: &settings.snippets,
        english: text::is_english(&settings.language),
    }
}

fn append(
    transcript: &mut String,
    phrase: &str,
    overlapping: bool,
    writer: &mut Writer,
    controller: &Controller,
    session: u64,
) {
    let phrase = if overlapping {
        without_overlap(transcript, phrase)
    } else {
        phrase
    };
    if phrase.is_empty() {
        return;
    }
    let insertion = if transcript.is_empty() {
        phrase.to_string()
    } else {
        format!(" {phrase}")
    };
    transcript.push_str(&insertion);
    if let Err(error) = writer.insert(&insertion) {
        eprintln!("[live] typing paused: {error}");
        controller.send(Event::LiveTypingPaused { session });
    }
}

/// Reconcile only overlapped audio; natural pauses preserve intentional repetition.
pub fn without_overlap<'a>(previous: &str, phrase: &'a str) -> &'a str {
    let words: Vec<_> = previous.split_whitespace().collect();
    let next: Vec<_> = phrase.split_whitespace().collect();
    let normalized = |word: &str| {
        word.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    for count in (1..=words.len().min(next.len()).min(6)).rev() {
        if words[words.len() - count..]
            .iter()
            .zip(&next[..count])
            .all(|(a, b)| {
                let a = normalized(a);
                !a.is_empty() && a == normalized(b)
            })
        {
            // Locate the last matched token by its slice pointer, retaining
            // Unicode byte boundaries and arbitrary whitespace correctly.
            let matched = next[count - 1];
            let offset = matched.as_ptr() as usize - phrase.as_ptr() as usize + matched.len();
            return phrase[offset..].trim_start();
        }
    }
    phrase
}

pub fn overlap_at(bounds: &Bounds, samples: &[f32], end: usize) -> usize {
    let has_pause = end >= bounds.quiet
        && samples[end - bounds.quiet..end]
            .chunks(FRAME)
            .all(|frame| rms(frame) < 0.006);
    if end >= bounds.max - RATE / 2 && !has_pause {
        bounds.overlap
    } else {
        0
    }
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len().max(1) as f64)
        .sqrt() as f32
}

pub fn has_speech(samples: &[f32]) -> bool {
    samples
        .chunks(FRAME)
        .filter(|frame| rms(frame) > 0.008)
        .take(3)
        .count()
        == 3
}

/// Finalize on a pause of `bounds.quiet`, or near the quietest point before
/// `bounds.max`. Forced cuts can retain audio context via `overlap_at`.
pub fn phrase_end(bounds: &Bounds, samples: &[f32], finished: bool) -> Option<usize> {
    let limit = samples.len().min(bounds.max);
    let mut quiet = 0;
    for (index, frame) in samples[..limit].chunks(FRAME).enumerate() {
        quiet = if rms(frame) < 0.006 {
            quiet + frame.len()
        } else {
            0
        };
        let end = (index * FRAME + frame.len()).min(limit);
        if end >= bounds.min && quiet >= bounds.quiet {
            return Some(end);
        }
    }
    if limit == bounds.max {
        // Prefer a natural word boundary within the last half second.
        let start = bounds.max - RATE / 2;
        let boundary = samples[start..bounds.max]
            .chunks(FRAME)
            .enumerate()
            .min_by(|(_, a), (_, b)| rms(a).total_cmp(&rms(b)))
            .map(|(i, _)| start + i * FRAME + FRAME)
            .unwrap_or(bounds.max);
        return Some(boundary.min(bounds.max));
    }
    (finished && !samples.is_empty()).then_some(samples.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_commands_are_never_typed_live() {
        let settings = Settings {
            language: "en".into(),
            ..Settings::default()
        };
        assert!(settings.voice_commands);
        let options = phrase_options(&settings, AppCategory::Other, true);
        assert!(!options.voice_commands);
        assert_eq!(
            text::finish("Hello new line there", &options),
            "Hello new line there"
        );
    }

    #[test]
    fn pauses_finalize_phrases_without_waiting_for_release() {
        let mut samples = vec![0.08; RATE * 2];
        samples.extend(vec![0.0; PHRASES.quiet]);
        assert_eq!(phrase_end(&PHRASES, &samples, false), Some(samples.len()));
        assert!(has_speech(&samples));
    }

    #[test]
    fn continuous_speech_is_bounded_and_no_samples_are_lost_or_repeated() {
        let original: Vec<f32> = (0..RATE * 11 + 137)
            .map(|i| (i % 320) as f32 / 3200.0)
            .collect();
        let mut pending = original.clone();
        let mut drained = Vec::new();
        while let Some(end) = phrase_end(&PHRASES, &pending, true) {
            assert!(end <= PHRASES.max);
            drained.extend(pending.drain(..end));
        }
        assert_eq!(drained, original);
        assert!(pending.is_empty());
    }

    #[test]
    fn short_utterances_flush_only_on_stop_and_noise_does_not_transcribe() {
        assert_eq!(phrase_end(&PHRASES, &vec![0.08; RATE / 2], false), None);
        assert_eq!(
            phrase_end(&PHRASES, &vec![0.08; RATE / 2], true),
            Some(RATE / 2)
        );
        assert_eq!(phrase_end(&PHRASES, &[], true), None);
        assert!(!has_speech(&vec![0.002; RATE * 4]));
    }

    #[test]
    fn cancelled_or_paused_writer_never_sends_more_input() {
        let mut writer = Writer {
            target: 0,
            cancelled: Arc::new(AtomicBool::new(true)),
            paused: false,
            enigo: None,
        };
        assert!(writer.insert("must not type").is_ok());
        assert!(writer.enigo.is_none());
        writer.cancelled.store(false, Ordering::SeqCst);
        writer.paused = true;
        assert!(writer.insert("must not type").is_ok());
        assert!(writer.enigo.is_none());
    }

    #[test]
    fn overlapping_audio_does_not_repeat_committed_words() {
        assert_eq!(
            without_overlap("while I am still", "I am still speaking."),
            "speaking."
        );
        assert_eq!(
            without_overlap("Hello world.", "World, and everyone."),
            "and everyone."
        );
        assert_eq!(without_overlap("yes", "yes"), "");
        assert_eq!(
            without_overlap("good morning", "The next sentence"),
            "The next sentence"
        );
        assert_eq!(without_overlap("你好 世界", "世界 再见"), "再见");
        assert_eq!(
            overlap_at(&PHRASES, &vec![0.08; PHRASES.max], PHRASES.max),
            PHRASES.overlap
        );
        let mut paused = vec![0.08; RATE * 2];
        paused.extend(vec![0.0; PHRASES.quiet]);
        assert_eq!(overlap_at(&PHRASES, &paused, paused.len()), 0);
    }

    #[test]
    #[ignore = "requires WORDFLOW_TEST_MODELS, WORDFLOW_TEST_AUDIO and a downloaded model"]
    fn offline_speech_emits_phrases_before_the_recording_ends() {
        let model_id = std::env::var("WORDFLOW_TEST_MODEL").unwrap_or("base".into());
        let models = std::env::var("WORDFLOW_TEST_MODELS").expect("model folder");
        let audio = std::env::var("WORDFLOW_TEST_AUDIO").expect("generated test audio");
        let model = models::find(&model_id).expect("catalog model");
        let store = models::ModelStore::new(models.into());
        let mut engine = crate::engine::Engine::default();
        crate::engine::silence_logs();
        engine
            .load(model, &store.path(model))
            .expect("load offline model");
        let (samples, _) =
            crate::media::decode_file(std::path::Path::new(&audio)).expect("decode fixture");
        let mut pending = Vec::new();
        let mut phrases = Vec::new();
        let mut transcript = String::new();
        let mut replayed = 0;
        let mut early = 0;
        let mut received = 0;
        for block in samples.chunks(RATE / 4) {
            received += block.len();
            pending.extend_from_slice(block);
            while let Some(end) = phrase_end(&PHRASES, &pending, false) {
                let carry = overlap_at(&PHRASES, &pending, end);
                if has_speech(&pending[replayed..end]) {
                    let text = engine
                        .transcribe(&pending[..end], "en", "")
                        .expect("recognize live phrase");
                    let text = if replayed > 0 {
                        without_overlap(&transcript, &text).to_string()
                    } else {
                        text
                    };
                    println!("at {:.2}s: {}", received as f64 / RATE as f64, text.trim());
                    transcript.push(' ');
                    transcript.push_str(text.trim());
                    phrases.push(text);
                    if received < samples.len() {
                        early += 1;
                    }
                }
                pending.drain(..end - carry);
                replayed = carry;
            }
        }
        if has_speech(&pending[replayed.min(pending.len())..]) {
            let text = engine
                .transcribe(&pending, "en", "")
                .expect("flush final phrase");
            phrases.push(if replayed > 0 {
                without_overlap(&transcript, &text).to_string()
            } else {
                text
            });
        }
        assert!(early >= 2, "must emit multiple phrases before release");
        let transcript = phrases.join(" ").to_lowercase();
        for word in ["testing", "dictation", "words", "speaking", "final"] {
            assert!(transcript.contains(word), "missing {word}: {transcript}");
        }
    }
}
