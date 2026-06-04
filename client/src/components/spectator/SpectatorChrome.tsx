import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { useSpectatorMode } from "../../hooks/useSpectatorMode";

export function SpectatorChrome() {
  const { t } = useTranslation("game");
  const navigate = useNavigate();
  const isSpectator = useSpectatorMode();
  const spectators = useMultiplayerStore((s) => s.spectators);

  if (!isSpectator) return null;

  return (
    <div className="pointer-events-auto fixed left-1/2 top-3 z-[70] flex max-w-[min(92vw,36rem)] -translate-x-1/2 flex-col items-center gap-1">
      <div className="flex flex-wrap items-center justify-center gap-2 rounded-full border border-amber-400/35 bg-amber-950/90 px-4 py-1.5 text-xs font-semibold uppercase tracking-wider text-amber-100 shadow-lg backdrop-blur-sm">
        <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-amber-300" aria-hidden />
        {t("spectator.banner")}
        <button
          type="button"
          onClick={() => navigate("/multiplayer")}
          className="rounded-full border border-amber-200/25 bg-amber-500/20 px-2.5 py-0.5 text-[10px] font-medium normal-case tracking-normal text-amber-50 transition hover:bg-amber-500/35"
        >
          {t("spectator.leave")}
        </button>
      </div>
      {spectators.length > 0 && (
        <p className="rounded-lg bg-black/55 px-3 py-1 text-[10px] text-slate-300 backdrop-blur-sm">
          {t("spectator.watchingWith", { names: spectators.join(", ") })}
        </p>
      )}
    </div>
  );
}
