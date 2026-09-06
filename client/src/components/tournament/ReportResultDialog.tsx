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
   * Entry state (winner, tally) is scoped to **this** pairing by the component
   * itself — see {@link EntryState}. A caller may reuse one mounted dialog
   * across pairings freely; it does not need to pass `key={pairing.id}`, and
   * forgetting to would not carry a result from one pairing to another.
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
 * Everything the organizer has entered, **tagged with the pairing it is for**.
 *
 * The `pairingId` field is what makes the reset total. Entry state is replaced
 * wholesale whenever it no longer matches the rendered pairing, so a field
 * added to this interface later is reset by construction rather than by
 * someone remembering to extend a per-field reset list — the same posture as
 * the absent `arity` prop above: make the wrong state unrepresentable instead
 * of documenting a rule the caller must follow.
 */
interface EntryState {
  readonly pairingId: TournamentPairingView["id"];
  readonly selection: ResultSelection | null;
  readonly gameWins: Readonly<Record<string, number | undefined>>;
}

function emptyEntry(pairingId: TournamentPairingView["id"]): EntryState {
  return { pairingId, selection: null, gameWins: {} };
}

/**
 * The single authority for "entry state that belongs to this pairing": the
 * held state when it matches, a blank entry when it does not. Both the
 * render-phase reset and the game-wins updater go through this, so there is
 * one place the scoping rule can be read or changed.
 */
function entryFor(held: EntryState, pairingId: TournamentPairingView["id"]): EntryState {
  return held.pairingId === pairingId ? held : emptyEntry(pairingId);
}

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
  const [entered, setEntered] = useState<EntryState>(() => emptyEntry(pairing.id));
  const title = t("report.heading");

  // Reset **during render** when the pairing changes — React's documented
  // "adjusting state when a prop changes" pattern, not a `useEffect`. Two
  // reasons it is the render-phase form: an effect commits one paint still
  // showing the previous pairing's answer, and — the load-bearing half —
  // everything below reads `entry`, so a submit can never emit the previous
  // pairing's result even in the pass that triggered the reset.
  //
  // This is not a nicety. One shared seat between two pairings (which is what
  // every multi-round tournament produces) turns carried-over entry state into
  // a tally `validate_match_result` ACCEPTS
  // (`crates/lobby-broker/src/tournament.rs:1000-1009`), silently recording a
  // result the organizer never entered for this pairing.
  const entry = entryFor(entered, pairing.id);
  if (entry !== entered) setEntered(entry);
  const { selection, gameWins } = entry;

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
                          setEntered({
                            ...entry,
                            selection: { kind: "winner", playerKey: seat.player_key },
                          })
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
                      onChange={() => setEntered({ ...entry, selection: { kind: "draw" } })}
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
                            setEntered((current) => {
                              const base = entryFor(current, pairing.id);
                              return {
                                ...base,
                                gameWins: {
                                  ...base.gameWins,
                                  [seat.player_key]: Number.isNaN(parsed) ? 0 : parsed,
                                },
                              };
                            });
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
