/**
 * Lets the UI run in a plain browser (`npm run dev`, then open localhost:1420)
 * with sample data, for design work without the native app. Never used inside Tauri.
 */
import { emit } from "@tauri-apps/api/event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";

export function installBrowserPreview() {
  if ("__TAURI_INTERNALS__" in window) return;

  const now = Date.now();
  const day = 86_400_000;
  const params = new URLSearchParams(location.search);
  // Add ?os=windows or ?os=linux to preview another system.
  const os = params.get("os") ?? "macos";

  let settings = {
    theme: (params.get("theme") ?? "light") as "light",
    hotkey: os === "macos" ? "Fn" : "Ctrl+Shift+Space",
    recordingMode: "hold",
    liveTyping: false,
    transcribeWhileSpeaking: false,
    finishAndSend: false,
    selectedModel: "small-en",
    language: "auto",
    recentLanguages: ["en", "hi"],
    inputDevice: "",
    boostQuietSpeech: true,
    pauseMedia: false,
    launchAtLogin: false,
    autoEdit: false,
    smartTrailingPunctuation: true,
    plainPunctuation: true,
    cleanup: {
      enabled: params.get("cleanup") !== null,
      backend: (["server", "api"].includes(params.get("cleanup") ?? "") ? params.get("cleanup") : "builtin") as
        | "builtin"
        | "server"
        | "api",
      builtinModel: "qwen2.5-1.5b",
      serverUrl: "http://localhost:11434/v1",
      serverModel: "llama3.2",
      serverKeepAliveMinutes: 60,
      apiUrl: "https://api.openai.com/v1",
      apiKey: "",
      apiModel: "",
    },
    tone: {
      enabled: true,
      formatCode: true,
      apps: [{ app: "Obsidian", category: "code" }] as { app: string; category: string }[],
    },
    commandHotkey: "",
    restoreClipboard: true,
    saveRecordings: true,
    learnCorrections: true,
    watchChromiumApps: false,
    alwaysShowPill: false,
    pillPosition: "bottomCenter",
    showTrayIcon: true,
    autoUpdate: true,
    dictionary: [
      { id: "1", trigger: "my email", replacement: "karan@example.com", isEnabled: true, matchWholeWord: true },
      { id: "2", trigger: "word flow", replacement: "WordFlow", isEnabled: true, matchWholeWord: true },
      { id: "3", trigger: "um", replacement: "", isEnabled: false, matchWholeWord: false },
      { id: "4", trigger: "cooper netties", replacement: "Kubernetes", isEnabled: true, matchWholeWord: true, learned: true },
    ],
    voiceCommands: true,
    selfCorrections: true,
    smartFormatting: true,
    snippets: [
      { id: "s1", trigger: "my calendar link", expansion: "https://cal.com/karan", isEnabled: true },
      { id: "s2", trigger: "my address", expansion: "1 Market Street\nSan Francisco, CA 94105", isEnabled: true },
      { id: "s3", trigger: "sign off", expansion: "Thanks,\nKaran", isEnabled: false },
    ],
    // Add ?sync=1 to preview sharing through a folder.
    syncFolder: params.get("sync") ? "/Users/karan/Dropbox/WordFlow" : "",
    hasCompletedOnboarding: params.get("onboarding") !== "1",
    hasShownModelPrompt: true,
    meetings: { captureSystemAudio: true, systemAudioSource: "", summarizeAutomatically: true },
  };
  const meetings = sampleMeetings(now, day);
  // Add ?meeting=recording to preview a meeting in progress.
  let meetingState: Record<string, unknown> = { phase: "idle" };
  let meetingTimer: ReturnType<typeof setInterval> | undefined;
  const startMeeting = (title: string) => {
    const id = `live-${Date.now()}`;
    const startedAtMs = Date.now() - (params.get("meeting") === "recording" && !meetingTimer ? 95_000 : 0);
    meetings.unshift({
      id,
      title,
      createdAt: startedAtMs,
      durationSecs: 0,
      status: "recording",
      sources: ["microphone", "system"],
      segments: [],
      audioPath: null,
      model: "Whisper Small (English)",
      notes: null,
      notesError: null,
      summarizing: false,
    });
    meetingState = { phase: "recording", id, title, startedAtMs, systemAudio: true, systemError: null };
    let next = 0;
    meetingTimer = setInterval(() => {
      const [source, text] = LIVE_SCRIPT[next % LIVE_SCRIPT.length];
      const segment = { startSecs: next * 6 + 2, endSecs: next * 6 + 7, source, text };
      meetings.find((m) => m.id === id)?.segments.push(segment);
      emit("meeting-segment", { meetingId: id, segment });
      next++;
    }, 2500);
    emit("meeting-state", meetingState);
    emit("meetings-changed");
    return meetingState;
  };
  const stopMeeting = () => {
    if (meetingState.phase !== "recording") return;
    clearInterval(meetingTimer);
    const meeting = meetings.find((m) => m.id === meetingState.id)!;
    meetingState = { phase: "finishing", id: meeting.id };
    emit("meeting-state", meetingState);
    setTimeout(() => {
      Object.assign(meeting, {
        status: "done",
        durationSecs: (Date.now() - meeting.createdAt) / 1000,
        audioPath: "/tmp/audio.wav",
      });
      meetingState = { phase: "idle" };
      emit("meeting-state", meetingState);
      emit("meetings-changed");
    }, 1500);
  };

  const transcripts = [
    "Let's move the design review to Thursday afternoon so the whole team can make it.",
    "Remind me to send the invoice to the client before the end of the week.",
    "The new onboarding flow feels much faster. I think we should ship it behind a flag first and watch the numbers.",
    "karan@example.com",
    "Can you pull the latest numbers for the quarterly report and share them in the channel?",
    "Great talk today. Let's follow up next week on the pricing changes.",
  ];
  const history = transcripts.map((transcript, i) => ({
    id: String(i),
    createdAt: now - i * day * 0.6 - 3_600_000,
    transcript,
    durationSecs: 3 + i * 2.5,
    model: "Whisper Large v3 Turbo (compressed)",
    wordCount: transcript.split(/\s+/).length,
    audioPath: i % 2 === 0 ? `/tmp/recording-${i}.wav` : null,
  }));
  const stats = Array.from({ length: 140 }, (_, i) => ({
    createdAt: now - Math.floor(i * i * 0.004 * day) - i * 1_800_000,
    wordCount: 8 + ((i * 37) % 60),
    durationSecs: 4 + ((i * 13) % 20),
  }));

  const models = [
    ["parakeet-tdt-v3", "Parakeet v3", "parakeet", 640, 0, false, 25, 9.7, 9.2, false],
    ["granite-speech-4.1-2b", "Granite Speech 4.1", "granite", 3115, 0, false, 6, 7.8, 9.8, false],
    ["parakeet-tdt-v2", "Parakeet v2 (English)", "parakeet", 631, 0, true, 1, 9.8, 9.1, false],
    ["large-v3-turbo-q5", "Whisper Large v3 Turbo (compressed)", "whisper", 547, 1119, false, null, 7.5, 9.4, true],
    ["large-v3-turbo", "Whisper Large v3 Turbo", "whisper", 1624, 1119, false, null, 7.0, 9.5, false],
    ["small-en", "Whisper Small (English)", "whisper", 466, 155, true, 1, 8.0, 8.5, true],
    ["base-en", "Whisper Base (English)", "whisper", 142, 36, true, 1, 9.0, 7.5, false],
    ["base", "Whisper Base", "whisper", 142, 36, false, null, 9.0, 7.3, false],
    ["tiny", "Whisper Tiny", "whisper", 75, 14, false, null, 9.5, 6.0, false],
  ].map(([id, name, engine, sizeMb, acceleratorMb, englishOnly, languageCount, speed, accuracy, downloaded]) => ({
    id,
    name,
    engine,
    sizeMb,
    acceleratorMb,
    downloadMb: (sizeMb as number) + ((acceleratorMb as number) <= 200 ? (acceleratorMb as number) : 0),
    accelerator: engine !== "whisper" ? "none" : id === "small-en" ? "installed" : "missing",
    englishOnly,
    languages: languageCount === null ? null : Array.from({ length: languageCount as number }, (_, i) => `l${i}`),
    description: "A sample description for the browser preview.",
    speed,
    accuracy,
    minRamGb: 4,
    // Add ?nomodel=1 to preview a fresh install with nothing downloaded.
    downloaded: downloaded && !params.get("nomodel"),
    downloading: false,
  }));

  const cleanupModels = [
    ["qwen2.5-1.5b", "Qwen 2.5 1.5B", 1117, "Quick on any computer, and writes 29 languages. The one to start with.", true],
    ["llama3.2-3b", "Llama 3.2 3B", 2020, "More careful edits, about twice as slow. Best with a GPU or Apple Silicon.", false],
  ].map(([id, name, sizeMb, description, downloaded]) => ({
    id,
    name,
    engine: "llama",
    sizeMb,
    acceleratorMb: 0,
    downloadMb: sizeMb,
    accelerator: "none",
    englishOnly: false,
    languages: null,
    description,
    speed: 9,
    accuracy: 7.5,
    minRamGb: 4,
    downloaded,
    downloading: false,
  }));

  mockWindows("main");
  mockIPC(
    async (cmd, args) => {
      const a = args as Record<string, unknown>;
      switch (cmd) {
        case "get_status":
          return {
            os,
            version: "2.0.0",
            build: "v2.0.0-alpha.8",
            setupNotes: [],
            // Add ?hotkeyError=1 to preview a hotkey the system wouldn't register.
            hotkeyError: params.get("hotkeyError") ? "Ctrl+Shift+Space is already used by another app" : null,
            // Add ?hotkeyBlocked=1 to preview Fn without Accessibility permission.
            hotkeyBlocked: Boolean(params.get("hotkeyBlocked")),
            modifierHotkeys: os === "macos" ? ["Fn", "RightCommand", "LeftCommand", "RightOption", "LeftOption", "RightControl", "LeftControl"] : [],
          };
        case "get_settings":
          return settings;
        case "save_settings":
          settings = a.settings as typeof settings;
          return settings;
        case "list_models":
          return models;
        case "get_cleanup_status":
          return { builtinAvailable: true, models: cleanupModels, setUp: settings.cleanup.enabled };
        case "get_app_tone":
          return {
            builtIn: [
              ["Slack", "chat"],
              ["Discord", "chat"],
              ["Messages", "chat"],
              ["Mail", "email"],
              ["Outlook", "email"],
              ["Terminal", "terminal"],
              ["iTerm2", "terminal"],
              ["VS Code", "code"],
              ["Cursor", "code"],
              ["Xcode", "code"],
            ].map(([name, category]) => ({ name, category })),
            lastApp: { id: "com.tinyspeck.slackmacgap", name: "Slack", title: "general - Acme", category: "chat" },
          };
        case "test_cleanup_api":
          if (!(a.cleanup as { apiKey: string }).apiKey) throw "Cleanup API: Incorrect API key provided";
          return "So, this is a test of the cleanup model.";
        case "list_server_models":
          return { models: ["gemma3:4b", "llama3.2", "qwen2.5:1.5b"], isLocal: String(a.url).includes("localhost") };
        case "get_engine_status":
          // Add ?warming=1 to preview the model loading.
          return params.get("warming") ? { loaded: null, loading: "small-en" } : { loaded: "small-en", loading: null };
        case "read_history_audio":
          return sampleRecording(history.find((h) => h.id === a.id)?.durationSecs ?? 3);
        case "start_mic_test":
          micTestStartedAt = Date.now();
          clearInterval(micTestLevels);
          micTestLevels = setInterval(() => emit("mic-test-level", 0.2 + Math.random() * 0.6), 50);
          return;
        case "stop_mic_test":
          clearInterval(micTestLevels);
          return sampleRecording(Math.max(1, (Date.now() - micTestStartedAt) / 1000));
        case "cancel_mic_test":
          clearInterval(micTestLevels);
          return;
        case "transcribe_mic_test":
          await new Promise((resolve) => setTimeout(resolve, 900));
          return "Testing one two three, can you hear me clearly?";
        case "get_device_info":
          return {
            device: { chip: "Apple M3 Pro", ramGb: 18, cores: 11, gpu: true, summary: "Apple M3 Pro · 18 GB · Metal", performanceTier: 0.95 },
            recommendation: {
              modelId: "parakeet-tdt-v3",
              reason: "Fast and accurate enough for live dictation, and loads quickly on your Apple M3 Pro.",
            },
          };
        case "get_history":
          return params.get("empty") ? [] : history;
        case "get_stats":
          return params.get("empty") ? [] : stats;
        case "edit_history_item": {
          const item = history.find((h) => h.id === a.id);
          if (!item) return [];
          const transcript = String(a.transcript);
          item.transcript = transcript;
          item.wordCount = transcript.split(/\s+/).length;
          // Pretend any new capitalized word was a misheard name.
          const learned = transcript
            .split(/\s+/)
            .map((w) => w.replace(/[^\p{L}\p{N}]/gu, ""))
            .filter((w) => /^\p{Lu}/u.test(w) && !transcripts.some((t) => t.includes(w)))
            .slice(0, 1)
            .map((word) => {
              const entry = { id: `learned-${word}`, trigger: word.toLowerCase(), replacement: word, isEnabled: true, matchWholeWord: true, learned: true };
              settings.dictionary.push(entry);
              return entry;
            });
          return learned;
        }
        case "forget_words":
          settings.dictionary = settings.dictionary.filter((e) => !(a.ids as string[]).includes(e.id));
          return null;
        case "get_sync_status":
        case "sync_now":
          return { lastSyncedAt: now - 120_000, error: null };
        case "pick_sync_folder":
          return "/Users/karan/Dropbox/WordFlow";
        case "export_library":
          return "/Users/karan/Downloads/wordflow-library.json";
        case "import_library":
          return { rules: 2, snippets: 1 };
        case "list_input_devices":
          return [
            { id: "coreaudio:1", name: "MacBook Pro Microphone", isDefault: true },
            { id: "coreaudio:2", name: "AirPods Pro", isDefault: false },
          ];
        case "get_dictation_state":
          return { phase: "idle" };
        case "get_permissions":
          return [
            { kind: "microphone", granted: true },
            { kind: "accessibility", granted: false },
          ];
        case "get_meeting_setup":
          return {
            systemAudioNote:
              os === "macos"
                ? "Recording system audio needs macOS 14.2 or later, and asks for permission the first time."
                : null,
            systemAudioSources: [
              { id: "", name: "Default output", kind: "output" },
              { id: "output:coreaudio:3", name: "MacBook Pro Speakers", kind: "output" },
              { id: "input:coreaudio:4", name: "BlackHole 2ch", kind: "input" },
            ],
            notesAvailable: settings.cleanup.enabled || params.get("notes") !== "0",
          };
        case "get_meeting_state":
          return meetingState;
        case "start_meeting":
          return startMeeting(String(a.title));
        case "stop_meeting":
          return stopMeeting();
        case "list_meetings": {
          const query = String(a.query ?? "").toLowerCase();
          if (params.get("empty")) return [];
          return meetings
            .filter((m) => !query || m.title.toLowerCase().includes(query) || m.segments.some((s) => s.text.toLowerCase().includes(query)))
            .map((m) => {
              const transcript = m.segments.map((s) => s.text).join(" ");
              return {
                id: m.id,
                title: m.title,
                createdAt: m.createdAt,
                durationSecs: m.durationSecs,
                status: m.status,
                wordCount: transcript.split(/\s+/).filter(Boolean).length,
                preview: transcript.slice(0, 220),
                hasNotes: m.notes !== null,
              };
            });
        }
        case "get_meeting": {
          const meeting = meetings.find((m) => m.id === a.id);
          if (!meeting) throw "This meeting was deleted";
          return structuredClone(meeting);
        }
        case "rename_meeting": {
          const meeting = meetings.find((m) => m.id === a.id);
          if (meeting) meeting.title = String(a.title);
          emit("meetings-changed");
          return null;
        }
        case "delete_meeting":
          meetings.splice(meetings.findIndex((m) => m.id === a.id), 1);
          emit("meetings-changed");
          return null;
        case "summarize_meeting": {
          const meeting = meetings.find((m) => m.id === a.id)!;
          await new Promise((resolve) => setTimeout(resolve, 2500));
          meeting.notes = SAMPLE_NOTES;
          return structuredClone(meeting);
        }
        case "export_meeting_markdown":
          return "/Users/karan/Downloads/meeting.md";
        case "check_for_update":
          return {
            available: false,
            currentVersion: "2.0.0",
            latestVersion: "2.0.0",
            notes: "",
            url: "",
            installable: false,
          };
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  // Only once mockIPC is installed: starting a meeting emits events.
  if (params.get("meeting") === "recording") startMeeting("Design review");
}

type Source = "microphone" | "system";

const LIVE_SCRIPT: [Source, string][] = [
  ["microphone", "Okay, I think everyone's here. Let's look at the new onboarding screens."],
  ["system", "Sure. I shared the link in the chat a minute ago."],
  ["system", "The main change is that the model download starts on the second screen now."],
  ["microphone", "Nice. Does it still work if someone skips the permissions step?"],
  ["system", "It does, we ask again the first time they dictate."],
  ["microphone", "Great. Let's get it in front of a few people this week."],
];

const SAMPLE_NOTES = {
  summary:
    "The team reviewed the pricing page and agreed to launch the annual plan next Tuesday. The comparison table needs a simpler layout before then.",
  actionItems: [
    "Maya: simplify the comparison table by Friday",
    "Karan: draft the launch email",
    "Leo: check the annual discount with finance",
  ],
  openQuestions: ["Do existing customers get the annual price automatically?"],
};

/** Saved meetings for the preview: one with notes, one without, both with a transcript. */
function sampleMeetings(now: number, day: number) {
  const pricing: [Source, string][] = [
    ["microphone", "Thanks for joining. Today is just the pricing page and the annual plan."],
    ["system", "I've got the latest mockups up. The comparison table is still pretty dense."],
    ["system", "I'd like to cut it down to the five features people actually ask about."],
    ["microphone", "Agreed. Can you have a simpler version by Friday?"],
    ["system", "Yes, Friday works."],
    ["microphone", "Then let's plan to launch the annual plan next Tuesday. Karan, can you draft the email?"],
    ["system", "Will do. One question, do existing customers get the annual price automatically?"],
    ["microphone", "Good question. Let's check with finance. Leo, can you ask about the discount too?"],
  ];
  const standup: [Source, string][] = [
    ["microphone", "Quick one today. I'm finishing the meeting notes feature and should have a build tomorrow."],
    ["microphone", "Blocked on nothing right now. Let's skip the demo and do it on Thursday instead."],
  ];
  const meeting = (id: string, title: string, createdAt: number, script: [Source, string][], notes: typeof SAMPLE_NOTES | null, both = true) => ({
    id,
    title,
    createdAt,
    durationSecs: script.length * 37,
    status: "done",
    sources: (both ? ["microphone", "system"] : ["microphone"]) as Source[],
    segments: script.map(([source, text], i) => ({ startSecs: i * 37 + 3, endSecs: i * 37 + 30, source, text })),
    audioPath: "/tmp/audio.wav" as string | null,
    model: "Whisper Small (English)",
    notes: notes as typeof SAMPLE_NOTES | null,
    notesError: null as string | null,
    summarizing: false,
  });
  return [
    meeting("m1", "Pricing review", now - 2 * 3_600_000, pricing, SAMPLE_NOTES),
    meeting("m2", "Standup", now - day - 5 * 3_600_000, standup, null, false),
  ];
}

let micTestStartedAt = 0;
let micTestLevels: ReturnType<typeof setInterval> | undefined;

/** A WAV of a soft tone that swells and pauses like speech, so playback can be previewed. */
function sampleRecording(seconds: number): ArrayBuffer {
  const rate = 8000;
  const samples = Math.round(seconds * rate);
  const buffer = new ArrayBuffer(44 + samples * 2);
  const view = new DataView(buffer);
  const text = (offset: number, value: string) =>
    [...value].forEach((c, i) => view.setUint8(offset + i, c.charCodeAt(0)));
  text(0, "RIFF");
  view.setUint32(4, 36 + samples * 2, true);
  text(8, "WAVEfmt ");
  view.setUint32(16, 16, true); // format chunk size
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, 1, true); // mono
  view.setUint32(24, rate, true);
  view.setUint32(28, rate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  text(36, "data");
  view.setUint32(40, samples * 2, true);
  for (let i = 0; i < samples; i++) {
    const t = i / rate;
    const envelope = Math.max(0, Math.sin(t * 2.3)) * (0.5 + 0.5 * Math.sin(t * 7.1) ** 2);
    view.setInt16(44 + i * 2, Math.round(envelope * 0.3 * Math.sin(2 * Math.PI * 220 * t) * 32767), true);
  }
  return buffer;
}
