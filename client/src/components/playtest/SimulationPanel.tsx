/**
 * Monte Carlo simulation panel. Lets the user configure and run simulations,
 * then displays the results as bar charts and summary statistics.
 */
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useSimStats } from "../../hooks/useSimStats";
import { ManaAvailabilityChart } from "./ManaAvailabilityChart";

interface Props {
  onClose: () => void;
}

export function SimulationPanel({ onClose }: Props) {
  const { t } = useTranslation("playtest");
  const {
    result,
    isSimulating,
    config,
    setConfig,
    runSimulation,
    turnLabels,
    avgOpeningLands,
    avgOpeningSpells,
    avgMulligans,
    pctKeepFirst,
    gamesSimulated,
  } = useSimStats();

  const [showMana, setShowMana] = useState(true);
  const [showLands, setShowLands] = useState(true);
  const [showPlayable, setShowPlayable] = useState(false);
  const [showHand, setShowHand] = useState(false);

  const turns = result?.turns ?? [];

  function SeriesToggle({
    label,
    active,
    color,
    onToggle,
  }: {
    label: string;
    active: boolean;
    color: string;
    onToggle: () => void;
  }) {
    return (
      <button
        type="button"
        onClick={onToggle}
        className={`flex items-center gap-1.5 rounded-lg px-2 py-1 text-xs transition-colors ${
          active ? "bg-white/10 text-white" : "text-slate-500 hover:text-slate-300"
        }`}
      >
        <span
          className="h-2 w-2 shrink-0 rounded-full"
          style={{ background: active ? color : "#475569" }}
        />
        {label}
      </button>
    );
  }

  return (
    <div className="flex h-full flex-col overflow-hidden rounded-2xl border border-white/10 bg-gray-900">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-white/8 px-4 py-3">
        <h2 className="text-sm font-semibold text-white">{t("sim.title")}</h2>
        <button
          type="button"
          onClick={onClose}
          className="rounded-lg px-2 py-1 text-xs text-slate-400 hover:text-white"
        >
          {t("sim.close")}
        </button>
      </div>

      <div className="flex-1 overflow-y-auto p-4 space-y-5">
        {/* Config */}
        <section className="space-y-3">
          <div className="text-[0.65rem] font-semibold uppercase tracking-wider text-slate-500">
            {t("sim.config")}
          </div>

          <div className="grid grid-cols-2 gap-3">
            {/* Num games */}
            <label className="space-y-1">
              <span className="text-xs text-slate-400">{t("sim.numGames")}</span>
              <select
                value={config.numGames}
                onChange={(e) => setConfig({ numGames: Number(e.target.value) })}
                className="w-full rounded-lg border border-white/10 bg-black/20 px-2 py-1.5 text-xs text-white focus:border-blue-400 focus:outline-none"
              >
                {[50, 100, 200, 500, 1000].map((n) => (
                  <option key={n} value={n}>{n}</option>
                ))}
              </select>
            </label>

            {/* Num turns */}
            <label className="space-y-1">
              <span className="text-xs text-slate-400">{t("sim.numTurns")}</span>
              <select
                value={config.numTurns}
                onChange={(e) => setConfig({ numTurns: Number(e.target.value) })}
                className="w-full rounded-lg border border-white/10 bg-black/20 px-2 py-1.5 text-xs text-white focus:border-blue-400 focus:outline-none"
              >
                {[5, 7, 10, 15, 20].map((n) => (
                  <option key={n} value={n}>{n}</option>
                ))}
              </select>
            </label>

            {/* Going first */}
            <label className="flex items-center gap-2 text-xs text-slate-400">
              <input
                type="checkbox"
                checked={config.goingFirst}
                onChange={(e) => setConfig({ goingFirst: e.target.checked })}
                className="accent-blue-500"
              />
              {t("sim.goingFirst")}
            </label>

            {/* Auto-keep */}
            <label className="flex items-center gap-2 text-xs text-slate-400">
              <input
                type="checkbox"
                checked={config.autoKeep}
                onChange={(e) => setConfig({ autoKeep: e.target.checked })}
                className="accent-blue-500"
              />
              {t("sim.autoKeep")}
            </label>
          </div>

          <button
            type="button"
            onClick={() => runSimulation()}
            disabled={isSimulating}
            className="w-full rounded-xl border border-blue-400/30 bg-blue-500/20 py-2 text-sm font-medium text-blue-200 transition-colors hover:bg-blue-500/30 disabled:opacity-50"
          >
            {isSimulating ? t("sim.running") : t("sim.run", { games: config.numGames })}
          </button>
        </section>

        {/* Results */}
        {result && (
          <>
            {/* Summary cards */}
            <section className="grid grid-cols-2 gap-2">
              <div className="rounded-xl border border-white/8 bg-black/18 p-3">
                <div className="text-[0.6rem] uppercase tracking-wider text-slate-500">
                  {t("sim.avgOpeningLands")}
                </div>
                <div className="mt-1 text-xl font-bold text-amber-300">
                  {avgOpeningLands.toFixed(1)}
                </div>
                <div className="text-[0.65rem] text-slate-500">
                  {t("sim.avgOpeningSpells", { count: avgOpeningSpells.toFixed(1) })}
                </div>
              </div>
              <div className="rounded-xl border border-white/8 bg-black/18 p-3">
                <div className="text-[0.6rem] uppercase tracking-wider text-slate-500">
                  {t("sim.keepFirst")}
                </div>
                <div className="mt-1 text-xl font-bold text-emerald-300">
                  {(pctKeepFirst * 100).toFixed(0)}%
                </div>
                <div className="text-[0.65rem] text-slate-500">
                  {t("sim.avgMulligans", { avg: avgMulligans.toFixed(2) })}
                </div>
              </div>
              <div className="col-span-2 rounded-xl border border-white/8 bg-black/18 p-3">
                <div className="text-[0.6rem] uppercase tracking-wider text-slate-500">
                  {t("sim.gamesSimulated")}
                </div>
                <div className="mt-1 text-lg font-bold text-slate-200">
                  {gamesSimulated.toLocaleString()}
                </div>
              </div>
            </section>

            {/* Chart series toggles */}
            <div className="flex flex-wrap gap-1.5">
              <SeriesToggle label={t("chart.mana")} active={showMana} color="#60a5fa" onToggle={() => setShowMana((v) => !v)} />
              <SeriesToggle label={t("chart.lands")} active={showLands} color="#34d399" onToggle={() => setShowLands((v) => !v)} />
              <SeriesToggle label={t("chart.playable")} active={showPlayable} color="#f59e0b" onToggle={() => setShowPlayable((v) => !v)} />
              <SeriesToggle label={t("chart.handSize")} active={showHand} color="#a78bfa" onToggle={() => setShowHand((v) => !v)} />
            </div>

            {/* Chart */}
            <div className="rounded-xl border border-white/8 bg-black/18 p-3">
              <ManaAvailabilityChart
                turns={turns}
                showMana={showMana}
                showLands={showLands}
                showPlayable={showPlayable}
                showHand={showHand}
              />
            </div>

            {/* Per-turn table */}
            <section className="space-y-2">
              <div className="text-[0.65rem] font-semibold uppercase tracking-wider text-slate-500">
                {t("sim.perTurnTable")}
              </div>
              <div className="overflow-x-auto">
                <table className="w-full text-[0.65rem]">
                  <thead>
                    <tr className="border-b border-white/8 text-slate-500">
                      <th className="pb-1 pr-2 text-left">{t("sim.table.turn")}</th>
                      <th className="pb-1 pr-2 text-right">{t("sim.table.mana")}</th>
                      <th className="pb-1 pr-2 text-right">{t("sim.table.lands")}</th>
                      <th className="pb-1 text-right">{t("sim.table.playable")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {turns.map((turn) => (
                      <tr key={turn.turnNumber} className="border-b border-white/4 text-slate-300">
                        <td className="py-0.5 pr-2">{turnLabels[turn.turnNumber - 1]}</td>
                        <td className="py-0.5 pr-2 text-right text-blue-300">
                          {turn.avgAvailableMana.toFixed(1)}
                        </td>
                        <td className="py-0.5 pr-2 text-right text-emerald-300">
                          {turn.avgLandsInPlay.toFixed(1)}
                        </td>
                        <td className="py-0.5 text-right text-amber-300">
                          {turn.avgPlayableCount.toFixed(1)}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </section>
          </>
        )}
      </div>
    </div>
  );
}
