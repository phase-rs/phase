import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { useAudioContext } from "../audio/useAudioContext";
import { GameBoard } from "../components/board/GameBoard";
import { ReplayControls } from "../components/replay/ReplayControls";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuPanel } from "../components/menu/MenuShell";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { useReplayStore } from "../stores/replayStore";

/**
 * Stand-alone replay viewer. Deliberately does NOT use `GameProvider` /
 * `GamePage`'s full chrome (multiplayer dialogs, animation overlays, action
 * UI) — those are built for live, interactive games. This page renders the
 * same `GameBoard` component a live game does (it reads from the shared
 * `useGameStore` directly, no props needed) in read-only "spectate" mode —
 * see `stores/replayStore.ts` — plus a scrubber/playback transport.
 */
export function ReplayPage() {
  const { t } = useTranslation("replay");
  const navigate = useNavigate();
  useAudioContext("menu");

  const fileInputRef = useRef<HTMLInputElement>(null);
  const [pickerError, setPickerError] = useState<string | null>(null);

  const isLoading = useReplayStore((s) => s.isLoading);
  const storeError = useReplayStore((s) => s.error);
  const loadReplay = useReplayStore((s) => s.loadReplay);
  const unload = useReplayStore((s) => s.unload);
  const isLoaded = useReplayStore((s) => s.adapter !== null);

  useEffect(() => unload, [unload]);

  const handleFileChosen = useCallback(
    async (file: File) => {
      setPickerError(null);
      try {
        const text = await file.text();
        await loadReplay(text);
      } catch (err) {
        setPickerError(err instanceof Error ? err.message : String(err));
      }
    },
    [loadReplay],
  );

  const handleBack = useCallback(() => {
    unload();
    navigate("/");
  }, [unload, navigate]);

  if (!isLoaded) {
    return (
      <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
        <MenuParticles />
        <ScreenChrome onBack={() => navigate("/")} />
        <div className="menu-scene__vignette" />
        <div className="relative z-10 flex min-h-0 flex-1 flex-col px-4 pt-16 pb-6 sm:px-8">
          <MenuPanel className="flex min-h-0 flex-1 flex-col gap-4">
            <div>
              <p className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
                {t("viewer.eyebrow")}
              </p>
              <h1 className="text-2xl font-semibold text-white">{t("viewer.title")}</h1>
            </div>

            {isLoading && <p className="text-sm text-slate-400">{t("viewer.loading")}</p>}
            {(pickerError || storeError) && (
              <p className="text-sm text-red-300">
                {pickerError ?? storeError ?? t("viewer.errorGeneric")}
              </p>
            )}

            <input
              ref={fileInputRef}
              type="file"
              accept=".json,application/json"
              className="hidden"
              onChange={(e) => {
                const file = e.target.files?.[0];
                e.target.value = "";
                if (file) void handleFileChosen(file);
              }}
            />
            <button
              type="button"
              className={menuButtonClass({ tone: "amber", size: "sm" })}
              onClick={() => fileInputRef.current?.click()}
              disabled={isLoading}
            >
              {t("viewer.chooseFile")}
            </button>

            <button
              type="button"
              className={menuButtonClass({ tone: "neutral", size: "sm" })}
              onClick={() => navigate("/")}
            >
              {t("viewer.backToMenu")}
            </button>
          </MenuPanel>
        </div>
      </div>
    );
  }

  return (
    <div className="relative flex h-screen min-h-0 flex-col bg-gray-950">
      <div className="absolute left-3 top-3 z-40">
        <button
          type="button"
          onClick={handleBack}
          className="rounded bg-black/60 px-3 py-1.5 text-xs font-semibold text-white hover:bg-black/80"
        >
          {t("viewer.backToMenu")}
        </button>
      </div>
      <div className="relative z-10 flex min-h-0 flex-1 flex-col">
        <GameBoard />
      </div>
      <ReplayControls />
    </div>
  );
}
