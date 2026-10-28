//! NVIDIA Parakeet (TDT, 0.6B) through ONNX Runtime: the int8 export, or the
//! full-precision one when a folder has no int8 files (v3, see `models`).

use std::{path::Path, sync::Once};

use transcribe_rs::onnx::{
    Quantization,
    parakeet::{ParakeetModel, ParakeetParams},
};

pub struct Parakeet {
    model: ParakeetModel,
}

/// The accelerator is a global ONNX Runtime setting, so it is chosen once.
static ACCELERATOR: Once = Once::new();

impl Parakeet {
    /// `dir` holds the encoder, decoder, preprocessor and vocabulary files.
    pub fn load(dir: &Path) -> Result<Self, String> {
        ACCELERATOR.call_once(|| {
            // WORDFLOW_ORT_CPU=1 forces CPU, for comparing against the accelerated path.
            let accelerator = if std::env::var_os("WORDFLOW_ORT_CPU").is_some() {
                transcribe_rs::OrtAccelerator::CpuOnly
            } else {
                crate::platform::ORT_ACCELERATOR
            };
            transcribe_rs::set_ort_accelerator(accelerator);
        });
        let model = ParakeetModel::load(dir, &Quantization::Int8)
            .map_err(|e| format!("Couldn't load model: {e}"))?;
        Ok(Self { model })
    }

    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String, String> {
        self.model
            .transcribe_with(samples, &ParakeetParams::default())
            .map(|result| result.text.trim().to_string())
            .map_err(|e| e.to_string())
    }
}
