import { useTranslation } from "react-i18next";
import type { PlaytestPhase } from "../../stores/playtestStore";

interface Props {
  phase: PlaytestPhase;
  turnNumber: number;
  goingFirst: boolean;
}

const PHASE_CONFIG: Record<
  PlaytestPhase,
  { labelKey: string; color: string; pulse: boolean }
> = {
  idle:       { labelKey: "phase.idle",       color: "text-slate-400",   pulse: false },
  loading:    { labelKey: "phase.loading",     color: "text-blue-400",    pulse: true  },
  mulligan:   { labelKey: "phase.mulligan",    color: "text-yellow-300",  pulse: false },
  bottoming:  { labelKey: "phase.bottoming",   color: "text-orange-300",  pulse: false },
  playing:    { labelKey: "phase.playing",     color: "text-emerald-300", pulse: false },
  cleanup:    { labelKey: "phase.cleanup",     color: "text-red-400",     pulse: true  },
  simulating: { labelKey: "phase.simulating",  color: "text-indigo-300",  pulse: true  },
  error:      { labelKey: "phase.error",       color: "text-red-500",     pulse: false },
};

export function PlaytestPhaseIndicator({ phase, turnNumber, goingFirst }: Props) {
  const { t } = useTranslation("playtest");
  const cfg = PHASE_CONFIG[phase];

  return (
    <div className="flex items-center gap-3 rounded-xl border border-white/8 bg-black/18 px-3 py-2">
      {/* Phase dot */}
      <span
        className={`h-2 w-2 rounded-full ${
          phase === "playing" ? "bg-emerald-400" :
          phase === "mulligan" || phase === "bottoming" ? "bg-yellow-400" :
          phase === "cleanup" ? "bg-red-500" :
          phase === "loading" || phase === "simulating" ? "bg-blue-400" :
          phase === "error" ? "bg-red-600" :
          "bg-slate-500"
        } ${cfg.pulse ? "animate-pulse" : ""}`}
      />

      {/* Phase label */}
      <span className={`text-xs font-medium uppercase tracking-wide ${cfg.color}`}>
        {t(cfg.labelKey)}
      </span>

      {/* Turn badge (only during play) */}
      {phase === "playing" || phase === "cleanup" ? (
        <span className="ml-auto rounded-md bg-white/8 px-2 py-0.5 text-xs text-slate-300">
          {t("turn", { turn: turnNumber })}
        </span>
      ) : null}

      {/* Going first/second badge */}
      {(phase === "mulligan" || phase === "bottoming" || phase === "playing") && (
        <span className="ml-auto rounded-md bg-white/6 px-2 py-0.5 text-xs text-slate-400">
          {goingFirst ? t("goingFirst") : t("goingSecond")}
        </span>
      )}
    </div>
  );
}
