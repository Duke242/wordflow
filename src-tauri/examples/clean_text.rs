//! Runs transcript cleanup on some text, without the UI, to try models and prompts.
//!
//!     cargo run --release --example clean_text -- download <cleanup-model-id> <models-dir>
//!     cargo run --release --example clean_text -- <cleanup-model-id> <models-dir> "<text>" [runs]
//!     cargo run --release --example clean_text -- server <url> <model> "<text>" [runs]
//!
//! `@<file>` in place of the text cleans up each line of the file in turn.
//!
//! Built-in models need the helper: build cleanup-worker/ first.

use wordflow_lib::devtools::{self, CleanupBackend, CleanupSettings};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let runs = |rest: &[String]| rest.first().and_then(|r| r.parse().ok()).unwrap_or(3);
    let texts = |text: &str| -> Result<Vec<String>, String> {
        match text.strip_prefix('@') {
            Some(path) => Ok(std::fs::read_to_string(path)
                .map_err(|e| format!("{path}: {e}"))?
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(String::from)
                .collect()),
            None => Ok(vec![text.to_string()]),
        }
    };
    match args.as_slice() {
        [_, command, model, dir] if command == "download" => {
            devtools::download(model, dir.as_ref(), None)
        }
        [_, command, url, model, text, rest @ ..] if command == "server" => {
            let settings = CleanupSettings {
                enabled: true,
                backend: CleanupBackend::Server,
                server_url: url.clone(),
                server_model: model.clone(),
                ..CleanupSettings::default()
            };
            devtools::cleanup(&settings, "".as_ref(), &texts(text)?, runs(rest))
        }
        [_, model, dir, text, rest @ ..] => {
            let settings = CleanupSettings {
                enabled: true,
                backend: CleanupBackend::Builtin,
                builtin_model: model.clone(),
                ..CleanupSettings::default()
            };
            devtools::cleanup(&settings, dir.as_ref(), &texts(text)?, runs(rest))
        }
        _ => Err(
            "usage: clean_text <cleanup-model-id> <models-dir> \"<text>\" [runs]\n       \
                  clean_text server <url> <model> \"<text>\" [runs]"
                .into(),
        ),
    }
}
