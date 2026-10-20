//! WordFlow's llama.cpp helper: runs a local language model for transcript
//! cleanup, or Granite Speech for transcription.
//!
//! The app starts this program when transcript cleanup is on or a Granite model
//! is chosen, and talks to it over stdin and stdout, one JSON object per line:
//!
//! - cleanup: `{"model": "/path/model.gguf", "messages": [{"role": "system", "content": "…"}, …], "maxTokens": 512}`
//! - speech: `{"model": "/path/model.gguf", "projector": "/path/mmproj.gguf", "audio": "<base64>", "prompt": "…", "maxTokens": 512}`,
//!   where `audio` is 16 kHz mono samples as little-endian f32
//! - reply: `{"text": "…"}` or `{"error": "…"}`. Speech replies also carry
//!   `"confidence"`: the average log-probability of the tokens written, low when
//!   the model is making text up for silence or noise.
//!
//! A request without messages or audio only loads the model (and projector), so
//! the first dictation doesn't wait for it. The model stays loaded until another
//! one is asked for or stdin closes. Logs go to stderr.

use std::{
    io::{self, BufRead, Write},
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use base64::Engine as _;
use llama_cpp_2::{
    LogOptions,
    context::{LlamaContext, params::LlamaContextParams},
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{LlamaChatMessage, LlamaChatTemplate, LlamaModel, params::LlamaModelParams},
    mtmd::{MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText, mtmd_default_marker},
    sampling::LlamaSampler,
};
use serde::{Deserialize, Serialize};

/// Room for the prompt plus the reply. Dictations are short; longer ones are
/// refused by the app before they get here.
const MAX_CONTEXT: u32 = 8192;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    model: PathBuf,
    #[serde(default)]
    messages: Vec<Message>,
    /// The audio projector that turns sound into the speech model's input.
    projector: Option<PathBuf>,
    /// Base64 of 16 kHz mono f32 samples, little-endian.
    audio: Option<String>,
    /// What to do with the audio, such as "transcribe the speech…".
    #[serde(default)]
    prompt: String,
    #[serde(default = "default_max_tokens")]
    max_tokens: u32,
}

fn default_max_tokens() -> u32 {
    512
}

#[derive(Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Reply {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confidence: Option<f32>,
}

/// What the model wrote, with the average log-probability of its tokens.
struct Generated {
    text: String,
    confidence: f32,
}

struct Loaded {
    path: PathBuf,
    projector: Option<PathBuf>,
    // Declared before the model so it's freed first: it points into the model.
    audio: Option<MtmdContext>,
    model: LlamaModel,
}

fn main() {
    if std::env::var_os("WORDFLOW_CLEANUP_LOGS").is_none() {
        // Unlike `LlamaBackend::void_logs`, this also quiets the audio projector,
        // and set first, it also covers starting the GPU.
        llama_cpp_2::send_logs_to_tracing(LogOptions::default().with_logs_enabled(false));
    }
    let backend = match LlamaBackend::init() {
        Ok(backend) => backend,
        Err(e) => {
            eprintln!("[cleanup] couldn't start llama.cpp: {e}");
            std::process::exit(1);
        }
    };

    let mut loaded: Option<Loaded> = None;
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(request) => handle(&backend, &mut loaded, &request),
            Err(e) => Err(format!("Unreadable request: {e}")),
        };
        let reply = match reply {
            Ok((text, confidence)) => Reply {
                text: Some(text),
                error: None,
                confidence,
            },
            Err(error) => Reply {
                text: None,
                error: Some(error),
                confidence: None,
            },
        };
        let written = serde_json::to_writer(&mut stdout, &reply)
            .map_err(io::Error::other)
            .and_then(|()| stdout.write_all(b"\n"))
            .and_then(|()| stdout.flush());
        if written.is_err() {
            // The app has gone away.
            break;
        }
    }
}

fn handle(
    backend: &LlamaBackend,
    loaded: &mut Option<Loaded>,
    request: &Request,
) -> Result<(String, Option<f32>), String> {
    let loaded = load(
        backend,
        loaded,
        &request.model,
        request.projector.as_deref(),
    )?;
    if let Some(audio) = &request.audio {
        let samples = decode_samples(audio)?;
        let projector = loaded
            .audio
            .as_mut()
            .ok_or("Audio needs a projector to go with the model")?;
        let generated = transcribe(
            backend,
            &loaded.model,
            projector,
            &samples,
            &request.prompt,
            request.max_tokens,
        )?;
        return Ok((generated.text, Some(generated.confidence)));
    }
    if request.messages.is_empty() {
        return Ok((String::new(), None));
    }
    let generated = generate(
        backend,
        &loaded.model,
        &request.messages,
        request.max_tokens,
    )?;
    Ok((generated.text, None))
}

