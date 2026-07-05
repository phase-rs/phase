import { type FormEvent, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useLocation, useNavigate, useSearchParams } from "react-router";

import { useAudioContext } from "../audio/useAudioContext";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { DraftSpectatorDashboard } from "../components/draft/DraftSpectatorDashboard";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuPanel } from "../components/menu/MenuShell";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { useDraftSpectatorStore } from "../stores/draftSpectatorStore";

type DraftSpectatorLocationState = {
  password?: string;
};

function isPasswordGateError(message: string | null): boolean {
  return message === "password_required" || message === "Wrong password";
}

export function DraftSpectatorPage() {
  const { t } = useTranslation("draft");
  const { t: tMultiplayer } = useTranslation("multiplayer");
  const navigate = useNavigate();
  const location = useLocation();
  const [searchParams] = useSearchParams();
  const code = (searchParams.get("code") ?? "").trim().toUpperCase();
  const routePassword =
    (location.state as DraftSpectatorLocationState | null)?.password ??
    searchParams.get("password") ??
    undefined;

  const [passwordInput, setPasswordInput] = useState(routePassword ?? "");
  const [spectatePassword, setSpectatePassword] = useState<string | undefined>(
    routePassword,
  );

  useAudioContext("menu");

  const status = useDraftSpectatorStore((s) => s.status);
  const view = useDraftSpectatorStore((s) => s.view);
  const error = useDraftSpectatorStore((s) => s.error);
  const watchDraft = useDraftSpectatorStore((s) => s.watchDraft);
  const leave = useDraftSpectatorStore((s) => s.leave);

  useEffect(() => {
    if (!code) return;
    void watchDraft(code, spectatePassword);
    return () => leave();
  }, [code, spectatePassword, watchDraft, leave]);

  const showPasswordForm =
    status === "error" && isPasswordGateError(error) && !view;

  const handlePasswordSubmit = (e: FormEvent) => {
    e.preventDefault();
    if (!passwordInput.trim()) return;
    setSpectatePassword(passwordInput);
  };

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
          {showPasswordForm && (
            <form className="flex flex-col gap-3" onSubmit={handlePasswordSubmit}>
              <p className="text-sm text-slate-300">
                {error === "Wrong password"
                  ? t("spectator.wrongPassword")
                  : t("spectator.passwordRequired")}
              </p>
              <input
                type="password"
                value={passwordInput}
                onChange={(e) => setPasswordInput(e.target.value)}
                placeholder={tMultiplayer("lobbyView.passwordPlaceholder")}
                className="w-full rounded-lg bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
                autoFocus
              />
              <button
                type="submit"
                disabled={!passwordInput.trim()}
                className={menuButtonClass({
                  tone: "cyan",
                  size: "sm",
                  disabled: !passwordInput.trim(),
                  className: "self-start",
                })}
              >
                {t("spectator.watchWithPassword")}
              </button>
            </form>
          )}
          {status === "error" && !showPasswordForm && (
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
