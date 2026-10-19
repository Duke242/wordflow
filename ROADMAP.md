# WordFlow: what's left

The engineering checklist for WordFlow.

## Core

### Dictation
- [x] Hold-to-talk and toggle modes, plus "Hold or tap" (a quick tap keeps recording until the next press)
- [x] Fn and single-modifier hotkeys (Right ⌘, Left ⌥…)
- [x] Custom shortcut hotkeys
- [x] Escape stops without pasting, keeps the transcript in History
- [x] Another key while holding the hotkey cancels
- [x] Fn doesn't open the emoji picker
- [x] Paste into the focused app, restore the clipboard afterwards
- [x] Load the model while the user is still speaking
- [ ] Switching microphone mid-recording restarts the recording
- [ ] Accessibility warning the first time dictation can't paste

### Recorder pill
- [x] Recording, transcribing, loading and message states
- [x] Live waveform and elapsed time
- [x] Nine screen positions, always-show option
- [x] Hover controls: microphone, mode, language
- [x] Click the dot to stop
- [x] Right-click menu to switch model
- [ ] Frosted idle look in light mode

### Transcription
- [x] Whisper models: download, cancel, delete, select
- [x] Recommended model for this machine
- [x] Language choice with recent languages
- [x] Filler-word removal, smart trailing punctuation, dictionary rules
- [x] Parakeet v2 (English) and v3 (25 languages), on CPU, about 25–30x real time on an M3 Pro
- [x] Granite Speech 4.1 2B (6 languages), through llama.cpp in the helper program: about half Parakeet's errors on LibriSpeech test-other, with dictionary words as keywords
- [x] Neural Engine acceleration for Whisper on Apple Silicon (15–30% faster; optional for Turbo)
- [x] Transcribe while recording, at pauses, so only the last few seconds are left on release
- [ ] Parakeet on the Neural Engine (CoreML through ONNX Runtime was 3–4x slower than CPU)

### App
- [x] Onboarding: welcome, permissions, model, hotkey
- [x] Dashboard, History (search, playback), Dictionary, Statistics, Settings
- [x] Light and dark themes
- [x] Update check against GitHub releases
- [x] Launch at login

## Planned
- [x] Install updates in place (signed with the updater key; builds without one open the release page)
- [x] Menu bar panel: dictate, today's stats, quick model/language/mic/mode switches, recent transcripts
- [ ] Hide the Dock icon when the window is closed
- [x] Signed and notarized macOS build, universal (Apple Silicon and Intel; Parakeet on Apple Silicon only)
- [ ] Signed Windows installer
- [x] CI builds for macOS, Windows and Linux on every tag

## Beyond the basics
- [x] AI cleanup with a local language model: built in (llama.cpp, in its own helper process) or through a local server (Ollama, LM Studio)
- [x] AI cleanup through a cloud API with the user's own key and model (OpenAI, Anthropic, Gemini, Groq, Mistral, OpenRouter or any OpenAI-compatible API)
- [ ] AI cleanup tested with real models on macOS, Windows and Linux, and the prompt tuned from what people dictate
- [x] Tone that fits the app: chat, email, terminal and code editor styles, with a per-app override list
- [x] Spoken file names, identifiers and flags written as code in editors and terminals
- [x] Command mode: select text, hold a shortcut and say how to change it
- [ ] Tone and command mode tested on macOS and Windows (app detection, copying the selection)
- [ ] Identify the focused app in native Wayland sessions, which command mode needs to copy safely
- [ ] Single-modifier keys (Right ⌥…) for the command shortcut
- [ ] Windows build, tested
- [ ] Linux build, tested (X11 and Wayland)
- [ ] Single-modifier hotkeys on Windows and Linux
- [x] Voice commands ("scratch that", "new line", "new paragraph"), mid-sentence corrections ("at 3, actually 4"), and spoken lists, numbers, dates, times, money and percentages, for English
- [ ] The same rules for other languages
- [x] Snippets: a spoken phrase that expands into saved text (#167)
- [x] Finish and send: Return while recording inserts the transcript and presses Return
- [x] Pause music and videos while dictating, browsers included, and resume them afterwards (#63)
- [x] Share the dictionary and snippets between computers or with a team, through a synced folder; export and import them as a file
- [ ] Sharing through a WordFlow account, without a synced folder

### Meeting notes
- [x] Record the microphone and system audio as separate tracks, labelled "You" and "Others" (#170)
- [x] Live transcript in 15–30 s windows cut at pauses, streamed to disk for hour-long meetings (#171)
- [x] Summary, action items and open questions by the cleanup model, map-reduce for long meetings; copy or save as Markdown (#172)
- [x] Meetings list with search, detail page, rename, delete
- [ ] System audio tested on real machines: WASAPI loopback (Windows), process taps (macOS 14.2+), PulseAudio/PipeWire monitors (Linux)
- [ ] Play a meeting's recording from its page (playback currently loads whole files, too large for long meetings)
- [ ] Speaker diarization within each track, beyond "You" and "Others"
- [ ] Echo cancellation instead of dropping microphone audio while the speakers are louder
