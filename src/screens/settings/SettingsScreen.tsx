import {
  AudioLines,
  BookA,
  ClipboardCheck,
  ClipboardPaste,
  Command,
  CornerDownLeft,
  Ear,
  Globe,
  Hand,
  Keyboard,
  Lightbulb,
  MessageSquareText,
  Mic,
  Monitor,
  Music,
  Power,
  Moon,
  PanelBottom,
  RefreshCw,
  RotateCw,
  Shield,

  Sparkles,
  Sun,
  TextCursorInput,
  Wand2,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { AppToneSection } from "@/components/settings/AppToneSection";
import { CleanupSection } from "@/components/settings/CleanupSection";
import { CommandHotkeyPicker } from "@/components/settings/CommandHotkeyPicker";
import { HotkeyPicker } from "@/components/settings/HotkeyPicker";
import { MicTestSection } from "@/components/settings/MicTestSection";
import { PasteLastHotkeyPicker } from "@/components/settings/PasteLastHotkeyPicker";
import { PillPositionPicker } from "@/components/settings/PillPositionPicker";
import { SharingSection } from "@/components/settings/SharingSection";
import { SpokenSection } from "@/components/settings/SpokenSection";
import { PermissionList } from "@/components/PermissionList";
import { UpdateDialog } from "@/components/UpdateDialog";
import { WhatsNewDialog } from "@/components/WhatsNewDialog";
import {
  Badge,
  Button,
  EmptyState,
  Page,
  PageHeader,
  Section,
  SegmentedControl,
  Select,
  SettingRow,
  Switch,
  useToast,
  type SelectOption,
} from "@/components/ui";
import { api, errorMessage, type InputDevice, type Settings, type UpdateInfo } from "@/lib/api";
import { cn } from "@/lib/cn";
import { hotkeyParts } from "@/lib/format";
import { hotkeyBlockedMessage, useWhileHotkeyBlocked } from "@/lib/hotkeyBlocked";

import { LANGUAGES, languageName } from "@/lib/languages";
import { useStore } from "@/lib/store";
import { WHATS_NEW } from "@/lib/whatsNew";

type Tab = "general" | "dictation" | "ai" | "audio" | "permissions";

/** The tab Settings opens on next, set by links into it from other screens. */
let nextTab: Tab | null = null;

/** Opens Settings on `tab` next time, as when a screen links to AI cleanup. */
export function showSettingsTab(tab: Tab) {
  nextTab = tab;
}

export function SettingsScreen() {
  const { status } = useStore();
  const [tab, setTab] = useState<Tab>(() => {
    const first = nextTab ?? "general";
    nextTab = null;
    return first;
  });
  const tabs: { value: Tab; label: string; icon: LucideIcon }[] = [
    { value: "general", label: "General", icon: Command },
    { value: "dictation", label: "Dictation", icon: MessageSquareText },
    { value: "ai", label: "AI cleanup", icon: Sparkles },
    { value: "audio", label: "Audio", icon: Mic },
  ];
  // Only macOS has per-app permissions to manage.
  if (status.os === "macos") tabs.push({ value: "permissions", label: "Permissions", icon: Shield });

  return (
    <Page>
      <PageHeader
        title="Settings"
        actions={<SegmentedControl value={tab} options={tabs} onChange={setTab} className={tabs.length > 4 ? "w-[600px]" : "w-[460px]"} />}
      />
      {tab === "general" && <GeneralTab />}
      {tab === "dictation" && <DictationTab />}
      {tab === "ai" && <CleanupSection />}
      {tab === "audio" && (
        <>
          <AudioTab />
          <MicTestSection />
          <RecordingSection />
        </>
      )}
      {tab === "permissions" && <PermissionsTab />}
    </Page>
  );
}

/** Saves a settings change and shows a toast if the backend rejects it. */
function useSave() {
  const { updateSettings } = useStore();
  const toast = useToast();
  return useCallback(
    (patch: Partial<Settings>) => updateSettings(patch).catch((e) => toast(errorMessage(e), "error")),
    [updateSettings, toast],
  );
}

function GeneralTab() {
  const { settings, status, refreshStatus } = useStore();
  const save = useSave();
  useWhileHotkeyBlocked(status.hotkeyBlocked, refreshStatus);
  const isMac = status.os === "macos";

  return (
    <>
      <Section title="Appearance">
        <SettingRow icon={Sun} label="Theme" description="Follow your system, or pick a look.">
          <SegmentedControl
            value={settings.theme}
            onChange={(theme) => save({ theme })}
            options={[
              { value: "system", label: "System", icon: Monitor },
              { value: "light", label: "Light", icon: Sun },
              { value: "dark", label: "Dark", icon: Moon },
            ]}
          />
        </SettingRow>
      </Section>

      <Section title="Shortcuts">
        <SettingRow
          icon={Keyboard}
         
          label="Dictation hotkey"
          description={isMac ? "Works in every app. Fn is the easiest to reach." : "Works in every app. Click to record a new one."}
        >
          <HotkeyPicker />
        </SettingRow>
        {status.hotkeyBlocked && (
          <div className="flex items-center justify-between gap-4 py-3 pl-8 type-small text-ink-secondary">
            <p>{hotkeyBlockedMessage(hotkeyParts(settings.hotkey).join(" + "))}</p>
            <Button size="sm" variant="ghost" onClick={() => api.openPermissionSettings("accessibility")}>
              Open Accessibility
            </Button>
          </div>
        )}
        <SettingRow
          icon={Hand}
         
          label="Recording mode"
          description={
            {
              hold: "Hold the hotkey while you talk, and let go when you're done. Double-tap it to keep recording hands-free, then press it once to stop.",
              toggle: "Press the hotkey to start recording, and press it again to stop.",
              hybrid: "Hold the hotkey while you talk, or tap it to keep recording hands-free and press it again to stop.",
            }[settings.recordingMode]
          }
        >
          <SegmentedControl
            value={settings.recordingMode}
            onChange={(recordingMode) => save({ recordingMode })}
            options={[
              { value: "hold", label: "Hold to talk" },
              { value: "toggle", label: "Toggle" },
              { value: "hybrid", label: "Hold or tap" },
            ]}
            className="w-[330px]"
          />
        </SettingRow>
        <SettingRow
          icon={TextCursorInput}
          label="Edit with your voice"
          description="Select text in any app, hold this shortcut and say what to change, like “make this friendlier”. The result replaces your selection. Uses the model set up under AI cleanup."
        >
          <CommandHotkeyPicker />
        </SettingRow>
        <SettingRow
          icon={ClipboardPaste}
          label="Paste last transcript"
          description="Pastes your latest dictation again, for when it landed in the wrong place or nowhere. Also in the menu of the menu bar icon."
        >
          <PasteLastHotkeyPicker />
        </SettingRow>
      </Section>

      <Section title="App">
        <SettingRow
          icon={Power}
          label="Launch at login"
          description={
            settings.launchAtLogin
              ? "WordFlow starts in the background when you log in, ready for your hotkey."
              : "WordFlow only runs when you open it."
          }
        >
          <Switch label="Launch at login" checked={settings.launchAtLogin} onChange={(launchAtLogin) => save({ launchAtLogin })} />
        </SettingRow>
        <SettingRow
          icon={Command}
         
          label={isMac ? "Show menu bar icon" : "Show tray icon"}
          description="Quick access to dictation and WordFlow's window."
        >
          <Switch checked={settings.showTrayIcon} onChange={(showTrayIcon) => save({ showTrayIcon })} />
        </SettingRow>
        <SettingRow
          icon={PanelBottom}
         
          label="Always show the recorder pill"
          description={
            settings.alwaysShowPill
              ? "The small recorder stays on screen even when you're not dictating."
              : "The recorder appears while you dictate and hides when you're done."
          }
        >
          <Switch checked={settings.alwaysShowPill} onChange={(alwaysShowPill) => save({ alwaysShowPill })} />
        </SettingRow>
        <SettingRow icon={PanelBottom} label="Recorder position" description="Where the pill appears on your screen.">
          <PillPositionPicker value={settings.pillPosition} onChange={(pillPosition) => save({ pillPosition })} />
        </SettingRow>
      </Section>

      <SharingSection />
      <UpdatesSection />
    </>
  );
}

function DictationTab() {
  const { settings, status } = useStore();
  const save = useSave();
  const isMac = status.os === "macos";

  return (
    <>
      <Section title="Inserting text">
        <SettingRow
          icon={Keyboard}
         
          label="Type while speaking"
          description="Insert short phrases as you speak. Escape stops further typing; words already inserted stay. Turn off to insert everything when you stop."
        >
          <Switch label="Type while speaking" checked={settings.liveTyping} onChange={(liveTyping) => save({ liveTyping })} />
        </SettingRow>
        {settings.liveTyping && settings.recordingMode !== "toggle" && settings.hotkey !== "Fn" && (
          <p className="py-3 pl-8 type-small text-ink-secondary">
            For live typing with this shortcut, choose Toggle mode. Other modes insert the transcript when you stop.
          </p>
        )}
        <SettingRow
          icon={Zap}
          label="Transcribe while speaking"
          description="Transcribe at pauses as you talk, so longer dictations insert almost as soon as you stop. Punctuation where parts meet can differ slightly. Not used while typing live."
        >
          <Switch
            label="Transcribe while speaking"
            checked={settings.transcribeWhileSpeaking}
            onChange={(transcribeWhileSpeaking) => save({ transcribeWhileSpeaking })}
          />
        </SettingRow>
        <SettingRow
          icon={CornerDownLeft}
          label="Finish and send"
          description={`Press ${isMac ? "Return" : "Enter"} while recording to stop, insert the text and send it, as in a chat or an AI prompt. Works while you hold the hotkey too.`}
        >
          <Switch label="Finish and send" checked={settings.finishAndSend} onChange={(finishAndSend) => save({ finishAndSend })} />
        </SettingRow>
        <SettingRow
          icon={ClipboardCheck}
         
          label="Restore clipboard after pasting"
          description={
            settings.restoreClipboard
              ? "After WordFlow pastes, whatever you had copied is put back."
              : "The transcript stays on your clipboard after WordFlow pastes it."
          }
        >
          <Switch checked={settings.restoreClipboard} onChange={(restoreClipboard) => save({ restoreClipboard })} />
        </SettingRow>
      </Section>

      <LanguageSection />
      <SpokenSection />

      <Section title="Transcript cleanup" description="Light, offline tidying after each dictation.">
        <SettingRow
          icon={Wand2}
         
          label="Remove filler words"
          description="Takes out “um”, “uh” and similar words. The meaning of what you said never changes."
        >
          <Switch checked={settings.autoEdit} onChange={(autoEdit) => save({ autoEdit })} />
        </SettingRow>
        <SettingRow
          icon={Sparkles}
         
          label="Smart trailing punctuation"
          description="When you dictate just an email, link, number or single word, the final period is dropped so it pastes clean. Sentences are left alone."
        >
          <Switch
            checked={settings.smartTrailingPunctuation}
            onChange={(smartTrailingPunctuation) => save({ smartTrailingPunctuation })}
          />
        </SettingRow>
        <SettingRow
          icon={Sparkles}
          label="Commas instead of colons and semicolons"
          description={
            settings.plainPunctuation
              ? "Colons and semicolons the speech model adds become commas, the way people usually type. Times like 3:30, links and the colon before a list stay."
              : "Punctuation stays as the speech model wrote it, colons and semicolons included."
          }
        >
          <Switch checked={settings.plainPunctuation} onChange={(plainPunctuation) => save({ plainPunctuation })} />
        </SettingRow>
        <SettingRow
          icon={BookA}
         
          label="Replacements and snippets"
          description="Word fixes and snippets (say “my calendar link”, get the link) live in Dictionary."
        />
      </Section>

      <Section title="Learning">
        <SettingRow
          icon={Lightbulb}
         
          label="Learn from my corrections"
          description={
            settings.learnCorrections
              ? "When you fix a misheard name or term, or its capitals (github → GitHub), WordFlow adds it to Dictionary and gets it right next time. To fix a dictation, hover it under Recently said in the menu bar and click the pencil, or edit it in History. Learned words can be removed in Dictionary."
              : "Corrections are saved but nothing is learned from them."
          }
        >
          <Switch
            checked={settings.learnCorrections}
            onChange={(learnCorrections) => save({ learnCorrections })}
          />
        </SettingRow>
        {isMac && settings.learnCorrections && (
          <SettingRow
            icon={Globe}
            label="Also learn in Chrome and Electron apps"
            description={
              settings.watchChromiumApps
                ? "Fixes made right after dictating in Chrome, Slack, VS Code and similar apps are learned too. Typing in them may lag for about 10 seconds after each dictation."
                : "Fixes made in Chrome, Slack, VS Code and similar apps aren't picked up, so typing in them stays smooth. Fix those dictations from the menu bar or History instead."
            }
          >
            <Switch
              checked={settings.watchChromiumApps}
              onChange={(watchChromiumApps) => save({ watchChromiumApps })}
            />
          </SettingRow>
        )}
      </Section>

      <AppToneSection />
    </>
  );
}

function LanguageSection() {
  const { settings } = useStore();
  const save = useSave();

  const recents = settings.recentLanguages.filter((code) => code !== settings.language);
  const options: SelectOption<string>[] = [
    { value: "auto", label: "Detect automatically" },
    ...recents.map((code) => ({ value: code, label: languageName(code), detail: "Recent" })),
    ...LANGUAGES.filter(([code]) => !recents.includes(code)).map(([code, name]) => ({ value: code, label: name })),
  ];

  const choose = (language: string) => {
    const recentLanguages =
      language === "auto"
        ? settings.recentLanguages
        : [language, ...settings.recentLanguages.filter((c) => c !== language)].slice(0, 5);
    save({ language, recentLanguages });
  };

  return (
    <Section title="Spoken language">
      <SettingRow
        icon={Globe}
       
        label="Language you speak"
        description="A hint for transcription. It doesn't translate, and a wrong hint can give poor results, so detection is the safest choice."
      >
        <Select
          value={settings.language}
          options={options}
          onChange={choose}
          searchable
          aria-label="Spoken language"
          className="w-[240px]"
        />
      </SettingRow>
      <div className="py-3.5 pl-8 type-small text-ink-muted">
        For languages other than English, use a multilingual model. English-only models can only write English. With
        detection on, you can switch languages between sentences: Whisper listens again after each pause.
      </div>
    </Section>
  );
}

function UpdatesSection() {
  const { settings, status } = useStore();
  const save = useSave();
  const toast = useToast();
  const [checking, setChecking] = useState(false);
  const [update, setUpdate] = useState<UpdateInfo | null>(null);
  const [showWhatsNew, setShowWhatsNew] = useState(false);

  const check = async () => {
    setChecking(true);
    try {
      const info = await api.checkForUpdate();
      if (info.available) setUpdate(info);
      else toast(`You're up to date. WordFlow ${info.currentVersion} is the latest version.`);
    } catch (error) {
      toast(errorMessage(error), "error");
    } finally {
      setChecking(false);
    }
  };

  return (
    <Section
      title="Updates"
      action={
        <Button size="sm" variant="ghost" icon={RotateCw} loading={checking} onClick={check}>
          {checking ? "Checking…" : "Check now"}
        </Button>
      }
    >
      <SettingRow
        icon={RefreshCw}
        label="Check for updates automatically"
        description={`Once a day, in the background. You have WordFlow ${status.build || status.version}.`}
      >
        <Switch checked={settings.autoUpdate} onChange={(autoUpdate) => save({ autoUpdate })} />
      </SettingRow>
      <SettingRow icon={Sparkles} label="What's new" description="The latest changes to WordFlow.">
        <Button size="sm" onClick={() => setShowWhatsNew(true)}>
          Show
        </Button>
      </SettingRow>
      <UpdateDialog update={update} onClose={() => setUpdate(null)} />
      <WhatsNewDialog entries={showWhatsNew ? WHATS_NEW : null} onClose={() => setShowWhatsNew(false)} />
    </Section>
  );
}

function AudioTab() {
  const { settings } = useStore();
  const save = useSave();
  const [devices, setDevices] = useState<InputDevice[]>();
  const [refreshing, setRefreshing] = useState(false);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    setDevices(await api.listInputDevices());
    setRefreshing(false);
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const selectedExists = devices?.some((d) => d.id === settings.inputDevice);
  const rows = [
    { id: "", name: "System default", detail: devices?.find((d) => d.isDefault)?.name },
    ...(devices ?? []).map((d) => ({ id: d.id, name: d.name, detail: undefined })),
  ];


  return (
    <Section
      title="Microphone"
      description="WordFlow records from this input."
      action={
        <Button size="sm" variant="ghost" icon={RotateCw} loading={refreshing} onClick={refresh}>
          Refresh
        </Button>
      }
    >
      {devices && devices.length === 0 ? (
        <EmptyState icon={Mic} title="No microphones found" description="Connect a microphone, then refresh." />
      ) : (
        rows.map((row) => {
          const selected = row.id === settings.inputDevice || (row.id === "" && !selectedExists);
          return (
            <button
              key={row.id || "default"}
              type="button"
              role="radio"
              aria-checked={selected}
              onClick={() => save({ inputDevice: row.id })}
              className="group flex w-full items-center gap-3.5 py-4 text-left"
            >
              <span
                className={cn(
                  "flex size-[18px] shrink-0 items-center justify-center rounded-full border-[1.5px] transition-colors",
                  selected ? "border-accent" : "border-line-strong group-hover:border-ink-muted",
                )}
              >
                {selected && <span className="size-2 rounded-full bg-accent" />}
              </span>
              <span className="min-w-0 flex-1">
                <span className="block truncate type-label">{row.name}</span>
                {row.detail && <span className="block truncate type-caption text-ink-muted">{row.detail}</span>}
              </span>
              {selected && (
                <Badge tone="success" icon={Mic}>
                  In use
                </Badge>
              )}
            </button>
          );
        })
      )}
    </Section>
  );
}

function RecordingSection() {
  const { settings } = useStore();
  const save = useSave();
  return (
    <Section title="While you dictate">
      <SettingRow
        icon={Ear}
        label="Whisper mode"
        description={
          settings.boostQuietSpeech
            ? "Speaking softly? Quiet speech is turned up before it's transcribed. Silence and background noise are left alone."
            : "Audio is transcribed at the level it was recorded."
        }
      >
        <Switch label="Whisper mode" checked={settings.boostQuietSpeech} onChange={(boostQuietSpeech) => save({ boostQuietSpeech })} />
      </SettingRow>
      <SettingRow
        icon={Music}
        label="Pause media while dictating"
        description={
          settings.pauseMedia
            ? "Music and videos pause when you start recording, and carry on when you stop."
            : "Music and videos keep playing while you dictate."
        }
      >
        <Switch label="Pause media while dictating" checked={settings.pauseMedia} onChange={(pauseMedia) => save({ pauseMedia })} />
      </SettingRow>
        <SettingRow
          icon={AudioLines}
         
          label="Keep recordings"
          description={
            settings.saveRecordings
              ? "Each dictation's audio is kept on this computer, so you can replay it from History."
              : "Only the text is kept. Recordings you already have stay until you delete them in History."
          }
        >
          <Switch checked={settings.saveRecordings} onChange={(saveRecordings) => save({ saveRecordings })} />
        </SettingRow>
    </Section>
  );
}

function PermissionsTab() {
  return (
    <Section title="App permissions" description="WordFlow needs both to dictate into other apps.">
      <PermissionList />
    </Section>
  );
}
