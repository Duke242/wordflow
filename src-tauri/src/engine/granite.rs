//! IBM Granite Speech, a language model that listens: run through llama.cpp by
//! the helper program (see `helper`), in its own process like cleanup models.
//! Dictionary words go to it as keywords, which it spells as given.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use base64::Engine as _;
use serde::Serialize;

use crate::{audio::WHISPER_SAMPLE_RATE, helper::Worker, models::GRANITE_FILES, text::Prompt};

const RATE: usize = WHISPER_SAMPLE_RATE as usize;
/// The first load on a computer also compiles the GPU's shaders, about 30
/// seconds on an M1 Max. Later loads take a second or two.
const LOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// Recordings are transcribed in parts of at most this long, cut at the
/// quietest moment near the end of each, as the model was trained on short clips.
const MAX_PART: usize = RATE * 30;
/// Where in each part to look for a pause to cut at: its last this many samples.
const CUT_WINDOW: usize = RATE * 8;
const FRAME: usize = RATE / 50;
/// Very short clips are padded with silence up to this length.
const MIN_SAMPLES: usize = RATE;

const INSTRUCTION: &str = "transcribe the speech with proper punctuation and capitalization.";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> {
    model: &'a Path,
    projector: &'a Path,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<String>,
    prompt: &'a str,
    max_tokens: u32,
}

pub struct Granite {
    worker: Worker,
    model: PathBuf,
    projector: PathBuf,
}

impl Granite {
    /// `dir` holds the model and its audio projector.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let [model, projector] = GRANITE_FILES.map(|file| dir.join(file));
        let worker = start(&model, &projector)?;
        Ok(Self {
            worker,
            model,
            projector,
        })
    }

    pub fn transcribe(&mut self, samples: &[f32], prompt: &Prompt) -> Result<String, String> {
        let instruction = instruction(prompt);
        let mut texts = Vec::new();
        for part in parts(samples) {
            // Given silence or a quiet room, Granite makes up a sentence, often in
            // another language, where Whisper and Parakeet write nothing.
            if !crate::live::has_speech(&samples[part.clone()]) {
                continue;
            }
            let text = self.run(&samples[part], &instruction)?;
            if !text.is_empty() {
                texts.push(text);
            }
        }
        Ok(texts.join(" "))
    }

    fn run(&mut self, samples: &[f32], instruction: &str) -> Result<String, String> {
        // A helper that crashed or hung last time is replaced.
        if self.worker.broken || !self.worker.is_alive() {
            self.worker = start(&self.model, &self.projector)?;
        }
        let mut bytes = Vec::with_capacity(samples.len().max(MIN_SAMPLES) * 4);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        if bytes.len() < MIN_SAMPLES * 4 {
            bytes.resize(MIN_SAMPLES * 4, 0);
        }
        let seconds = samples.len() as u64 / RATE as u64 + 1;
        let request = Request {
            model: &self.model,
            projector: &self.projector,
            audio: Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
            prompt: instruction,
            // Speech is rarely more than 4 tokens a second.
            max_tokens: 64 + 10 * seconds as u32,
        };
        let timeout = Duration::from_secs(20 + 2 * seconds);
        self.worker
            .call(&request, timeout)
            .map(|text| text.trim().to_string())
    }
}

/// Starts a helper with the model loaded.
fn start(model: &Path, projector: &Path) -> Result<Worker, String> {
    let mut worker = Worker::spawn(0, "speech")?;
    let request = Request {
        model,
        projector,
        audio: None,
        prompt: "",
        max_tokens: 0,
    };
    worker.call(&request, LOAD_TIMEOUT)?;
    Ok(worker)
}

/// Small words trimmed from the ends of keywords. Granite expects to hear each
/// keyword, so with "a PR" it wrote "A." at the start of dictations.
const FILLER_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "to", "in", "on", "at", "for", "my", "your",
];

/// The model's instruction, with the dictionary's words as keywords. Words with
/// angle brackets are left out: the helper reads the prompt's special tokens
/// and audio marker, and the dictionary can come from a shared sync folder.
fn instruction(prompt: &Prompt) -> String {
    let mut words: Vec<String> = Vec::new();
    for word in &prompt.words {
        let word = keyword(word);
        if !word.is_empty()
            && !word.contains(['<', '>'])
            && !words.iter().any(|w| w.eq_ignore_ascii_case(&word))
        {
            words.push(word);
        }
    }
    if words.is_empty() {
        return INSTRUCTION.to_string();
    }
    format!("{INSTRUCTION} Keywords: {}", words.join(", "))
}

/// `word` without small words at either end: "a PR" is "PR".
fn keyword(word: &str) -> String {
    let parts: Vec<&str> = word.split_whitespace().collect();
    let is_filler = |part: &&str| FILLER_WORDS.iter().any(|f| f.eq_ignore_ascii_case(part));
    let start = parts
        .iter()
        .position(|p| !is_filler(p))
        .unwrap_or(parts.len());
    let end = parts
        .iter()
        .rposition(|p| !is_filler(p))
        .map_or(start, |i| i + 1);
    parts[start..end].join(" ")
}

/// `samples` split into parts of at most `MAX_PART`, each cut at its quietest
/// frame near the end so words aren't split.
fn parts(samples: &[f32]) -> Vec<std::ops::Range<usize>> {
    let mut parts = Vec::new();
    let mut start = 0;
    while samples.len() - start > MAX_PART {
        let end = start + MAX_PART;
        let quietest = (end - CUT_WINDOW..end)
            .step_by(FRAME)
            .min_by(|&a, &b| {
                energy(&samples[a..a + FRAME]).total_cmp(&energy(&samples[b..b + FRAME]))
            })
            .unwrap_or(end);
        parts.push(start..quietest);
        start = quietest;
    }
    parts.push(start..samples.len());
    parts
}

fn energy(frame: &[f32]) -> f32 {
    frame.iter().map(|s| s * s).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_words_become_keywords() {
        assert_eq!(instruction(&Prompt::NONE), INSTRUCTION);
        let prompt = Prompt {
            words: vec!["Kubernetes".into(), "Karan".into()],
            previous: "so we deployed it".into(),
        };
        assert_eq!(
            instruction(&prompt),
            format!("{INSTRUCTION} Keywords: Kubernetes, Karan")
        );
        let tokens = Prompt {
            words: vec!["<__media__>".into(), "<|end_of_text|>".into(), "C++".into()],
            previous: String::new(),
        };
        assert_eq!(instruction(&tokens), format!("{INSTRUCTION} Keywords: C++"));
    }

    #[test]
    fn small_words_are_trimmed_from_keywords() {
        let prompt = Prompt {
            words: ["a PR", "Wisper Flow", "the", "Bank of America", "pr"]
                .map(String::from)
                .to_vec(),
            previous: String::new(),
        };
        assert_eq!(
            instruction(&prompt),
            format!("{INSTRUCTION} Keywords: PR, Wisper Flow, Bank of America")
        );
    }

    #[test]
    fn long_recordings_are_cut_at_a_pause() {
        let whole = |samples: &[f32]| {
            let parts = parts(samples);
            parts.len() == 1 && parts[0] == (0..samples.len())
        };
        assert!(whole(&[0.0; 100]));
        assert!(whole(&vec![0.5; MAX_PART]));

        let mut samples = vec![0.5; MAX_PART * 2];
        let pause = MAX_PART - RATE * 3;
        samples[pause..pause + FRAME].fill(0.0);
        let parts = parts(&samples);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], 0..pause);
        assert!(parts.iter().all(|p| p.len() <= MAX_PART));
        assert_eq!(parts.last().unwrap().end, samples.len());
    }
}
