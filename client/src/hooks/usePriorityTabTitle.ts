import { useEffect } from "react";
import i18n from "i18next";

import { usePreferencesStore } from "../stores/preferencesStore";
import { usePriorityAttention } from "./usePriorityAttention";

/** Owns a hidden-tab cue only until another feature changes the title. */
export function usePriorityTabTitle(): void {
  const { cue } = usePriorityAttention();
  // The cue's intensity changes (announced to reminded) while the same window
  // stays pending; the title's ownership must not restart with it.
  const active = cue !== "none";
  const enabled = usePreferencesStore((s) => s.priorityAlertTitle);

  useEffect(() => {
    if (!active || !enabled) return;

    let applied: { original: string; title: string } | null = null;

    const restore = () => {
      if (applied && document.title === applied.title) {
        document.title = applied.original;
      }
      applied = null;
    };

    const syncVisibility = () => {
      if (!document.hidden) {
        restore();
        return;
      }
      // Repeated announcements and visibility events must never nest prefixes
      // or overwrite a title that another feature took over in the meantime.
      if (applied) return;
      const original = document.title;
      const title = `● ${i18n.t("game:status.yourPriority")} — ${original}`;
      applied = { original, title };
      document.title = title;
    };

    syncVisibility();
    document.addEventListener("visibilitychange", syncVisibility);
    window.addEventListener("focus", restore);
    return () => {
      document.removeEventListener("visibilitychange", syncVisibility);
      window.removeEventListener("focus", restore);
      restore();
    };
  }, [active, enabled]);
}
