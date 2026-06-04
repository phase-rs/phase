import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { DeckStats } from "../../stores/matchHistoryStore";
import { ManaSymbol } from "../mana/ManaSymbol";

interface PerDeckStatsProps {
  decks: DeckStats[];
}

type DeckSortKey = "games" | "winRate" | "avgTurns" | "lastPlayed";

function formatDuration(seconds: number): string {
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const m = Math.floor(seconds / 60);
  if (m >= 60) return `${Math.floor(m / 60)}h ${m % 60}m`;
  return `${m}m`;
}

function formatDate(ts: number): string {
  const days = Math.floor((Date.now() - ts) / 86400000);
  if (days < 1) return "today";
  if (days === 1) return "yesterday";
  if (days < 7) return `${days}d ago`;
  return new Date(ts).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

interface WinRateBarProps {
  winRate: number;
  wins: number;
  losses: number;
  draws: number;
}

function WinRateBar({ winRate, wins, losses, draws }: WinRateBarProps) {
  const total = wins + losses + draws;
  const lossRate = total > 0 ? losses / total : 0;
  const drawRate = total > 0 ? draws / total : 0;

  return (
    <div className="flex h-1.5 w-full overflow-hidden rounded-full bg-slate-700/50">
      <div className="bg-emerald-500 transition-all" style={{ width: `${winRate * 100}%` }} />
      <div className="bg-slate-500/60 transition-all" style={{ width: `${drawRate * 100}%` }} />
      <div className="bg-red-600 transition-all" style={{ width: `${lossRate * 100}%` }} />
    </div>
  );
}

export function PerDeckStats({ decks }: PerDeckStatsProps) {
  const { t } = useTranslation("history");
  const [sortKey, setSortKey] = useState<DeckSortKey>("games");
  const [expanded, setExpanded] = useState<string | null>(null);

  const sorted = [...decks].sort((a, b) => {
    switch (sortKey) {
      case "games": return b.record.total - a.record.total;
      case "winRate": return b.record.winRate - a.record.winRate;
      case "avgTurns": return a.avgTurns - b.avgTurns;
      case "lastPlayed": return b.lastPlayedAt - a.lastPlayedAt;
    }
  });

  if (decks.length === 0) return null;

  const colHeaders: { key: DeckSortKey; label: string }[] = [
    { key: "games", label: t("byDeck.record") },
    { key: "winRate", label: t("byDeck.winRate") },
    { key: "avgTurns", label: t("byDeck.avgTurns") },
    { key: "lastPlayed", label: t("byDeck.lastPlayed") },
  ];

  return (
    <section>
      <h2 className="mb-4 text-sm font-semibold uppercase tracking-wider text-slate-400">
        {t("byDeck.heading")}
      </h2>

      <div className="overflow-x-auto rounded-xl border border-slate-700/40">
        <table className="w-full min-w-[600px] text-sm">
          <thead className="border-b border-slate-700/40 bg-slate-900/60">
            <tr>
              <th className="px-4 py-2.5 text-left text-[10px] font-medium uppercase tracking-wider text-slate-500">
                {t("byDeck.deck")}
              </th>
              {colHeaders.map((col) => (
                <th key={col.key} className="px-3 py-2.5 text-right text-[10px] font-medium uppercase tracking-wider text-slate-500">
                  <button
                    type="button"
                    onClick={() => setSortKey(col.key)}
                    className={`transition-colors ${sortKey === col.key ? "text-slate-200" : "hover:text-slate-300"}`}
                  >
                    {col.label}
                    {sortKey === col.key && " ↓"}
                  </button>
                </th>
              ))}
            </tr>
          </thead>
          <tbody className="divide-y divide-slate-700/20">
            {sorted.map((deck) => (
              <>
                <tr
                  key={deck.deckName}
                  className="cursor-pointer transition-colors hover:bg-slate-800/30"
                  onClick={() => setExpanded(expanded === deck.deckName ? null : deck.deckName)}
                >
                  <td className="px-4 py-2.5">
                    <div className="flex items-center gap-2">
                      {deck.colors.length > 0 && (
                        <span className="flex shrink-0 items-center gap-0.5">
                          {deck.colors.map((c) => (
                            <ManaSymbol key={c} shard={c} size="xs" />
                          ))}
                        </span>
                      )}
                      <span className="font-medium text-slate-200">{deck.deckName}</span>
                      <span className="rounded bg-slate-700/40 px-1.5 py-0.5 text-xs text-slate-500">
                        {deck.record.total}
                      </span>
                    </div>
                  </td>
                  <td className="px-3 py-2.5 text-right">
                    <span className="font-medium text-slate-200">
                      {deck.record.wins}W / {deck.record.losses}L
                      {deck.record.draws > 0 ? ` / ${deck.record.draws}D` : ""}
                    </span>
                  </td>
                  <td className="px-3 py-2.5 text-right">
                    <span className={`font-semibold tabular-nums ${
                      deck.record.winRate >= 0.6
                        ? "text-emerald-400"
                        : deck.record.winRate < 0.4
                          ? "text-red-400"
                          : "text-slate-200"
                    }`}>
                      {Math.round(deck.record.winRate * 100)}%
                    </span>
                  </td>
                  <td className="px-3 py-2.5 text-right tabular-nums text-slate-400">
                    {deck.avgTurns > 0 ? deck.avgTurns.toFixed(1) : "—"}
                  </td>
                  <td className="px-3 py-2.5 text-right text-slate-500">
                    {formatDate(deck.lastPlayedAt)}
                  </td>
                </tr>
                {expanded === deck.deckName && (
                  <tr key={`${deck.deckName}-detail`} className="bg-slate-800/20">
                    <td colSpan={5} className="px-6 py-3">
                      <div className="flex flex-wrap gap-6">
                        <div className="flex flex-col gap-1.5 flex-1 min-w-[200px]">
                          <span className="text-xs text-slate-500">Win rate breakdown</span>
                          <WinRateBar
                            winRate={deck.record.winRate}
                            wins={deck.record.wins}
                            losses={deck.record.losses}
                            draws={deck.record.draws}
                          />
                          <div className="flex gap-3 text-xs">
                            <span className="text-emerald-400">{deck.record.wins} wins</span>
                            <span className="text-red-400">{deck.record.losses} losses</span>
                            {deck.record.draws > 0 && (
                              <span className="text-slate-400">{deck.record.draws} draws</span>
                            )}
                          </div>
                        </div>
                        <div className="flex flex-col gap-1">
                          <span className="text-xs text-slate-500">Avg. game duration</span>
                          <span className="text-sm text-slate-200">
                            {deck.avgDuration > 0 ? formatDuration(deck.avgDuration) : "—"}
                          </span>
                        </div>
                        <div className="flex flex-col gap-1">
                          <span className="text-xs text-slate-500">Avg. turns</span>
                          <span className="text-sm text-slate-200">
                            {deck.avgTurns > 0 ? deck.avgTurns.toFixed(1) : "—"}
                          </span>
                        </div>
                      </div>
                    </td>
                  </tr>
                )}
              </>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}
