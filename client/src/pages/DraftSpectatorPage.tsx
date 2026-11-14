import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useSearchParams } from "react-router";

import { useAudioContext } from "../audio/useAudioContext";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { DraftSpectatorDashboard } from "../components/draft/DraftSpectatorDashboard";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuPanel } from "../components/menu/MenuShell";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { normalizeSpectatorDraftCode, useDraftSpectatorStore } from "../stores/draftSpectatorStore";
import { useEffectiveOffline } from "../stores/connectivityStore";

export function DraftSpectatorPage() {
  const { t } = useTranslation("draft");
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const code = normalizeSpectatorDraftCode(searchParams.get("code") ?? "");
  // The authority that listed this draft, carried by the route from the
  // lobby. Absent → the store falls back to the hosting server.
  const serverUrl = searchParams.get("server") ?? undefined;

  useAudioContext("menu");

  const effectiveOffline = useEffectiveOffline();
  const draftCode = useDraftSpectatorStore((s) => s.draftCode);
  const status = useDraftSpectatorStore((s) => s.status);
  const view = useDraftSpectatorStore((s) => s.view);
  const error = useDraftSpectatorStore((s) => s.error);
  const session = useDraftSpectatorStore((s) => s.session);
  const watchDraft = useDraftSpectatorStore((s) => s.watchDraft);
  const leave = useDraftSpectatorStore((s) => s.leave);
  const closed = useRef(false);
  const watchingCode = useRef<string | null>(null);
  const exactRouteIdentity = code !== "" && draftCode === code;
  const exactActiveSession = session !== null && draftCode === code;
  const offlineUnavailable = effectiveOffline && !exactActiveSession;

  const leaveAndNavigate = () => {
    if (!closed.current) {
      closed.current = true;
      leave();
    }
    navigate("/multiplayer");
  };

  useEffect(() => {
    if (!code) {
      if (!effectiveOffline && draftCode !== null) leave();
      return;
    }
    if (exactActiveSession || effectiveOffline) return;
    if (watchingCode.current === code) return;

    watchingCode.current = code;
    void watchDraft(code, serverUrl).finally(() => {
      if (watchingCode.current === code) watchingCode.current = null;
    });
  }, [code, draftCode, effectiveOffline, exactActiveSession, leave, serverUrl, watchDraft]);

  useEffect(() => () => {
    if (!closed.current) {
      closed.current = true;
      leave();
    }
  }, [leave]);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <MenuParticles />
      <ScreenChrome onBack={leaveAndNavigate} />
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

          {exactRouteIdentity && status === "connecting" && (
            <p className="text-sm text-slate-400">{t("spectator.connecting")}</p>
          )}
          {exactRouteIdentity && status === "error" && (
            <p className="text-sm text-red-300">{error ?? t("spectator.errorGeneric")}</p>
          )}
          {offlineUnavailable && (
            <p className="text-sm text-amber-200">{t("offline.watchUnavailable")}</p>
          )}
          {exactActiveSession && view && <DraftSpectatorDashboard view={view} />}

          <button
            type="button"
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
            onClick={leaveAndNavigate}
          >
            {t("spectator.backToLobby")}
          </button>
        </MenuPanel>
      </div>
    </div>
  );
}
