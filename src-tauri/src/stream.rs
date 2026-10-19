//! Transcribes a dictation while it is being recorded, so that on release only
//! the speech since the last pause is left for the engine. Long dictations then
//! paste about as soon as short ones.
//!
//! Only raw engine output is kept here. Tone, cleanup, voice commands and the
//! dictionary still run once on the whole transcript in `pipeline::run`, so
//! the text that's pasted is tidied exactly as it would be without streaming.
//!
//! Each part is heard on its own, without the words before it as a prompt:
//! Whisper copies the punctuation of a prompt, and one run-on part would
//! turn every part after it into a run-on too. Where parts meet, the next
//! part's first letter decides whether a sentence ended (see `Heard::append`).

use std::{
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use tauri::{AppHandle, Manager};

use crate::{
    AppState, LockExt,
    audio::{Monitor, WHISPER_SAMPLE_RATE},
    gain::{self, Booster},
    live::{self, Bounds},
    models,
    settings::Settings,
    text,
    tone::{self, AppCategory},
};

const RATE: usize = WHISPER_SAMPLE_RATE as usize;
const POLL: Duration = Duration::from_millis(100);

/// Longer parts than typing live: each costs a pass of the encoder, and the
/// models punctuate short clips poorly. A part ends at a pause once it's five
/// seconds long. Speech without a pause is cut at 28 seconds, inside Whisper's
/// 30-second window: rarely, as a word split by a forced cut can come out twice.
const PARTS: Bounds = Bounds {
    min: RATE * 5,
    max: RATE * 28,
    quiet: RATE * 3 / 10,
    overlap: RATE / 2,
};

/// What was transcribed before the recording ended.
#[derive(Debug, Default)]
pub struct Heard {
    /// Raw engine output, parts joined with spaces.
    pub raw: String,
    /// Where the audio not yet in `raw` begins, in samples from the start of the recording.
    pub start: usize,
    /// How many samples from `start` on were already heard, at the end of the
    /// last part. They're heard again, so a word split by the cut comes out whole.
    pub replayed: usize,
}

impl Heard {
    /// Adds the words heard in the audio from `start` on, without the ones
    /// already in `raw` from the replayed audio.
    pub fn append(&mut self, words: &str) {
        let words = if self.replayed > 0 {
            live::without_overlap(&self.raw, words)
        } else {
            words
        };
        let words = words.trim();
        if words.is_empty() {
            return;
        }
        if !self.raw.is_empty() {
            // After a pause, the model ending a part couldn't hear what came
            // next, so its last punctuation is a guess. The next part, which
            // heard it, says whether a sentence ended there.
            if self.replayed == 0 {
                self.punctuate_end(starts_sentence(words));
            }
            self.raw.push(' ');
        }
        self.raw.push_str(words);
    }

    fn punctuate_end(&mut self, sentence: Option<bool>) {
        let last = self.raw.chars().last();
        match (sentence, last) {
            (Some(true), Some(',')) => {
                self.raw.pop();
                self.raw.push('.');
            }
            (Some(true), Some(c)) if c.is_alphanumeric() => self.raw.push('.'),
            // Not "etc." or "...", where the period belongs to the word.
            (Some(false), Some('.'))
                if !self.raw.ends_with("..") && !ends_with_abbreviation(&self.raw) =>
            {
                self.raw.pop();
                self.raw.push(',');
            }
            _ => {}
        }
    }
}

/// Whether `words` begin a sentence (a capital) or continue one (lower case).
/// None for "I", which is capitalized either way, and scripts without case.
fn starts_sentence(words: &str) -> Option<bool> {
    let first = words.split_whitespace().next()?;
    let letter = first.chars().find(|c| c.is_alphabetic())?;
    let pronoun = first.trim_start_matches(|c: char| !c.is_alphabetic());
    if pronoun == "I" || pronoun.starts_with("I'") || pronoun.starts_with("I’") {
        return None;
    }
    if letter.is_uppercase() {
        Some(true)
    } else if letter.is_lowercase() {
        Some(false)
    } else {
        None
    }
}

/// "p.m." or "e.g.": a period inside the last word.
fn ends_with_abbreviation(text: &str) -> bool {
    text.split_whitespace()
        .last()
        .is_some_and(|word| word.trim_end_matches('.').contains('.'))
}

/// Transcribes the recording behind the speaker, a part at a time.
pub struct Prefetch {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Heard>>,
}

impl Prefetch {
    pub fn spawn(app: AppHandle, monitor: Monitor, settings: Settings) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::Builder::new()
            .name("prefetch".into())
            .spawn(move || run(&app, &monitor, &settings, &stopped))?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }

    /// Stops, after the part being transcribed, and returns what was heard.
    /// Nothing, if the worker crashed: the whole recording is transcribed then.
    pub fn finish(mut self) -> Heard {
        self.stop.store(true, Ordering::SeqCst);
        let worker = self.worker.take().expect("prefetch finishes once");
        // Wake it from its poll wait rather than letting release wait it out.
        worker.thread().unpark();
        worker.join().unwrap_or_else(|_| {
            eprintln!("[stream] the worker crashed");
            Heard::default()
        })
    }
}