/// The model at `path`, with `projector` for audio, loading them unless
/// they're the ones already in memory.
fn load<'a>(
    backend: &LlamaBackend,
    loaded: &'a mut Option<Loaded>,
    path: &Path,
    projector: Option<&Path>,
) -> Result<&'a mut Loaded, String> {
    if loaded
        .as_ref()
        .is_none_or(|l| l.path != path || l.projector.as_deref() != projector)
    {
        // Free the old model before loading the new one, so both are never in memory.
        *loaded = None;
        if !path.is_file() {
            return Err(format!("The model is missing: {}", path.display()));
        }
        let model = LlamaModel::load_from_file(backend, path, &LlamaModelParams::default())
            .map_err(|e| format!("Couldn't load the model: {e}"))?;
        let audio = projector
            .map(|projector| load_projector(&model, projector))
            .transpose()?;
        *loaded = Some(Loaded {
            path: path.to_path_buf(),
            projector: projector.map(Path::to_path_buf),
            audio,
            model,
        });
    }
    Ok(loaded.as_mut().expect("loaded above"))
}

fn load_projector(model: &LlamaModel, path: &Path) -> Result<MtmdContext, String> {
    if !path.is_file() {
        return Err(format!(
            "The audio projector is missing: {}",
            path.display()
        ));
    }
    let params = MtmdContextParams {
        n_threads: thread_count(),
        print_timings: false,
        ..MtmdContextParams::default()
    };
    let context = MtmdContext::init_from_file(&path.to_string_lossy(), model, &params)
        .map_err(|e| format!("Couldn't load the audio projector: {e}"))?;
    if !context.support_audio() {
        return Err("This projector doesn't take audio".into());
    }
    Ok(context)
}

fn decode_samples(audio: &str) -> Result<Vec<f32>, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(audio)
        .map_err(|e| format!("Unreadable audio: {e}"))?;
    if bytes.len() % 4 != 0 {
        return Err("Unreadable audio: not a whole number of samples".into());
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect())
}

/// The speech model's reply to `instruction` about `samples`. The prompt follows
/// Granite Speech's chat template, which llama.cpp's built-in templates don't know.
fn transcribe(
    backend: &LlamaBackend,
    model: &LlamaModel,
    projector: &mut MtmdContext,
    samples: &[f32],
    instruction: &str,
    max_tokens: u32,
) -> Result<Generated, String> {
    let bitmap = MtmdBitmap::from_audio_data(samples)
        .map_err(|e| format!("Couldn't read the audio: {e}"))?;
    let text = MtmdInputText {
        text: format!("USER: {}{instruction}\n ASSISTANT:", mtmd_default_marker()),
        add_special: true,
        parse_special: true,
    };
    let chunks = projector
        .tokenize(text, &[&bitmap])
        .map_err(|e| format!("Couldn't prepare the audio: {e}"))?;
    let prompt_len = u32::try_from(chunks.total_positions()).map_err(|e| e.to_string())?;
    let n_ctx = (prompt_len + max_tokens).min(model.n_ctx_train());
    if prompt_len >= n_ctx {
        return Err("The recording is too long for the model".into());
    }
    let mut ctx = new_context(backend, model, n_ctx)?;
    let position = chunks
        .eval_chunks(projector, &mut ctx, 0, 0, n_ctx as i32, true)
        .map_err(|e| format!("Couldn't run the model on the audio: {e}"))?;
    sample(model, &mut ctx, position, n_ctx, -1)
}

fn new_context<'a>(
    backend: &LlamaBackend,
    model: &'a LlamaModel,
    n_ctx: u32,
) -> Result<LlamaContext<'a>, String> {
    let threads = thread_count();
    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_batch(n_ctx)
        .with_n_ubatch(n_ctx)
        .with_n_threads(threads)
        .with_n_threads_batch(threads);
    model
        .new_context(backend, params)
        .map_err(|e| format!("Couldn't prepare the model: {e}"))
}

