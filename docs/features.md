# Features

How each feature behaves, in detail. For setup and builds, see the [README](../README.md).

## Transcribing while you speak

With Transcribe while speaking on (Settings → Behavior, off by default), dictation that's pasted when you finish is transcribed while you're still talking (`src-tauri/src/stream.rs`). Each time you pause for about 0.3 seconds after at least five seconds of speech, that part goes to the speech model in the background. Where two parts meet, the second one's first letter decides whether a sentence ended there, since the model finishing the first couldn't hear what came next. Continuous speech is cut near its quietest point at 28 seconds, and the last half second is heard again so a word split by the cut comes out whole. On release only the speech since the last cut is left, so a one-minute dictation pastes about as soon as a five-second one. Tone, AI cleanup, voice commands and the dictionary still run once over the whole transcript. Command mode and live typing don't use it.

## Live typing

Live typing is enabled in Settings → Behavior. It inserts short phrases during recording, at pauses or roughly every four seconds of continuous speech (plus model processing time). Use Fn in Hold mode on macOS, or Toggle mode with other shortcuts. Native Wayland sessions retain insertion on release. Escape stops further insertion and keeps the transcript in History; text already typed remains. Switching the active app pauses live insertion for that session. Live typing leaves the clipboard untouched.

## Finish and send

Finish and send (Settings → Behavior) sends a message in one go: press Return while recording, hands-free or with the hotkey held, and WordFlow stops, inserts the transcript and presses Return about 150 ms later. Return (and on a Mac keypad Enter, which Fn+Return gives) is only claimed while recording. With a modifier hotkey like ⌘ held, Return is part of a shortcut and cancels as any other key does; Fn isn't such a modifier, so Fn+Return sends; the Fn listener catches that Return itself, as the global shortcut doesn't fire while Fn is held. A sent dictation isn't watched for corrections, as the field is cleared.

## Paste last transcript

When a dictation lands in the wrong place, or nowhere because no field had focus, the menu bar icon's menu has Paste Last Transcript, and Settings can give it a shortcut (Ctrl+Shift+V suggested on a Mac, Ctrl+Alt+V elsewhere). It pastes the newest History item into the focused app. It does nothing while a dictation is recording or transcribing.

## Tone that fits the app

Dictation matches the app it goes into (Settings → Tone that fits the app). The focused app is identified when recording starts, by bundle id on macOS, executable on Windows and window class on X11 (native Wayland apps can't be identified), and sorted into chat, email, terminal, code editor or other, with the user's own list checked before the built-in one in `src-tauri/src/tone.rs`. With AI cleanup on, the category adds a style line next to the transcript and swaps in examples written in that style, and spoken lists stay in the sentence in chat and terminals; chat still drops a final period the model leaves in. Without it, chat drops the final period and terminals lose the capital letter and trailing punctuation. In code editors and terminals, `src-tauri/src/coding.rs` writes spoken code as code: "main dot rs" → `main.rs`, "camel case get user name" → `getUserName`, "dash dash verbose" → `--verbose` (terminals only). The app is looked up on a thread of its own while recording, so a slow answer (macOS Accessibility can take up to half a second) never holds up the hotkey.

## Command mode

Command mode edits selected text by voice. Set a shortcut in Settings → Shortcuts, select text in any app, hold the shortcut and say what to change ("make this more formal", "translate to German"). WordFlow copies the selection, sends it with the instruction to the AI cleanup model, pastes the result over the selection and puts the clipboard back. If the model hands the text back unchanged, WordFlow asks once more, then says so in the pill instead of pasting the same text. It needs AI cleanup set up, and has its own shortcut rather than guessing from what was said, so a dictation that starts with "make" or "fix" never replaces the text around it. Single modifier keys aren't offered for it: the macOS listener for those holds one key, the dictation hotkey. Each shortcut only ends the recordings it started, so pressing one while the other is held never stops the other's recording (in Toggle mode either one stops a recording with a second press).

Copying the selection takes care with the app it goes to (`src-tauri/src/command.rs`, `src-tauri/src/paste.rs`):

- Terminals get Ctrl+Shift+C on Linux and Ctrl+Insert on Windows, as Ctrl+C would stop the running program; macOS uses ⌘C everywhere. Native Wayland sessions don't say which app has focus, so a terminal can't be told apart there, and command mode says so in the pill rather than risk Ctrl+C.
- With nothing selected, VS Code and JetBrains IDEs copy the whole current line. In code editors, a copy ending in a line break is taken as no selection ("Select some text first"), which also turns away selections of whole lines down to the next one; select up to the end of the last line instead.
- Text and images on the clipboard are put back afterwards. Anything else, like copied files, can't be, so it isn't cleared before copying: copying with nothing selected leaves it in place, while copying a selection replaces it, as Ctrl+C would.

## Commands and formatting

Settings → Commands and formatting turns on rules for English dictation, all on by default: "scratch that", "new line" and "new paragraph" as commands, keeping only the correction in "meet at 3, actually 4", and writing spoken lists, numbers, dates, times, prices and percentages as you'd type them. They live in `src-tauri/src/spoken/` and leave the text alone when unsure. Voice commands apply to dictation pasted when you finish, not to live typing, where "new line" would press Return. Snippets ("my calendar link" → the link) are in Dictionary → Snippets.

## Sharing

To share the dictionary and snippets between computers or with a team, pick a folder in Settings → Sharing that Dropbox, iCloud Drive or a shared drive keeps in step. WordFlow keeps `wordflow-shared.json` there and merges it at launch, every minute and after each change, keeping the newest version of each rule or snippet. The same section exports and imports them as a file.

## Meetings

The Meetings screen records a call and shows its transcript as people talk. It records the microphone and, where the system allows, what the computer plays, as two tracks: the transcript labels them "You" and "Others" (a cheap speaker split, not voice recognition). Each track is written to a WAV file as it's recorded and transcribed in 15–30 second windows cut at pauses, so memory stays flat in long meetings. When a meeting ends, the tracks are mixed into one recording beside the transcript, in `meetings/<id>/` next to History. If AI cleanup is set up (built-in model, local server or cloud API), it writes a summary, action items and open questions, in parts for long meetings. Notes and transcript copy or save as Markdown.

Recording what the computer plays depends on the system:

- **Windows:** WASAPI loopback of any output, through cpal. Nothing extra to install.
- **macOS 14.2 or later:** a Core Audio process tap on an output, through cpal. macOS asks for permission to record system audio the first time. An output that's also a microphone (AirPods, most headsets) can't be tapped this way; choose the Mac's speakers or a virtual device. On macOS 13, route the call through a virtual device such as [BlackHole](https://github.com/ExistentialAudio/BlackHole) and choose it as the source.
- **Linux:** the output's PulseAudio or PipeWire monitor, recorded with `parec` (in `pulseaudio-utils`; PipeWire serves it through `pipewire-pulse`). Any monitor listed by `pactl` can be chosen. Without `parec`, choose an input device that carries the output, or record the microphone only.

Without system audio, the meeting is recorded from the microphone alone and the transcript isn't split by speaker. Through speakers rather than headphones, the microphone also hears the other side; microphone audio quieter than the system audio at the same moment is dropped so it isn't transcribed twice, which can also drop your words when you talk over someone.