impl Drop for Prefetch {
    /// A discarded recording: the worker stops after the part it's on.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn run(app: &AppHandle, monitor: &Monitor, settings: &Settings, stop: &AtomicBool) -> Heard {
    let mut heard = Heard::default();
    let Some(model) = models::find(&settings.selected_model) else {
        return heard;
    };
    // Picks the dictionary words to hint. Found before the first part, which
    // needs five seconds of speech anyway.
    let category = tone::Detection::start()
        .app()
        .map_or(AppCategory::Other, |app| {
            tone::classify(&app, &settings.tone.apps)
        });
    let mut pending = Vec::new();
    let mut offset = 0;
    // Whisper mode: pauses are judged on amplified audio, as in `live`.
    let mut booster = Booster::default();

    while !stop.load(Ordering::SeqCst) {
        thread::park_timeout(POLL);
        let samples = monitor.read(offset);
        offset += samples.len();
        booster.push(&samples);
        pending.extend(samples);
        let gain = if settings.boost_quiet_speech {
            booster.gain()
        } else {
            1.0
        };
        // Once stopped, what's left is transcribed in one pass on release.
        while !stop.load(Ordering::SeqCst) {
            let audio = gain::amplify(&pending, gain);
            let Some(end) = live::phrase_end(&PARTS, &audio, false) else {
                break;
            };
            let carry = live::overlap_at(&PARTS, &audio, end);
            let part = &audio[..end];
            if live::has_speech(&part[heard.replayed.min(part.len())..]) {
                let words = panic::catch_unwind(AssertUnwindSafe(|| {
                    transcribe(app, settings, model, category, part)
                }));
                match words {
                    // A cut without a pause isn't the end of a sentence.
                    Ok(Ok(words)) if carry > 0 => heard.append(words.trim_end_matches('.')),
                    Ok(Ok(words)) => heard.append(&words),
                    Ok(Err(error)) => {
                        eprintln!("[stream] {error}");
                        return heard;
                    }
                    Err(_) => {
                        eprintln!("[stream] the engine crashed");
                        return heard;
                    }
                }
            }
            drop(audio);
            pending.drain(..end - carry);
            heard.start += end - carry;
            heard.replayed = carry;
        }
    }
    heard
}

