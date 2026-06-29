import type { TournamentStanding } from "../../adapter/types";

interface StandingsTableProps {
  standings: TournamentStanding[];
  highlightPlayerKey?: string | null;
}

export function StandingsTable({ standings, highlightPlayerKey }: StandingsTableProps) {
  if (standings.length === 0) {
    return (
      <p className="text-sm text-white/50">Standings appear after the first round.</p>
    );
  }

  return (
    <div className="overflow-x-auto rounded-xl border border-white/10">
      <table className="min-w-full text-left text-sm text-white/80">
        <thead className="bg-white/5 text-xs uppercase tracking-wide text-white/50">
          <tr>
            <th className="px-3 py-2">#</th>
            <th className="px-3 py-2">Player</th>
            <th className="px-3 py-2">Pts</th>
            <th className="px-3 py-2">Record</th>
            <th className="px-3 py-2">OMW%</th>
          </tr>
        </thead>
        <tbody>
          {standings.map((row, index) => {
            const highlighted = highlightPlayerKey === row.playerKey;
            return (
              <tr
                key={row.playerKey}
                className={highlighted ? "bg-amber-500/10" : "border-t border-white/5"}
              >
                <td className="px-3 py-2">{index + 1}</td>
                <td className="px-3 py-2">
                  {row.displayName}
                  {row.dropped && (
                    <span className="ml-2 text-xs text-red-300">(dropped)</span>
                  )}
                </td>
                <td className="px-3 py-2 font-medium">{row.matchPoints}</td>
                <td className="px-3 py-2">
                  {row.matchWins}-{row.matchLosses}-{row.matchDraws}
                </td>
                <td className="px-3 py-2">
                  {(row.omwPercentage * 100).toFixed(1)}%
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
