# WordFlow

WordFlow is a dictation app for macOS, Windows and Linux.

## Features

- **Live typing** — inserts text at pauses while you speak.
- **Finish and send** — press Return while recording to insert and send in one go.
- **Tone that fits the app** — adjusts punctuation and style for chat, email, terminals and code editors, and writes spoken code as code.
- **Command mode** — select text, hold a shortcut and say how to change it.
- **Commands and formatting** — "scratch that", "new line", spoken lists, numbers, dates and prices.
- **Sharing** — sync the dictionary and snippets through a shared folder.
- **Meetings** — records and transcribes calls, with notes and action items.

Details for each are in [docs/features.md](docs/features.md). To contribute, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Develop

On a new Mac, this one command installs everything, clones the repo to `~/wordflow` and starts the app:

```sh
curl -fsSL https://raw.githubusercontent.com/Duke242/wordflow/main/scripts/bootstrap.sh | bash
```

Otherwise, WordFlow requires Node 22+, a current stable Rust toolchain and CMake. On Linux, also install the system libraries listed in `.github/actions/setup-desktop-build/action.yml`.

```sh
npm run setup      # first time: install, build the cleanup helper and start the app
npm run app        # run with live reload
npm run app:build  # build installers into src-tauri/target/release/bundle
```

AI cleanup's built-in models run in a separate helper, [`cleanup-worker/`](cleanup-worker/README.md). Run `cargo build --release` there once and `npm run app` picks it up; without it, cleanup only works through a local server or a cloud API.

### Preview in the browser

Run `npm run dev` to design screens without the native app. The UI uses sample data.

| URL | Shows |
| --- | --- |
| http://localhost:1420 | Main window (add `?theme=dark` or `?onboarding=1`) |
| http://localhost:1420/pill.html | Recorder with a simulated waveform |
| http://localhost:1420/?meeting=recording | A meeting in progress (add `?notes=0` for no cleanup model) |

## Check

```sh
npm run typecheck && npm test                 # UI
cd src-tauri && cargo fmt --check \
  && cargo clippy --all-targets -- -D warnings \
  && cargo test                               # app core
```

Benchmark a downloaded model on an audio file:

```sh
cd src-tauri
cargo run --release --example transcribe_wav -- parakeet-tdt-v3 "<models folder>" clip.wav
```

## Release

Needs the [GitHub CLI](https://cli.github.com), signed in with `gh auth login`.

```sh
npm run release          # tag and push the next alpha, e.g. v2.0.0-alpha.5
npm run release -- beta  # or the next beta
```

CI (`.github/workflows/desktop.yml`) runs on every push to `main` and every pull request: UI checks and build, Rust format/lint/tests on all three systems, a dependency audit, and installers as artifacts.

Pushing a `v2.*` tag publishes the installers as a GitHub pre-release.

Release builds take their version from the tag and include signed update packages, which the app installs in place. Signing needs the `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets; the matching public key is in `src-tauri/tauri.conf.json`. Keep the private key safe: without it, installed copies can't be updated.

## Layout

| Path | Contents |
| --- | --- |
| `src/` | UI. Design tokens live in `src/styles/globals.css`. |
| `src-tauri/src/` | App core: audio, transcription, dictation, paste, history, models, cleanup. |
| `src-tauri/src/platform/` | OS-specific code, one folder per OS with the same functions. |
| `cleanup-worker/` | Helper that runs built-in cleanup models with llama.cpp. |
