import { useEffect, useState } from "react";
import { UpdateDialog } from "@/components/UpdateDialog";
import { api, type DictionaryEntry, type UpdateInfo } from "@/lib/api";
import { useStore } from "@/lib/store";
import { useLearnedToast } from "@/lib/useLearnedToast";
import { useTauriEvent } from "@/lib/useTauriEvent";
import { NAV } from "./routes";
import { useApplyTheme } from "@/lib/theme";
import { DashboardScreen } from "@/screens/dashboard/DashboardScreen";
import { DictionaryScreen } from "@/screens/dictionary/DictionaryScreen";
import { HistoryScreen } from "@/screens/history/HistoryScreen";
import { MeetingsScreen } from "@/screens/meetings/MeetingsScreen";
import { ModelsScreen } from "@/screens/models/ModelsScreen";
import { OnboardingScreen } from "@/screens/onboarding/OnboardingScreen";
import { SettingsScreen } from "@/screens/settings/SettingsScreen";
import { StatisticsScreen } from "@/screens/statistics/StatisticsScreen";
import type { Route } from "./routes";
import { Rail } from "./Rail";

const DAY_MS = 86_400_000;
const LAST_CHECK_KEY = "lastUpdateCheck";

/** Checks GitHub for a newer release at most once a day. */
function useDailyUpdateCheck(enabled: boolean) {
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  useEffect(() => {
    if (!enabled) return;
    let last = 0;
    try {
      last = Number(localStorage.getItem(LAST_CHECK_KEY)) || 0;
    } catch {}
    if (Date.now() - last < DAY_MS) return;
    api
      .checkForUpdate()
      .then((result) => {
        try {
          localStorage.setItem(LAST_CHECK_KEY, String(Date.now()));
        } catch {}
        if (result.available) setInfo(result);
      })
      .catch(() => {});
  }, [enabled]);
  return { info, dismiss: () => setInfo(null) };
}

export function App() {
  const { settings, models, updateSettings } = useStore();
  // A window reopened from the menu bar may start on a screen given in its URL.
  const [route, setRoute] = useState<Route>(() => {
    const hash = window.location.hash.slice(1);
    return NAV.some((item) => item.route === hash) ? (hash as Route) : "dashboard";
  });
  // The menu bar panel can open a specific screen.
  useTauriEvent<string>("navigate", ({ payload }) => {
    if (NAV.some((item) => item.route === payload)) setRoute(payload as Route);
  });
  // Words learned from a correction made in another app.
  const showLearned = useLearnedToast();
  useTauriEvent<DictionaryEntry[]>("words-learned", ({ payload }) => showLearned(payload));
  useApplyTheme(settings.theme);

  // First launch without a model: start on AI Models, once.
  useEffect(() => {
    if (!settings.hasShownModelPrompt && !models.some((m) => m.downloaded) && models.length > 0) {
      setRoute("models");
      updateSettings({ hasShownModelPrompt: true });
    }
  }, [settings.hasShownModelPrompt, models, updateSettings]);

  const update = useDailyUpdateCheck(settings.autoUpdate && settings.hasCompletedOnboarding);

  if (!settings.hasCompletedOnboarding) {
    return <OnboardingScreen />;
  }

  return (
    <div className="flex h-full bg-shell">
      <UpdateDialog update={update.info} onClose={update.dismiss} />
      <Rail route={route} onNavigate={setRoute} />
      {/* The screens sit on a panel that floats on the window's shell, beside the rail. */}
      <div className="relative my-2 mr-2 min-w-0 flex-1 overflow-hidden rounded-panel border border-line bg-flow shadow-panel">
        <main key={route} className="relative h-full animate-fade-in">
          {/* The window can be dragged by the strip along the top, as with a title bar. */}
          <div data-tauri-drag-region className="absolute inset-x-0 top-0 z-10 h-7" />
          {route === "dashboard" && <DashboardScreen onNavigate={setRoute} />}
          {route === "history" && <HistoryScreen />}
          {route === "meetings" && <MeetingsScreen onNavigate={setRoute} />}
          {route === "dictionary" && <DictionaryScreen />}
          {route === "statistics" && <StatisticsScreen />}
          {route === "models" && <ModelsScreen />}
          {route === "settings" && <SettingsScreen />}
        </main>
      </div>
    </div>
  );
}
