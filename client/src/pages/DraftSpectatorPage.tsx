import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useSearchParams } from "react-router";

import { useAudioContext } from "../audio/useAudioContext";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { DraftSpectatorDashboard } from "../components/draft/DraftSpectatorDashboard";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuPanel } from "../components/menu/MenuShell";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { useDraftSpectatorStore } from "../stores/draftSpectatorStore";

export function DraftSpectatorPage() {
  const { t } = useTranslation("draft");
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const code = (searchParams.get("code") ?? "").trim().toUpperCase();

  useAudioContext("menu");

  const status = useDraftSpectatorStore((s) => s.status);
  const view = useDraftSpectatorStore((s) => s.view);
  const error = useDraftSpectatorStore((s) => s.error);
  const watchDraft = useDraftSpectatorStore((s) => s.watchDraft);
  const leave = useDraftSpectatorStore((s) => s.leave);

  useEffect(() => {
    if (!code) return;
    void watchDraft(code);
    return () => leave();
  }, [code, watchDraft, leave]);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <MenuParticles />
      <ScreenChrome onBack={() => { leave(); navigate("/multiplayer"); }} />
      <div className="menu-scene__vignette" />
      <div className="relative z-10 flex min-h-0 flex-1 flex-col px-4 pt-16 pb-6 sm:px-8">
        <MenuPanel className="flex min-h-0 flex-1 flex-col gap-4">
          <div>
            <p className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
              {t("spectator.eyebrow")}
            </p>
            <h1 className="text-2xl font-semibold text-white">
              {code ? t("spectator.title", { code }) : t("spectator.missingCode")}
            </h1>
          </div>

          {status === "connecting" && (
            <p className="text-sm text-slate-400">{t("spectator.connecting")}</p>
          )}
          {status === "error" && (
            <p className="text-sm text-red-300">{error ?? t("spectator.errorGeneric")}</p>
          )}
          {view && <DraftSpectatorDashboard view={view} />}

          <button
            type="button"
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
            onClick={() => { leave(); navigate("/multiplayer"); }}
          >
            {t("spectator.backToLobby")}
          </button>
        </MenuPanel>
      </div>
    </div>
  );
}
