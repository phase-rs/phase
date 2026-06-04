import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import type { GameState } from "../adapter/types";
import { useAudioContext } from "../audio/useAudioContext";
import { PreviewBadge } from "../components/chrome/PreviewBadge";
import { LoadGameStateModal } from "../components/menu/LoadGameStateModal";
import { HomeDashboard } from "../components/menu/home/HomeDashboard";
import { isTauri } from "../services/sidecar";
import { buildLegalAiDeckCatalog } from "../services/aiDeckCatalog";
import { saveActiveGame, saveGame, useGameStore } from "../stores/gameStore";
import { useCardDataStore } from "../stores/cardDataStore";
import { usePreferencesStore } from "../stores/preferencesStore";

/**
 * Home route. The persistent app shell owns the scene + chrome; this page runs
 * the menu-time warmups (card DB + AI-catalog prewarm, menu audio) and renders
 * the Cards dashboard plus a thin footer (load-state, alpha note, sign-off).
 */
export function MenuPage() {
  const { t } = useTranslation("menu");
  const navigate = useNavigate();
  const [loadModalOpen, setLoadModalOpen] = useState(false);
  const cardStatus = useCardDataStore((s) => s.status);
  const lastFormat = usePreferencesStore((s) => s.lastFormat);
  const lastMatchType = usePreferencesStore((s) => s.lastMatchType);
  const deckPrewarmStarted = useRef(false);
  useAudioContext("menu");

  // Warm the shared engine worker's card DB immediately so compat checks and
  // game start are instant by the time a deck-requiring action is clicked.
  useEffect(() => {
    void useCardDataStore.getState().warm();
  }, []);

  // Once warm, pre-run summary compatibility for the saved-deck + AI catalog on
  // the last-used format so the setup page's checks become cache hits.
  useEffect(() => {
    if (cardStatus !== "ready" || !lastFormat || deckPrewarmStarted.current) return;
    deckPrewarmStarted.current = true;
    void buildLegalAiDeckCatalog({
      selectedFormat: lastFormat,
      selectedMatchType: lastMatchType,
    }).catch(() => {/* prewarm is best-effort */});
  }, [cardStatus, lastFormat, lastMatchType]);

  // Load an externally-provided GameState (pasted JSON / exported .zip): persist
  // under a fresh gameId, record AI active-game meta, navigate into the game.
  const handleLoadState = useCallback(
    async (state: GameState) => {
      const gameId = crypto.randomUUID();
      await saveGame(gameId, state);
      saveActiveGame({ id: gameId, mode: "ai", difficulty: "Medium" });
      useGameStore.setState({ gameId });
      const playerCount = state.players?.length ?? 0;
      const playersParam = playerCount > 2 ? `&players=${playerCount}` : "";
      navigate(`/game/${gameId}?mode=ai&difficulty=Medium${playersParam}`);
    },
    [navigate],
  );

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <PreviewBadge />
      <HomeDashboard />

      <div className="mx-auto flex w-full max-w-[1180px] flex-col items-center gap-4 px-7 pb-12">
        <button
          onClick={() => setLoadModalOpen(true)}
          className="flex items-center gap-2 rounded-full border border-hairline-strong bg-black/20 px-4 py-1.5 text-xs font-medium text-fg-muted backdrop-blur-sm transition-colors hover:border-hairline-hover hover:text-white"
        >
          <svg aria-hidden="true" viewBox="0 0 24 24" className="h-3.5 w-3.5 shrink-0 fill-current">
            <path d="M12 3a1 1 0 0 1 1 1v8.59l2.3-2.3a1 1 0 1 1 1.4 1.42l-4 4a1 1 0 0 1-1.4 0l-4-4a1 1 0 1 1 1.4-1.42l2.3 2.3V4a1 1 0 0 1 1-1ZM5 19a1 1 0 0 1 1-1h12a1 1 0 1 1 0 2H6a1 1 0 0 1-1-1Z" />
          </svg>
          {t("home.load.title")}
        </button>

        <div className="max-w-md rounded-[10px] border border-ember/20 bg-amber-950/20 px-4 py-2.5 text-center text-sm text-ember-text/70">
          <span className="font-semibold text-ember-soft/90">{t("home.alpha.label")}</span>
          {t("home.alpha.message")}
        </div>

        {isTauri() && (
          <button
            onClick={() => {
              import("@tauri-apps/plugin-process").then((m) => m.exit(0));
            }}
            className="rounded-full border border-hairline-strong bg-black/20 px-5 py-1.5 text-xs font-medium text-fg-meta backdrop-blur-sm transition-colors hover:border-red-500/30 hover:text-red-400"
          >
            {t("home.exit")}
          </button>
        )}

        <p className="text-[11px] tracking-wide text-slate-600">matt evans :: 2026</p>
      </div>

      <LoadGameStateModal
        open={loadModalOpen}
        onClose={() => setLoadModalOpen(false)}
        onLoaded={handleLoadState}
      />
    </div>
  );
}
