import { useTranslation } from "react-i18next";

import type { TournamentStanding } from "../../adapter/types";
import { formatTiebreakValue, tiebreakCells } from "../../pages/tournamentPageState";

interface TournamentStandingsTableProps {
  /**
   * Rendered in the exact order given. `standings()`
   * (`crates/lobby-broker/src/tournament.rs:905-935`) emits rows already
   * ranked, so re-sorting here would install a second ranking authority that
   * can disagree with the server's.
   */
  standings: readonly TournamentStanding[];
}

/**
 * The standings table.
 *
 * Three things this component deliberately does NOT do, all three of which
 * `components/draft/StandingsTable.tsx` does and none of which may be copied
 * from it: it does not subscribe to a store, it does not re-sort the rows, and
 * it does not compute any tiebreak value. Every number below is the broker's,
 * rendered; the only client-side decisions are which columns exist (a
 * projection of the `Tiebreaks` arm the broker chose) and how a number is
 * formatted for display.
 */
export function TournamentStandingsTable({ standings }: TournamentStandingsTableProps) {
  const { t } = useTranslation("tournament");

  if (standings.length === 0) {
    return <p className="text-sm text-gray-500">{t("standings.empty")}</p>;
  }

  // Header shape comes from the first row's arm. Per-row cells are joined back
  // onto these ids, so a row carrying a different arm renders an explicit
  // placeholder rather than a plausible-looking number under a foreign header.
  const headerCells = tiebreakCells(standings[0].tiebreaks);

  return (
    <table className="w-full text-left text-sm">
      <thead className="text-xs uppercase text-gray-500">
        <tr>
          <th scope="col">{t("standings.rank")}</th>
          <th scope="col">{t("standings.player")}</th>
          <th scope="col" title={t("standings.matchPointsTitle")}>
            {t("standings.matchPoints")}
          </th>
          <th scope="col" title={t("standings.matchesPlayedTitle")}>
            {t("standings.matchesPlayed")}
          </th>
          <th scope="col" title={t("standings.byesTitle")}>
            {t("standings.byes")}
          </th>
          {headerCells.map((cell) => (
            <th key={cell.id} scope="col" title={t(cell.titleKey)}>
              {t(cell.labelKey)}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {standings.map((row, index) => {
          const cells = new Map(tiebreakCells(row.tiebreaks).map((cell) => [cell.id, cell]));
          return (
            <tr key={row.player_key} className="border-t border-white/5 text-gray-200">
              <td>{index + 1}</td>
              <td>
                {row.display_name}
                {row.dropped && (
                  <span className="ml-2 rounded-[5px] border border-amber-300/20 bg-amber-500/15 px-1.5 py-0.5 text-xs text-amber-200">
                    {t("labels.dropped")}
                  </span>
                )}
              </td>
              <td>{row.match_points}</td>
              <td>{row.matches_played}</td>
              <td>{row.byes}</td>
              {headerCells.map((header) => {
                // The test is the cell's PRESENCE, never the truthiness of its
                // value: `0.0` is a legitimate tiebreak (a player with no
                // opponents yet), and testing the value instead would render
                // it as the absence placeholder.
                const cell = cells.get(header.id);
                return (
                  <td key={header.id}>{cell ? formatTiebreakValue(cell) : "—"}</td>
                );
              })}
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}
