import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { ScoreBadge } from "./ScoreBadge";

import type { StandingEntry } from "../../adapter/draft-adapter";

// ── Helpers ───────────────────────────────────────────────────────────

/** Game Win Percentage (GWP) — minimum 33% floor per WPN tiebreaker rules. */
function computeGwp(entry: StandingEntry): number {
  const totalGames = entry.game_wins + entry.game_losses;
  if (totalGames === 0) return 0;
  return Math.max(entry.game_wins / totalGames, 1 / 3);
}

function formatGwp(entry: StandingEntry): string {
  const totalGames = entry.game_wins + entry.game_losses;
  if (totalGames === 0) return "-";
  return `${(computeGwp(entry) * 100).toFixed(0)}%`;
}

// ── Component ───────────────────────────────────────────────────────────

/** Swiss tournament standings sorted by match wins (GWP tiebreaker), with current round pairings and live game scores. */
export function StandingsTable() {
  const standings = useMultiplayerDraftStore((s) => s.standings);
  const currentRound = useMultiplayerDraftStore((s) => s.currentRound);
  const localSeat = useMultiplayerDraftStore((s) => s.seatIndex);
  const pairings = useMultiplayerDraftStore((s) => s.pairings);

  if (standings.length === 0) return null;

  // Sort by match_wins desc, then GWP desc, then fewer losses
  const sorted = [...standings].sort((a, b) => {
    const winDiff = b.match_wins - a.match_wins;
    if (winDiff !== 0) return winDiff;
    const gwpDiff = computeGwp(b) - computeGwp(a);
    if (Math.abs(gwpDiff) > 0.001) return gwpDiff;
    return a.match_losses - b.match_losses;
  });

  return (
    <div className="rounded-xl border border-white/10 bg-black/30 p-4">
      <h3 className="text-lg font-medium text-white mb-3">
        Standings — Round {currentRound + 1}
      </h3>
      <table className="w-full text-sm text-white/80">
        <thead>
          <tr className="border-b border-white/10 text-left text-white/50">
            <th className="pb-2 pr-4">#</th>
            <th className="pb-2 pr-4">Player</th>
            <th className="pb-2 pr-4">Record</th>
            <th className="pb-2 pr-4">GWP</th>
          </tr>
        </thead>
        <tbody>
          {sorted.map((entry, i) => (
            <tr
              key={entry.seat_index}
              className={
                entry.seat_index === localSeat ? "text-emerald-300" : ""
              }
            >
              <td className="py-1 pr-4 text-white/40">{i + 1}</td>
              <td className="py-1 pr-4">{entry.display_name}</td>
              <td className="py-1 pr-4 tabular-nums">
                {entry.match_wins}-{entry.match_losses}
              </td>
              <td className="py-1 pr-4 tabular-nums text-white/50">
                {formatGwp(entry)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      {/* Current round pairings with live game scores */}
      {pairings.length > 0 && (
        <div className="mt-4 border-t border-white/10 pt-3">
          <h4 className="text-sm font-medium text-white/60 mb-2">
            Current Pairings
          </h4>
          {pairings.map((p) => (
            <div
              key={p.match_id}
              className="flex items-center gap-2 text-sm py-1"
            >
              <span className="text-white/80">{p.name_a}</span>
              {p.score_a != null && p.score_b != null && (
                <ScoreBadge
                  score={{ p0_wins: p.score_a, p1_wins: p.score_b, draws: 0 }}
                  player={0}
                />
              )}
              <span className="text-white/30">vs</span>
              {p.score_a != null && p.score_b != null && (
                <ScoreBadge
                  score={{ p0_wins: p.score_a, p1_wins: p.score_b, draws: 0 }}
                  player={1}
                />
              )}
              <span className="text-white/80">{p.name_b}</span>
              <span className="ml-auto text-white/40 text-xs">{p.status}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