/// Generates greedily from `position` until the model stops or the context is
/// full. `last` is the index of the logits to sample first.
fn sample(
    model: &LlamaModel,
    ctx: &mut LlamaContext,
    mut position: i32,
    n_ctx: u32,
    mut last: i32,
) -> Result<Generated, String> {
    let vocab = model.vocab();
    // Greedy: cleanup and transcripts should be the same every time, not creative.
    let mut sampler = LlamaSampler::greedy();
    let mut batch = LlamaBatch::new(1, 1);
    let mut output = Vec::new();
    let mut log_probability = 0.0;
    let mut tokens = 0;
    while (position as u32) < n_ctx {
        let token = sampler.sample(ctx, last);
        if vocab.is_eog(token) {
            break;
        }
        log_probability += token_log_probability(ctx.get_logits_ith(last), token.0);
        tokens += 1;
        vocab.token_to_piece_into(token, &mut output, false, None);
        batch.clear();
        batch
            .add(token, position, &[0], true)
            .map_err(|e| e.to_string())?;
        ctx.decode(&mut batch).map_err(|e| e.to_string())?;
        last = 0;
        position += 1;
    }
    Ok(Generated {
        text: String::from_utf8_lossy(&output).into_owned(),
        confidence: if tokens == 0 {
            0.0
        } else {
            log_probability / tokens as f32
        },
    })
}

/// The log-probability of `token` under `logits`, by a stable log-softmax.
fn token_log_probability(logits: &[f32], token: i32) -> f32 {
    let Some(&logit) = usize::try_from(token).ok().and_then(|i| logits.get(i)) else {
        return f32::NEG_INFINITY;
    };
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let sum: f32 = logits.iter().map(|l| (l - max).exp()).sum();
    logit - max - sum.ln()
}

/// The prompt Gemma 4's template writes for a plain chat with thinking off.
/// The start token is added when it's tokenized.
fn gemma4_prompt(messages: &[Message]) -> String {
    let mut prompt = String::new();
    for message in messages {
        let role = match message.role.as_str() {
            "assistant" => "model",
            role => role,
        };
        prompt.push_str(&format!(
            "<|turn>{role}\n{}<turn|>\n",
            message.content.trim()
        ));
    }
    prompt.push_str("<|turn>model\n");
    prompt
}

fn generate(
    backend: &LlamaBackend,
    model: &LlamaModel,
    messages: &[Message],
    max_tokens: u32,
) -> Result<Generated, String> {
    let chat = messages
        .iter()
        .map(|m| LlamaChatMessage::new(m.role.clone(), m.content.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    // Every catalog model has its template built in; ChatML is the common fallback.
    let template = model.chat_template(None).or_else(|_| {
        LlamaChatTemplate::new("chatml").map_err(|e| format!("Bad chat template: {e}"))
    })?;
    let source = template.to_str().unwrap_or_default();
    let mut prompt = match model.apply_chat_template(&template, &chat, true) {
        Ok(prompt) => prompt,
        // llama.cpp doesn't know Gemma 4's turns yet.
        Err(_) if source.contains("<|turn>") => gemma4_prompt(messages),
        Err(e) => return Err(format!("Couldn't build the prompt: {e}")),
    };
    // Qwen 3 and 3.5 think before answering unless the reply starts with an
    // empty thought, which their own templates add when thinking is off.
    // Cleanup never wants the thinking: it's slow, and runs out of tokens.
    if source.contains("enable_thinking") && source.contains("<think>") {
        prompt.push_str("<think>\n\n</think>\n\n");
    }

    let vocab = model.vocab();
    let mut tokens = vocab.tokenize(prompt.as_bytes(), true, true);
    // Templates that write the start token themselves would otherwise get two.
    if tokens.len() > 1 && tokens[0] == vocab.bos() && tokens[1] == vocab.bos() {
        tokens.remove(0);
    }
    if tokens.is_empty() {
        return Err("The prompt came out empty".into());
    }
    let prompt_len = u32::try_from(tokens.len()).map_err(|e| e.to_string())?;
    let n_ctx = (prompt_len + max_tokens)
        .min(MAX_CONTEXT)
        .min(model.n_ctx_train());
    if prompt_len >= n_ctx {
        return Err("The transcript is too long to clean up".into());
    }

    let mut ctx = new_context(backend, model, n_ctx)?;

    let mut batch = LlamaBatch::new(tokens.len().max(1), 1);
    let last = tokens.len() - 1;
    for (i, token) in tokens.iter().enumerate() {
        batch
            .add(*token, i as i32, &[0], i == last)
            .map_err(|e| e.to_string())?;
    }
    ctx.decode(&mut batch).map_err(|e| e.to_string())?;
    sample(
        model,
        &mut ctx,
        prompt_len as i32,
        n_ctx,
        batch.n_tokens() - 1,
    )
}

/// llama.cpp gains little past 8 threads, and more would starve the rest of the system.
fn thread_count() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get().min(8) as i32)
        .unwrap_or(4)
}
