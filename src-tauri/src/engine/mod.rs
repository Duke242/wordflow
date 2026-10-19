//! Speech-to-text engines behind one interface.
//!
//! - Whisper runs through whisper.cpp. On macOS it uses Metal, and the Neural
//!   Engine for the encoder when the model's CoreML companion is downloaded.
//! - Parakeet runs through ONNX Runtime.
//! - Granite runs through llama.cpp, in the helper program.

mod granite;
#[cfg(parakeet)]
mod parakeet;
mod whisper;

use std::path::Path;

pub use whisper::silence_logs;

use crate::{
    models::{EngineKind, ModelInfo},
    text::Prompt,
};

enum Loaded {
    Whisper(whisper::Whisper),
    Granite(granite::Granite),
    #[cfg(parakeet)]
    Parakeet(parakeet::Parakeet),
}

/// At most one model in memory, with the id it was loaded as.
#[derive(Default)]
pub struct Engine {
    loaded: Option<(&'static str, Loaded)>,
}

impl Engine {
    pub fn loaded_model(&self) -> Option<&'static str> {
        self.loaded.as_ref().map(|(id, _)| *id)
    }

    /// Loads a model unless it is already the loaded one. The previous model is
    /// freed first so two large models are never in memory together.
    pub fn load(&mut self, model: &ModelInfo, path: &Path) -> Result<(), String> {
        if self.loaded_model() == Some(model.id) {
            return Ok(());
        }
        self.loaded = None;
        let loaded = match model.engine {
            EngineKind::Whisper => Loaded::Whisper(whisper::Whisper::load(path)?),
            EngineKind::Granite => Loaded::Granite(granite::Granite::load(path)?),
            #[cfg(parakeet)]
            EngineKind::Parakeet => Loaded::Parakeet(parakeet::Parakeet::load(path)?),
            #[cfg(not(parakeet))]
            EngineKind::Parakeet => return Err("Parakeet models don't run on Intel Macs".into()),
            EngineKind::Llama => return Err("This is a cleanup model, not a speech model".into()),
        };
        self.loaded = Some((model.id, loaded));
        Ok(())
    }

    pub fn unload(&mut self) {
        self.loaded = None;
    }

    /// Transcribes 16 kHz mono audio. `language` is a Whisper code or "auto",
    /// which Whisper detects for each part of a longer dictation separated by
    /// pauses. Parakeet takes no language: v3 recognizes its 25 languages
    /// without being told which, and Granite its 6. Granite takes the prompt's
    /// words as keywords; Parakeet has no way to take a `prompt`.
    pub fn transcribe(
        &mut self,
        samples: &[f32],
        language: &str,
        prompt: &Prompt,
    ) -> Result<String, String> {
        match &mut self.loaded {
            Some((_, Loaded::Whisper(model))) => {
                model.transcribe(samples, language, &prompt.text())
            }
            Some((_, Loaded::Granite(model))) => model.transcribe(samples, prompt),
            #[cfg(parakeet)]
            Some((_, Loaded::Parakeet(model))) => model.transcribe(samples),
            None => Err("No model loaded".into()),
        }
    }
}
