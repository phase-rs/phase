/**
 * Right sidebar for the playtest page.
 * Shows: hand quality meter, turn history, library / graveyard / exile counts,
 * and quick stats about the current board state.
 */
import { useTranslation } from "react-i18next";
import { DrawQualityMeter } from "./DrawQualityMeter";
import { TurnHistoryNav } from "./TurnHistoryNav";
import type { PlaytestCard, TurnSnapshot } from "../../services/playtestSession";
import type { PlaytestPermanent } from "../../services/playtestSession";

interface Props {
  hand: PlaytestCard[];
  battlefield: PlaytestPermanent[];
  graveyard: PlaytestCard[];
  exile: PlaytestCard[];
  library: PlaytestCard[];
  history: TurnSnapshot[];
  turnNumber: number;
  availableMana: number;
  mulliganCount: number;
  seed: number;
  onNewSeed: (seed: number) => void;
}

export function PlaytestSidebar({
  hand,
  battlefield,
  graveyard,
  exile,
  library,
  history,
  turnNumber,
  availableMana,
  mulliganCount,
  seed,
  onNewSeed,
}: Props) {
  const { t } = useTranslation("playtest");

  const lands = battlefield.filter((p) =>
    p.card.face.cardType?.coreTypes?.includes("Land"),
  ).length;
  const tapped = battlefield.filter((p) => p.tapped).length;

  return (
    <div className="flex h-full flex-col gap-3 overflow-y-auto p-3">
      {/* Hand quality */}
      <DrawQualityMeter
        hand={hand}
        availableMana={availableMana}
        turnNumber={turnNumber}
      />

      {/* Board stats */}
      <div className="rounded-xl border border-white/8 bg-black/18 p-3 space-y-2">
        <div className="text-[0.65rem] font-semibold uppercase tracking-wider text-slate-500">
          {t("sidebar.boardStats")}
        </div>
        <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
          <span className="text-slate-500">{t("sidebar.landsInPlay")}</span>
          <span className="text-right text-amber-300">{lands}</span>

          <span className="text-slate-500">{t("sidebar.tapped")}</span>
          <span className="text-right text-slate-400">{tapped}</span>

          <span className="text-slate-500">{t("sidebar.graveyard")}</span>
          <span className="text-right text-slate-400">{graveyard.length}</span>

          <span className="text-slate-500">{t("sidebar.exile")}</span>
          <span className="text-right text-slate-400">{exile.length}</span>

          <span className="text-slate-500">{t("sidebar.library")}</span>
          <span className="text-right text-blue-300">{library.length}</span>

          {mulliganCount > 0 && (
            <>
              <span className="text-slate-500">{t("sidebar.mulligans")}</span>
              <span className="text-right text-yellow-400">{mulliganCount}</span>
            </>
          )}
        </div>
      </div>

      {/* Turn history */}
      <div className="rounded-xl border border-white/8 bg-black/18 p-3">
        <TurnHistoryNav history={history} currentTurn={turnNumber} />
      </div>

      {/* Seed control */}
      <div className="rounded-xl border border-white/8 bg-black/18 p-3 space-y-2">
        <div className="text-[0.65rem] font-semibold uppercase tracking-wider text-slate-500">
          {t("sidebar.seed")}
        </div>
        <div className="flex gap-2">
          <input
            type="number"
            value={seed}
            onChange={(e) => onNewSeed(Number(e.target.value))}
            className="min-w-0 flex-1 rounded-lg border border-white/10 bg-black/20 px-2 py-1.5 text-xs text-white focus:border-blue-400 focus:outline-none"
          />
          <button
            type="button"
            onClick={() => onNewSeed(Math.floor(Math.random() * 0xffffffff))}
            className="rounded-lg border border-white/10 bg-white/8 px-2 py-1.5 text-xs text-slate-300 hover:bg-white/14"
            title={t("sidebar.randomSeed")}
          >
            🎲
          </button>
        </div>
        <p className="text-[0.6rem] text-slate-600">{t("sidebar.seedHint")}</p>
      </div>
    </div>
  );
}
