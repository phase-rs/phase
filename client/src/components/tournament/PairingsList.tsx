import { Fragment } from "react";
import { useTranslation } from "react-i18next";

import type { TournamentPairingView } from "../../adapter/types";
import {
  decisiveGameWins,
  gameWinsEntries,
  isReportable,
  outcomeLabelKey,
} from "../../pages/tournamentPageState";

interface PairingsListProps {
  pairings: readonly TournamentPairingView[];
  /**
   * Supplied by phase 5 only for a viewer holding the organizer credential.
   * Presence alone does NOT make a row reportable — see the arm gate below.
   */
  onReport?: (pairing: TournamentPairingView) => void;
}

/**
 * Every round's pairings, rendered arity-polymorphically.
 *
 * There is **no arity branch anywhere in this file**. A bye (1 seat), a
 * head-to-head (2), a short pod (`arity - 1`) and a full pod all render
 * through the same code path, because `TournamentPairingView.players` is a
 * list in every case. The one place seat count could have leaked in — the
 * game-wins tally, which is empty for a pod per MSTR — is handled by the
 * broker's own emptiness rather than by counting seats here.
 */
export function PairingsList({ pairings, onReport }: PairingsListProps) {
  const { t } = useTranslation("tournament");

  if (pairings.length === 0) {
    return <p className="text-sm text-gray-500">{t("pairings.empty")}</p>;
  }

  // Grouped by round in the array order the broker emitted (byes included, in
  // generation order). A `Map` is used rather than a plain object because its
  // iteration order is insertion order for every key type — a plain object
  // would hoist these integer-like round keys into ascending numeric order.
  const rounds = new Map<number, TournamentPairingView[]>();
  for (const pairing of pairings) {
    const bucket = rounds.get(pairing.round);
    if (bucket) bucket.push(pairing);
    else rounds.set(pairing.round, [pairing]);
  }

  return (
    <div className="flex flex-col gap-4">
      {Array.from(rounds, ([round, roundPairings]) => (
        <section key={round} className="flex flex-col gap-2">
          <h3 className="text-xs uppercase tracking-wide text-gray-500">
            {t("pairings.round", { round })}
          </h3>
          <ul className="flex flex-col gap-2">
            {roundPairings.map((pairing) => {
              // `outcome === null` is a nullability test, not a discrimination
              // of the wire union: every arm test lives in the page-state
              // module behind a named export.
              const label =
                pairing.outcome === null
                  ? null
                  : outcomeLabelKey(pairing.outcome, pairing.players);
              const gameWins = decisiveGameWins(pairing.outcome);
              return (
                <li
                  key={pairing.id}
                  className="flex flex-col gap-1 rounded-[8px] border border-white/10 bg-black/20 px-3 py-2"
                >
                  <div className="flex items-center justify-between gap-3">
                    <span className="text-xs text-gray-500">
                      {t("pairings.table", { id: pairing.id })}
                    </span>
                    {/* Arm-gated, not merely prop-gated. `report_result`
                        (`crates/lobby-broker/src/tournament.rs:1741-1753`)
                        refuses `Bye` and `Forfeit` unconditionally before any
                        validation runs, so offering the action there would
                        build a request that can never succeed. An already
                        `Reported` pairing IS reportable again (`:1752`,
                        overwritten at `:1755`) — correcting a mistyped tally
                        is a legitimate organizer action — so the guard is
                        arm-selective, never "unresolved only". */}
                    {/* The one interactive control this component renders, and
                        therefore the only one the >= 44pt touch-target rule
                        applies to. `min-h-[44px]` is this repo's established
                        spelling of it (`components/lobby/LobbyView.tsx:332,348`,
                        `components/lobby/HostSetup.tsx:664`) — the `text-xs`
                        padding alone leaves the hit area well under it, and the
                        raw utility classes here bypass `menuButtonClass`, whose
                        `sm` size would otherwise have supplied `min-h-11`.
                        `inline-flex items-center` keeps the label centred once
                        the box is taller than its text. */}
                    {onReport && isReportable(pairing.outcome) && (
                      <button
                        type="button"
                        onClick={() => onReport(pairing)}
                        className="inline-flex min-h-[44px] items-center rounded-[6px] bg-emerald-600 px-2 py-1 text-xs font-medium text-white"
                      >
                        {t("detail.reportResult")}
                      </button>
                    )}
                  </div>
                  <span className="flex flex-wrap items-center gap-2 text-sm text-gray-200">
                    {pairing.players.map((seat, index) => (
                      <Fragment key={seat.player_key}>
                        {index > 0 && (
                          <span className="text-xs text-gray-500">
                            {t("pairings.versus")}
                          </span>
                        )}
                        <span>{seat.display_name}</span>
                      </Fragment>
                    ))}
                  </span>
                  <span className="text-xs text-gray-400">
                    {label === null
                      ? t("pairings.pending")
                      : "winner" in label
                        ? t(label.key, { winner: label.winner })
                        : t(label.key)}
                  </span>
                  {/* `gameWins &&` is a null-check on a helper's return, not a
                      union narrowing. `null` means "no decisive result here";
                      an empty record means "decisive, but a pod, so there is
                      no per-game tally" — both render nothing, for different
                      reasons, with no seat count consulted. */}
                  {gameWins && (
                    <span className="flex flex-wrap gap-3 text-xs text-gray-500">
                      {gameWinsEntries(gameWins, pairing.players).map((entry) => (
                        <span key={entry.playerKey}>
                          {t("outcome.gameWins", { name: entry.name, wins: entry.wins })}
                        </span>
                      ))}
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
        </section>
      ))}
    </div>
  );
}
