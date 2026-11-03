import { useEffect, useState } from "react";

import { ensureCardLocale, type LocalizedFace } from "../services/engineRuntime.ts";
import { usePreferencesStore } from "../stores/preferencesStore.ts";

/** Official printed content for display only. Never identifies or translates
 * an individual runtime ability: the sidecar contains the whole card text. */
export function useLocalizedCardContent(name: string | null): LocalizedFace | null {
  const language = usePreferencesStore((s) => s.language);
  const [result, setResult] = useState<{
    name: string;
    language: string;
    face: LocalizedFace | null;
  } | null>(null);

  useEffect(() => {
    if (!name || language === "en") return;
    let cancelled = false;
    void ensureCardLocale(language).then((map) => {
      if (!cancelled) {
        setResult({ name, language, face: map.get(name.toLowerCase()) ?? null });
      }
    });
    return () => { cancelled = true; };
  }, [name, language]);

  // A new card/language must never briefly display the previous card's text.
  return language !== "en" && result?.name === name && result.language === language
    ? result.face
    : null;
}
