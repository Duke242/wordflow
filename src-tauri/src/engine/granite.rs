//! IBM Granite Speech, a language model that listens: run through llama.cpp by
//! the helper program (see `helper`), in its own process like cleanup models.
//!
//! It takes no keywords. Given the dictionary's words, it dropped most
//! punctuation, wrote the words where they weren't said ("A." from "a PR"),
//! and swapped similar ones ("Wisper" for "Whisper"). The dictionary still
//! corrects its transcripts afterwards, as for the other models.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use base64::Engine as _;
use serde::Serialize;

use crate::{audio::WHISPER_SAMPLE_RATE, helper::Worker, models::GRANITE_FILES};

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
/// Transcripts with a lower average log-probability per token are dropped as
/// made up. For silence, a tap of the key or room noise, Granite writes a
/// sentence, often in another language, at -0.76 or lower; real dictations
/// measured -0.61 or higher, most above -0.3.
const MIN_CONFIDENCE: f32 = -0.7;

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

    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String, String> {
        let mut texts = Vec::new();
        for part in parts(samples) {
            // Given silence, Granite makes up a sentence where Whisper and
            // Parakeet write nothing. Quiet parts are skipped without asking it,
            // and what it isn't sure of is dropped.
            if !crate::live::has_speech(&samples[part.clone()]) {
                continue;
            }
            let (text, confidence) = self.run(&samples[part])?;
            if confidence.is_some_and(|c| c < MIN_CONFIDENCE) {
                eprintln!("[granite] dropped a transcript it wasn't sure of ({confidence:.2?})");
                continue;
            }
            if !text.is_empty() {
                texts.push(text);
            }
        }
        Ok(texts.join(" "))
    }

    fn run(&mut self, samples: &[f32]) -> Result<(String, Option<f32>), String> {
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
            prompt: INSTRUCTION,
            // Speech is rarely more than 4 tokens a second.
            max_tokens: 64 + 10 * seconds as u32,
        };
        let timeout = Duration::from_secs(20 + 2 * seconds);
        self.worker
            .call_scored(&request, timeout)
            .map(|(text, confidence)| (text.trim().to_string(), confidence))
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
