import { X } from "lucide-react";
import { Button, Hotkey, IconButton, useToast } from "@/components/ui";
import { errorMessage } from "@/lib/api";
import { useStore } from "@/lib/store";
import { HotkeyRecorder } from "./HotkeyRecorder";

/** The suggested shortcut. Ctrl+Shift+V pastes plain text in browsers and terminals outside a Mac. */
function recommendedPasteLastHotkey(os: string) {
  return os === "macos" ? "Ctrl+Shift+V" : "Ctrl+Alt+V";
}

/** Records the shortcut that pastes the latest transcript, or turns it off. */
export function PasteLastHotkeyPicker() {
  const { settings, status, updateSettings } = useStore();
  const toast = useToast();
  const save = (pasteLastHotkey: string) =>
    updateSettings({ pasteLastHotkey }).catch((e) => toast(errorMessage(e), "error"));
  const recommended = recommendedPasteLastHotkey(status.os);
  const free = recommended !== settings.hotkey && recommended !== settings.commandHotkey;

  return (
    <div className="flex items-center gap-1.5">
      {!settings.pasteLastHotkey && free && (
        <Button size="sm" variant="ghost" onClick={() => save(recommended)}>
          Use <Hotkey value={recommended} /> (Recommended)
        </Button>
      )}
      <HotkeyRecorder value={settings.pasteLastHotkey} onChange={save} placeholder="Click to set a shortcut" />
      {settings.pasteLastHotkey && <IconButton icon={X} label="Turn off" size="sm" onClick={() => save("")} />}
    </div>
  );
}
