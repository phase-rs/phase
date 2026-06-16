import { useTranslation } from "react-i18next";
import type { PlaytestPhase } from "../../stores/playtestStore";

interface Props {
  phase: PlaytestPhase;
  landPlayedThisTurn: boolean;
  needsCleanup: boolean;
  discardNeeded: number;
  librarySize: number;
  onKeepHand: () => void;
  onMulligan: () => void;
  onDraw: () => void;
  onAdvanceTurn: () => void;
  onReset: () => void;
  onEndSession: () => void;
  onOpenSimulation: () => void;
  isLoading: boolean;
}

function CtrlButton({
  onClick,
  disabled,
  variant = "neutral",
  children,
}: {
  onClick: () => void;
  disabled?: boolean;
  variant?: "primary" | "danger" | "neutral" | "ghost";
  children: React.ReactNode;
}) {
  const base = "rounded-xl px-3 py-1.5 text-sm font-medium transition-colors disabled:opacity-40";
  const variants = {
    primary: "border border-emerald-400/40 bg-emerald-500/20 text-emerald-200 hover:bg-emerald-500/30",
    danger:  "border border-red-400/30 bg-red-500/15 text-red-300 hover:bg-red-500/25",
    neutral: "border border-white/10 bg-white/8 text-slate-200 hover:bg-white/14",
    ghost:   "border border-transparent text-slate-400 hover:text-slate-200",
  };
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={`${base} ${variants[variant]}`}
    >
      {children}
    </button>
  );
}

export function PlaytestControls({
  phase,
  landPlayedThisTurn: _landPlayedThisTurn,
  needsCleanup: _needsCleanup,
  discardNeeded,
  librarySize,
  onKeepHand,
  onMulligan,
  onDraw,
  onAdvanceTurn,
  onReset,
  onEndSession,
  onOpenSimulation,
  isLoading,
}: Props) {
  const { t } = useTranslation("playtest");

  const isMulligan = phase === "mulligan";
  const isBottoming = phase === "bottoming";
  const isPlaying = phase === "playing";
  const isCleanup = phase === "cleanup";

  return (
    <div className="flex flex-wrap items-center gap-2 rounded-xl border border-white/8 bg-black/18 px-3 py-2">
      {/* Mulligan phase */}
      {isMulligan && (
        <>
          <CtrlButton onClick={onKeepHand} variant="primary" disabled={isLoading}>
            {t("controls.keepHand")}
          </CtrlButton>
          <CtrlButton onClick={onMulligan} disabled={isLoading}>
            {t("controls.mulligan")}
          </CtrlButton>
        </>
      )}

      {/* Bottoming phase — no controls, prompt shown in hand */}
      {isBottoming && (
        <span className="text-sm text-orange-300">
          {t("controls.bottomingPrompt")}
        </span>
      )}

      {/* Playing phase */}
      {(isPlaying || isCleanup) && (
        <>
          <CtrlButton
            onClick={onDraw}
            disabled={isLoading || librarySize === 0 || isCleanup}
            variant="neutral"
          >
            {t("controls.draw")}
          </CtrlButton>
          <CtrlButton
            onClick={onAdvanceTurn}
            disabled={isLoading || isCleanup}
            variant="primary"
          >
            {isCleanup
              ? t("controls.discardFirst", { count: discardNeeded })
              : t("controls.nextTurn")}
          </CtrlButton>
        </>
      )}

      {/* Separator */}
      <div className="mx-1 h-4 w-px bg-white/10" />

      {/* Always-visible controls */}
      <CtrlButton onClick={onOpenSimulation} variant="ghost" disabled={isLoading}>
        {t("controls.simulate")}
      </CtrlButton>
      <CtrlButton onClick={onReset} disabled={isLoading} variant="ghost">
        {t("controls.reset")}
      </CtrlButton>
      <CtrlButton onClick={onEndSession} variant="danger">
        {t("controls.exit")}
      </CtrlButton>
    </div>
  );
}