fn transcribe(
    app: &AppHandle,
    settings: &Settings,
    model: &models::ModelInfo,
    category: AppCategory,
    samples: &[f32],
) -> Result<String, String> {
    let state = app.state::<AppState>();
    let mut engine = state.engine.lock_unpoisoned();
    state.load_model(app, &mut engine, model)?;
    let prompt = text::prompt(&settings.dictionary, "", category);
    engine.transcribe(samples, &settings.language, &prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_end_at_pauses_once_long_enough() {
        let mut speech = vec![0.08; RATE * 4];
        speech.extend(vec![0.0; PARTS.quiet]);
        assert_eq!(live::phrase_end(&PARTS, &speech, false), None);
        let mut longer = vec![0.08; RATE * 6];
        longer.extend(vec![0.0; PARTS.quiet]);
        assert_eq!(live::phrase_end(&PARTS, &longer, false), Some(longer.len()));
        assert_eq!(live::overlap_at(&PARTS, &longer, longer.len()), 0);
    }

    #[test]
    fn long_speech_without_a_pause_is_cut_with_overlap() {
        let speech = vec![0.08; RATE * 30];
        let end = live::phrase_end(&PARTS, &speech, false).unwrap();
        assert!(end <= PARTS.max);
        assert_eq!(live::overlap_at(&PARTS, &speech, end), PARTS.overlap);
    }

    #[test]
    fn replayed_words_are_not_repeated() {
        let mut heard = Heard::default();
        heard.append("so the plan for");
        heard.replayed = PARTS.overlap;
        heard.append("for tomorrow is simple.");
        assert_eq!(heard.raw, "so the plan for tomorrow is simple.");
        heard.replayed = 0;
        heard.append("  ");
        heard.append("Yes yes.");
        assert_eq!(heard.raw, "so the plan for tomorrow is simple. Yes yes.");
    }

    #[test]
    fn the_next_part_decides_whether_a_sentence_ended() {
        let joined = |parts: &[&str]| {
            let mut heard = Heard::default();
            for part in parts {
                heard.append(part);
            }
            heard.raw
        };
        assert_eq!(
            joined(&["We finished last week,", "We start again Monday."]),
            "We finished last week. We start again Monday."
        );
        assert_eq!(
            joined(&["We finished last week.", "and it went well."]),
            "We finished last week, and it went well."
        );
        assert_eq!(
            joined(&["We finished last week", "Then we rested"]),
            "We finished last week. Then we rested"
        );
        // Periods that belong to a word, "I", and text without case stay as they are.
        assert_eq!(
            joined(&["Meet at 3 p.m.", "sharp."]),
            "Meet at 3 p.m. sharp."
        );
        assert_eq!(joined(&["Wait for it...", "now."]), "Wait for it... now.");
        assert_eq!(joined(&["So,", "I think so."]), "So, I think so.");
        assert_eq!(joined(&["So,", "I'm sure."]), "So, I'm sure.");
        assert_eq!(joined(&["你好,", "世界。"]), "你好, 世界。");
        assert_eq!(
            joined(&["Call me,", "\"Now\" he said."]),
            "Call me. \"Now\" he said."
        );
    }

    /// Compares the wait after release with and without streaming. The parts
    /// are transcribed as the audio arrives; only the rest is timed.
    #[test]
    #[ignore = "requires WORDFLOW_TEST_MODELS, WORDFLOW_TEST_AUDIO and a downloaded model"]
    fn streaming_leaves_little_to_transcribe_on_release() {
        use std::time::Instant;
        let model_id = std::env::var("WORDFLOW_TEST_MODEL").unwrap_or("base".into());
        let models = std::env::var("WORDFLOW_TEST_MODELS").expect("model folder");
        let audio = std::env::var("WORDFLOW_TEST_AUDIO").expect("speech recording");
        let model = models::find(&model_id).expect("catalog model");
        let store = models::ModelStore::new(models.into());
        let mut engine = crate::engine::Engine::default();
        crate::engine::silence_logs();
        engine
            .load(model, &store.path(model))
            .expect("load offline model");
        let (samples, _) =
            crate::media::decode_file(std::path::Path::new(&audio)).expect("decode audio");
        let seconds = samples.len() as f64 / RATE as f64;

        let started = Instant::now();
        let whole = engine.transcribe(&samples, "en", "").expect("whole");
        let without = started.elapsed();

        let mut heard = Heard::default();
        let mut pending = Vec::new();
        let mut busy = Duration::ZERO;
        for block in samples.chunks(RATE / 4) {
            pending.extend_from_slice(block);
            while let Some(end) = live::phrase_end(&PARTS, &pending, false) {
                let carry = live::overlap_at(&PARTS, &pending, end);
                let started = Instant::now();
                let words = engine.transcribe(&pending[..end], "en", "").expect("part");
                busy += started.elapsed();
                heard.append(if carry > 0 {
                    words.trim_end_matches('.')
                } else {
                    &words
                });
                pending.drain(..end - carry);
                heard.start += end - carry;
                heard.replayed = carry;
            }
        }
        let started = Instant::now();
        let rest = engine
            .transcribe(&samples[heard.start..], "en", "")
            .expect("rest");
        heard.append(&rest);
        let with = started.elapsed();

        println!("{seconds:.1}s of audio with {model_id}");
        println!(
            "without streaming: {without:.2?} after release\n  {}",
            whole.trim()
        );
        println!(
            "with streaming: {with:.2?} after release ({:.1}s left; {busy:.2?} spent while speaking)\n  {}",
            (samples.len() - heard.start) as f64 / RATE as f64,
            heard.raw
        );
        assert!(
            busy.as_secs_f64() < seconds,
            "must keep up with the speaker"
        );
        assert!(with < without);
    }
}
