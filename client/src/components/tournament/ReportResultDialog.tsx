import { AnimatePresence, motion } from "framer-motion";
import { useId, useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";

import type { PodOutcome, TournamentPairingView } from "../../adapter/types";
import { FocusScope } from "../ui/FocusScope";

interface ReportResultDialogProps {
  isOpen: boolean;
  /**
   * The pairing whose result is being entered.
   *
   * There is deliberately **no `arity` prop**. The broker branches on
   * `pairing.players.len() == 2`, not on the tournament's `MatchArity`
   * (`validate_match_result`,
   * `crates/lobby-broker/src/tournament.rs:967-1021`), and the two differ in
   * production: a short pod at arity 3 seats two players
   * (`short_pod_size = arity - 1`, `:123-126`, reached by `partition_round`,
   * `:1058-1095`). Gating on the tournament's arity there would submit an
   * empty tally for a two-seat pairing, which the broker rejects every time.
   * Omitting the prop makes the wrong authority unrepresentable rather than
   * merely unused.
   *
   * Entry state (winner, tally) is per-mount. A caller reusing one mounted
   * dialog across pairings should pass `key={pairing.id}` so entry state does
   * not carry over.
   */
  pairing: TournamentPairingView;
  onSubmit: (outcome: PodOutcome) => void;
  onCancel: () => void;
  submitting?: boolean;
  /** Stable destination when the invoking surface supplies one explicitly. */
  returnFocusRef?: RefObject<HTMLElement | SVGElement | null>;
}

/** What the organizer picked in the winner radio group. */
type ResultSelection =
  | { readonly kind: "draw" }
  | { readonly kind: "winner"; readonly playerKey: string };

/**
 * Result-entry dialog for one pairing.
 *
 * Composed from the same primitives as `ConcedeDialog` — `FocusScope`,
 * `AnimatePresence`, `aria-modal`, `useId()`-linked `aria-labelledby` — with
 * one deliberate deviation: **`role="dialog"`, not `role="alertdialog"`**.
 * Per WAI-ARIA, `alertdialog` is for an urgent interruption demanding
 * immediate acknowledgement, which is what conceding a game is. This is a
 * result-entry form with radios and numeric inputs, opened deliberately by an
 * organizer; announcing it as an alert would be wrong. Do not "correct" this
 * to match the `ConcedeDialog` template.
 */
export function ReportResultDialog({
  isOpen,
  pairing,
  onSubmit,
  onCancel,
  submitting = false,
  returnFocusRef,
}: ReportResultDialogProps) {
  const { t } = useTranslation("tournament");
  const titleId = useId();
  const radioName = useId();
  const overlayRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const [selection, setSelection] = useState<ResultSelection | null>(null);
  const [gameWins, setGameWins] = useState<Record<string, number | undefined>>({});
  const title = t("report.heading");

  // The sole gate on game-wins inputs: the pairing's own seat count, which is
  // what `validate_match_result` branches on. At three or more seats the
  // broker rejects any non-empty map unconditionally ("Pod results are
  // single-game per MSTR"), so rendering inputs there would build a request
  // that can never succeed.
  const isHeadToHead = pairing.players.length === 2;

  function submit() {
    if (selection === null) return;
    if (selection.kind === "draw") {
      // The unit variant crosses the wire as the bare string, never `{Draw:{}}`.
      onSubmit("Draw");
      return;
    }
    const tally: Record<string, number> = {};
    if (isHeadToHead) {
      for (const seat of pairing.players) {
        tally[seat.player_key] = gameWins[seat.player_key] ?? 0;
      }
    }
    // Submitted exactly as entered. Bo3 legality and the winner-versus-tally
    // consistency check belong to `validate_match_result`
    // (`crates/lobby-broker/src/tournament.rs:967-1021`) alone; a second copy
    // here would be a drifting duplicate of a rule the server owns.
    onSubmit({ Decisive: { winner: selection.playerKey, game_wins: tally } });
  }

  return (
    <FocusScope
      active={isOpen}
      containerRef={dialogRef}
      ownerRef={overlayRef}
      initialFocusRef={cancelRef}
      returnFocusRef={returnFocusRef}
      onEscape={onCancel}
    >
      {({ onKeyDown }) => (
        <AnimatePresence>
          {isOpen && (
            <div
              ref={overlayRef}
              className="fixed inset-0 z-50 flex items-center justify-center"
              onKeyDown={onKeyDown}
            >
              <motion.button
                type="button"
                className="absolute inset-0 bg-black/70"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                onClick={onCancel}
                aria-label={t("common:actions.closeNamed", { name: title })}
              />
              <motion.div
                ref={dialogRef}
                role="dialog"
                aria-modal="true"
                aria-labelledby={titleId}
                tabIndex={-1}
                className="relative z-10 w-80 rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700"
                initial={{ opacity: 0, scale: 0.9 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.9 }}
                transition={{ type: "spring", stiffness: 300, damping: 25 }}
              >
                <h2 id={titleId} className="mb-3 text-xl font-bold text-white">
                  {title}
                </h2>

                <fieldset className="mb-4 flex flex-col gap-1">
                  <legend className="mb-1 text-xs text-gray-400">
                    {t("report.winnerLabel")}
                  </legend>
                  {pairing.players.map((seat) => (
                    <label
                      key={seat.player_key}
                      className="flex items-center gap-2 text-sm text-gray-200"
                    >
                      <input
                        type="radio"
                        name={radioName}
                        checked={
                          selection?.kind === "winner" &&
                          selection.playerKey === seat.player_key
                        }
                        onChange={() =>
                          setSelection({ kind: "winner", playerKey: seat.player_key })
                        }
                      />
                      {seat.display_name}
                    </label>
                  ))}
                  <label className="flex items-center gap-2 text-sm text-gray-200">
                    <input
                      type="radio"
                      name={radioName}
                      checked={selection?.kind === "draw"}
                      onChange={() => setSelection({ kind: "draw" })}
                    />
                    {t("report.drawOption")}
                  </label>
                </fieldset>

                {isHeadToHead && (
                  <fieldset className="mb-4 flex flex-col gap-2">
                    <legend className="mb-1 text-xs text-gray-400">
                      {t("report.gameWinsLabel")}
                    </legend>
                    {pairing.players.map((seat) => (
                      <label
                        key={seat.player_key}
                        className="flex items-center justify-between gap-2 text-sm text-gray-200"
                      >
                        {t("report.gameWinsFor", { name: seat.display_name })}
                        <input
                          type="number"
                          value={gameWins[seat.player_key] ?? 0}
                          onChange={(event) => {
                            const parsed = Number.parseInt(event.target.value, 10);
                            setGameWins((current) => ({
                              ...current,
                              [seat.player_key]: Number.isNaN(parsed) ? 0 : parsed,
                            }));
                          }}
                          className="w-16 rounded-[6px] border border-white/10 bg-black/30 px-2 py-1 text-sm text-gray-100"
                        />
                      </label>
                    ))}
                  </fieldset>
                )}

                <div className="flex justify-end gap-3">
                  <button
                    ref={cancelRef}
                    type="button"
                    onClick={onCancel}
                    className="rounded-lg bg-gray-700 px-5 py-2 text-sm font-semibold text-gray-200 transition hover:bg-gray-600"
                  >
                    {t("common:actions.cancel")}
                  </button>
                  <button
                    type="button"
                    onClick={submit}
                    // Disabled only while nothing is selected — with no
                    // selection there is no `PodOutcome` to construct at all.
                    // This is not a legality check: an inconsistent tally is
                    // submitted as entered and refused by the broker.
                    disabled={submitting || selection === null}
                    className="rounded-lg bg-emerald-600 px-5 py-2 text-sm font-semibold text-white transition hover:bg-emerald-500 disabled:bg-gray-700 disabled:text-gray-500"
                  >
                    {submitting ? t("report.submitting") : t("report.submit")}
                  </button>
                </div>
              </motion.div>
            </div>
          )}
        </AnimatePresence>
      )}
    </FocusScope>
  );
}
