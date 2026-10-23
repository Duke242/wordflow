import type { Route } from "@/app/routes";

export type WhatsNewEntry = {
  /** Stable, so a seen entry stays seen. */
  id: string;
  title: string;
  description: string;
  /** A screen to try it on, if any. */
  route?: Route;
};

/** Newest first. Add new entries at the top; the dialog shows those added since it was last closed. */
export const WHATS_NEW: WhatsNewEntry[] = [
  {
    id: "paste-last",
    title: "Paste your last transcript again",
    description:
      "Text landed in the wrong place, or nowhere? Choose Paste Last Transcript from the menu bar icon, or set a shortcut for it in Settings.",
    route: "settings",
  },
  {
    id: "fn-return-sends",
    title: "Fn + Return sends",
    description: "With Finish and send on, pressing Return while holding Fn now stops, types and sends your message.",
  },
  {
    id: "close-to-menu-bar",
    title: "Closing keeps WordFlow in the menu bar",
    description: "Closing the window frees its memory. Dictation keeps working, and the menu bar icon opens it again.",
  },
  {
    id: "history-playback",
    title: "Play recordings back in History",
    description: "Listen to what you said next to each transcript.",
    route: "history",
  },
  {
    id: "new-models",
    title: "New models",
    description:
      "Granite Speech 4.1 for transcription, and Gemma 4 E2B and Qwen 3.5 4B for AI cleanup. Switching models no longer holds up transcription.",
    route: "models",
  },
  {
    id: "google-search",
    title: "Dictate into Google Search",
    description: "Dictations are typed into the Google search box rather than pasted, so they no longer go missing.",
  },
];

const SEEN_KEY = "whatsNewSeen";

/** Entries added since the dialog was last closed: all of them the first time. */
export function unseenEntries(seen: string | null, entries = WHATS_NEW): WhatsNewEntry[] {
  if (!seen) return entries;
  const index = entries.findIndex((entry) => entry.id === seen);
  return index === -1 ? entries : entries.slice(0, index);
}

export function readSeen(): string | null {
  try {
    return localStorage.getItem(SEEN_KEY);
  } catch {
    return null;
  }
}

/** Marks everything up to now as seen. */
export function markAllSeen() {
  try {
    localStorage.setItem(SEEN_KEY, WHATS_NEW[0].id);
  } catch {}
}
