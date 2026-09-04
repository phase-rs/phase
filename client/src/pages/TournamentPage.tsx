import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "react-router";

import type {
  PodOutcome,
  TournamentPairingView,
  TournamentUpdateReply,
  TournamentView,
} from "../adapter/types";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { useInShell } from "../components/chrome/ShellContext";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuPanel, MenuShell } from "../components/menu/MenuShell";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { PairingsList } from "../components/tournament/PairingsList";
import { ReportResultDialog } from "../components/tournament/ReportResultDialog";
import { TournamentStandingsTable } from "../components/tournament/TournamentStandingsTable";
import type { TournamentSubscriptionHandlers } from "../services/tournamentClient";
import {
  useMultiplayerStore,
  type GatedTournamentRpcResult,
} from "../stores/multiplayerStore";
import {
  arityLabel,
  failureLabel,
  isActiveEntrant,
  myPairing,
  viewerRelation,
  viewerRoles,
  type FailureLabel,
} from "./tournamentPageState";

/**
 * Which action is mid-flight, or `null` for none.
 *
 * One slot — but the slot's arity is not what makes concurrent dispatch
 * impossible, the CONTROLS are: every action-dispatching control on this page
 * is disabled whenever `busy !== null`, never merely when `busy` equals its own
 * kind. Gating each control on its own kind alone left a live double-dispatch
 * window, since a second action's `setBusy` overwrites the slot and thereby
 * re-enables the FIRST action's control while its request is still in flight
 * (click Start, then End: `busy` moves `"start"` → `"end"` and Start's button
 * becomes clickable again with its own request unanswered).
 *
 * The value is therefore read for two different jobs. `!== null` gates every
 * control; the equality test picks one control's in-flight LABEL, and only
 * that. Keeping the label kind-specific is what still tells the viewer which
 * action is running while all of them are held.
 */
type BusyKind = "start" | "end" | "drop" | "report";

/**
 * One tournament's detail page: header, gated organizer and player controls,
 * the viewer's own pairing, standings, and the full pairing history.
 *
 * **The rendered state is always the ambient subscription's.** Nothing here
 * ever writes `view` from an RPC return value — see the note above `run`.
 */
export function TournamentPage() {
  const { code = "" } = useParams<{ code: string }>();
  const { t } = useTranslation("tournament");
  const navigate = useNavigate();
  const embedded = useInShell();

  const [view, setView] = useState<TournamentView | null>(null);
  const [offline, setOffline] = useState(false);
  const [failure, setFailure] = useState<FailureLabel | null>(null);
  const [removed, setRemoved] = useState(false);
  const [busy, setBusy] = useState<BusyKind | null>(null);
  const [reporting, setReporting] = useState<TournamentPairingView | null>(
    null,
  );

  /**
   * The code the page is showing **now**, readable from an async continuation.
   *
   * `seed`, `run` and `handleReport` close over the code their request was
   * issued FOR, so comparing that closed-over `code` against
   * `shownCode.current` scopes a settling RPC exactly as `onTournamentUpdate`
   * scopes a broadcast with `broadcastCode === code`. The ref is needed only
   * because a promise, unlike a subscription handler, has no cleanup that
   * could detach it when the route changes: the closure alone can only ever
   * compare a code to itself.
   *
   * EVERY write a continuation makes is either behind this guard or behind a
   * strictly stronger one. Four are behind this guard directly — the two
   * failure alerts, `run`'s `setBusy(null)` and `handleReport`'s
   * `setReporting(null)`. The fifth, the subscription effect's own
   * `setOffline(true)`, does not need it: that continuation is scoped by the
   * effect-instance `cancelled` flag instead, which additionally covers
   * unmount — a case `shownCode` alone does not reach. This is deliberately
   * not "guard the writes that looked reachable": each of the four unguarded
   * ones found so far turned out to have a concrete repro against a successor
   * tournament's page. The counterpart is that the subscription effect resets
   * each of those same pieces of state on a `:code` change, so declining a
   * stale write here never strands anything.
   *
   * Assigned in the subscription effect rather than during render, and
   * deliberately immediately before the state resets there. That placement
   * covers the one window the guard alone cannot: a continuation landing
   * between the commit that changed `code` and that effect still passes the
   * guard, but its write is then cleared by the `setFailure(null)` reset
   * queued behind it — React flushes pending passive effects before starting
   * the next render, so the two updates apply in that order. That much is
   * reasoned from React's scheduling; what the tests measure is the guard's
   * own path, where the effect has already run.
   */
  const shownCode = useRef(code);

  const subscribeTournaments = useMultiplayerStore(
    (s) => s.subscribeTournaments,
  );
  const getTournament = useMultiplayerStore((s) => s.getTournament);
  const startTournamentRound = useMultiplayerStore(
    (s) => s.startTournamentRound,
  );
  const endTournament = useMultiplayerStore((s) => s.endTournament);
  const dropFromTournament = useMultiplayerStore((s) => s.dropFromTournament);
  const reportMatchResult = useMultiplayerStore((s) => s.reportMatchResult);

  // ── Authority: three conjuncts, mirroring the broker's own three report
  // refusals — `authorize_player`'s token and dropped checks plus
  // `handle_report_match_result`'s seat check.

  // C1 — token possession. From the store's credential map, never from the view.
  const credential = useMultiplayerStore((s) => s.tournamentCredentials[code]);
  const roles = viewerRoles(credential);
  const relation = viewerRelation(roles); // display badge only, never a gate

  // C2 — server state. From the view, never from the credential: a successful
  // drop clears no credential (the store forgets one only on
  // `TournamentRemoved`), so `roles.has("player")` alone renders affordances
  // the broker refuses every time. `view === null` is the loading branch,
  // which renders no controls at all, so short-circuiting here is not a third
  // policy.
  const canPlayerAct =
    view !== null &&
    roles.has("player") &&
    isActiveEntrant(view, credential?.playerKey);

  // C3 — the seat conjunct, for reporting only: `myPairing` below.
  const mine = view === null ? null : myPairing(view, credential?.playerKey);

  /**
   * Re-fetches this tournament's view. `TournamentUpdate` broadcasts fire only
   * on mutation, so a page mounted on a quiet tournament would otherwise
   * render nothing forever.
   *
   * The reply is awaited only so a failure can be surfaced — it is **never**
   * written to `view`. The same frame reaches the ambient subscription, which
   * is the sole render source.
   */
  const seed = useCallback(async () => {
    const r = await getTournament(code);
    // Code-scoped like `onTournamentUpdate` below: `code` is the tournament
    // this request was issued for, `shownCode.current` the one on screen when
    // it settled. A failure for a tournament the viewer has already navigated
    // away from is dropped, never rendered against its successor.
    if (!r.ok && shownCode.current === code) setFailure(failureLabel(r));
  }, [getTournament, code]);

  useEffect(() => {
    let cancelled = false;
    let detach: (() => void) | null = null;

    // Everything below is scoped to ONE code. Re-running for a different code
    // must not leave the previous tournament's view, removal, alert or
    // in-flight control state on screen; React bails out of these when the
    // value is already identical, so this costs nothing on mount.
    //
    // `busy` is reset for the same reason as the rest, and the reset is the
    // ONLY thing that clears it across a navigation: the settling continuation
    // in `run` deliberately declines to (see there), so without this line an
    // action dispatched against the PREVIOUS tournament would render a stuck,
    // disabled control — "Ending…" — on a successor page that never dispatched
    // it. The two halves are a pair: this reset owns the navigation case, the
    // guard in `run` owns the stale-settlement case.
    //
    // The ref moves first, so that an RPC settling in the window between this
    // commit and this effect is covered by the reset that follows rather than
    // by the guard — see `shownCode`.
    shownCode.current = code;
    setView(null);
    setRemoved(false);
    setFailure(null);
    setReporting(null);
    setOffline(false);
    setBusy(null);

    // Built here, not per render: `tournamentSubscribers` is a `Set` keyed by
    // object identity, so a fresh handlers object on every render would churn
    // the shared acquire/release refcount on every keystroke of page state.
    // What this buys is ONE handlers object per subscribed code — not one for
    // the component's whole lifetime, which a `code` in the dependency array
    // rules out by construction.
    //
    // A `:code` change therefore really does re-subscribe, and with this page
    // as the only subscriber the refcount really does touch 0 on the way: React
    // runs the old cleanup (delete + `UnsubscribeLobby`) before this effect's
    // async `subscribeTournaments` re-acquires and sends a fresh
    // `SubscribeLobby`. Safe rather than free, on three counts — the first two
    // measured by the `:code` navigation test, the third by construction:
    //  1. the release drops the cached list snapshot
    //     (`detachSharedSubscription`), so the re-acquire cannot fan a previous
    //     tournament's stale list into the new handlers — a re-fan would show
    //     up there as a second seed for the new code;
    //  2. the re-acquire re-registers this connection with the broker's
    //     delivery set, which is per-connection, not per-subscriber
    //     (`multiplayerStore.ts`, `lobbySubscriptionRefCount`), and the new
    //     code's broadcasts do land on the re-attached handlers;
    //  3. any broadcast missed inside the gap is superseded by this run's own
    //     `seed()` below, which re-fetches the new code's view unconditionally.
    const handlers: TournamentSubscriptionHandlers = {
      // The code conjunct is load-bearing: `TournamentUpdate` is a broadcast
      // for every tournament on the socket, not just this one.
      onTournamentUpdate: (broadcastCode, next) => {
        if (broadcastCode === code) setView(next);
      },
      onTournamentRemoved: (broadcastCode) => {
        if (broadcastCode === code) setRemoved(true);
      },
      // Re-seed on any list push. This is the only signal a page gets that the
      // socket came back after a reconnect (`SubscribeLobby`'s
      // `ToSelf(TournamentListUpdate)`), and without it a detail page open
      // across a reconnect shows a stale view until someone mutates the
      // tournament. Provably non-recursive: `handle_get_tournament` emits
      // `ToSelf(TournamentUpdate)` and no `tournament_list_update()`, so the
      // re-seed cannot re-trigger itself.
      onListUpdate: () => {
        void seed();
      },
    };

    void (async () => {
      const d = await subscribeTournaments(handlers);
      // Unmounted while the connect was in flight: detach anyway (#4615).
      if (cancelled) {
        d?.();
        return;
      }
      if (d === null) {
        setOffline(true);
        return;
      }
      detach = d;
      // Sequenced AFTER the subscription resolves, so the `ToSelf` reply
      // cannot arrive before a listener exists to catch it.
      void seed();
    })();

    return () => {
      cancelled = true;
      detach?.();
    };
  }, [subscribeTournaments, seed, code]);

  /**
   * Runs one gated action and reports its failure.
   *
   * The result is used for the failure alert only. It is **never** written to
   * `view`. The four gated RPCs now settle on the broker's own
   * `TournamentActionAck` / `TournamentActionRejected` for **this exact
   * request** (`services/tournamentClient.ts`, module header parts 3-4), so
   * `{ok:false, reason:"rejected"}` is a reliable "the server refused *me*"
   * signal, not another actor's frame arriving in its place. The residual
   * "not confirmed" cases are `{reason:"unsupported"}` — a peer too old to
   * mint an ack, where the frame is still sent and very likely performed —
   * and, less commonly, `{reason:"timeout"}`: a correlated request also
   * ignores a bare `Error` (`services/tournamentClient.ts`, module header
   * part 5), so a fast-fail at the parse boundary or a lost/late ack settles
   * this way too, and neither means the action definitely did not happen.
   * `view` still comes only from the
   * ambient subscription, as a layering choice — the fan-out owns state, this
   * function owns the call — not as compensation for a signal that could not
   * be trusted.
   *
   * The alert is scoped, not unscoped, though: it belongs to the tournament
   * the action was dispatched for, so the write is gated on the page still
   * showing that tournament — see `shownCode`.
   */
  const run = useCallback(
    async (
      kind: BusyKind,
      action: () => Promise<GatedTournamentRpcResult<TournamentUpdateReply>>,
    ): Promise<boolean> => {
      setBusy(kind);
      setFailure(null);
      const r = await action();
      // `code` is the tournament this action was dispatched for; every caller
      // below sends it in the frame. Dropping these writes when the viewer has
      // moved on is the same scoping `onTournamentUpdate` applies to a
      // broadcast — one tournament's rejection must never be attributed to
      // another tournament's page, and neither must its settlement.
      //
      // `setBusy(null)` is inside the guard for a reason distinct from the
      // alert's: an unscoped clear lets a PREVIOUS tournament's settlement
      // re-enable a control that the tournament now on screen is holding
      // disabled for its OWN in-flight action, which is exactly the window a
      // duplicate dispatch needs. Skipping it strands nothing — the
      // subscription effect already cleared `busy` on the navigation that made
      // this continuation stale.
      if (shownCode.current === code) {
        setBusy(null);
        if (!r.ok) setFailure(failureLabel(r));
      }
      return r.ok;
    },
    [code],
  );

  const handleStart = useCallback(() => {
    void run("start", () => startTournamentRound(code));
  }, [run, startTournamentRound, code]);

  const handleEnd = useCallback(() => {
    if (!window.confirm(t("detail.endTournamentConfirm"))) return;
    void run("end", () => endTournament(code));
  }, [run, endTournament, code, t]);

  const handleDrop = useCallback(() => {
    if (!window.confirm(t("detail.dropConfirm"))) return;
    void run("drop", () => dropFromTournament(code));
  }, [run, dropFromTournament, code, t]);

  const handleReport = useCallback(
    (outcome: PodOutcome) => {
      if (reporting === null) return;
      const pairingId = reporting.id;
      void (async () => {
        const ok = await run("report", () =>
          reportMatchResult(code, pairingId, outcome),
        );
        // Scoped like every other continuation on this page. Not a no-op after
        // a navigation: the viewer may already have opened the SUCCESSOR
        // tournament's own report dialog, and an unscoped clear would close it
        // under them, silently discarding the selection they had entered
        // there. The dialog belonging to `code` was already dismissed by the
        // subscription effect's `setReporting(null)` reset.
        if (ok && shownCode.current === code) setReporting(null);
      })();
    },
    [run, reportMatchResult, code, reporting],
  );

  // Re-derived from the live view on every render, so a broadcast arriving
  // while the dialog is open cannot carry a stale seat list into the payload.
  // No `key={pairing.id}` is passed: the dialog's entry-state reset is
  // structural, and its own prop doc says a caller need not pass one.
  const freshPairing =
    reporting === null
      ? null
      : (view?.pairings.find((p) => p.id === reporting.id) ?? reporting);

  const arity = view === null ? null : arityLabel(view.summary.arity);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      {!embedded && <MenuParticles />}
      <ScreenChrome onBack={() => navigate("/tournament")} />

      <MenuShell
        eyebrow={t("page.eyebrow")}
        title={view?.summary.name}
        description={t("page.detailDescription", { code })}
        layout="stacked"
        contentWidthClass="max-w-4xl"
      >
        <div className="flex w-full flex-col gap-6">
          <button
            type="button"
            onClick={() => navigate("/tournament")}
            className={menuButtonClass({
              tone: "neutral",
              size: "xs",
              ghost: true,
              className: "self-start",
            })}
          >
            {t("page.backToList")}
          </button>

          {failure !== null && (
            <div
              role="alert"
              className="rounded-[10px] border border-red-300/20 bg-red-500/10 px-4 py-3 text-sm text-red-200"
            >
              {"message" in failure
                ? t(failure.key, { message: failure.message })
                : t(failure.key)}
            </div>
          )}

          {removed ? (
            <p className="text-sm text-gray-400">{t("errors.notFound")}</p>
          ) : offline ? (
            <p className="text-sm text-gray-400">{t("errors.connectionLost")}</p>
          ) : view === null || arity === null ? (
            <p className="text-sm text-gray-500">{t("detail.loading")}</p>
          ) : (
            <>
              <div className="flex flex-wrap items-center gap-2 text-xs">
                <span className="rounded-[5px] border border-white/10 bg-white/5 px-1.5 py-0.5 font-semibold text-slate-200">
                  {t("labels.code", { code })}
                </span>
                <span className="rounded-[5px] border border-cyan-300/20 bg-cyan-500/15 px-1.5 py-0.5 font-semibold text-cyan-200">
                  {t(`status.${view.summary.status}`)}
                </span>
                <span className="rounded-[5px] border border-indigo-300/20 bg-indigo-500/15 px-1.5 py-0.5 font-semibold text-indigo-200">
                  {t(`bracket.${view.summary.bracket}`)}
                </span>
                <span className="text-slate-400">
                  {"seats" in arity
                    ? t(arity.key, { seats: arity.seats })
                    : t(arity.key)}
                </span>
                {view.summary.current_round > 0 && (
                  <span className="text-slate-400">
                    {t("labels.roundOf", {
                      current: view.summary.current_round,
                      total: view.summary.total_rounds,
                    })}
                  </span>
                )}
                {/* Rendered for every relation, "Spectating" included: on a
                    single-tournament page the viewer's relation to THIS event
                    is exactly the thing worth stating. */}
                <span className="rounded-[5px] border border-amber-300/20 bg-amber-500/15 px-1.5 py-0.5 font-semibold text-amber-200">
                  {t(`labels.${relation}`)}
                </span>
              </div>

              {roles.has("organizer") && (
                <MenuPanel>
                  <div className="flex flex-col gap-3">
                    <h2 className="text-sm font-semibold uppercase tracking-wide text-slate-400">
                      {t("detail.organizerControls")}
                    </h2>
                    {/* `busy !== null` on both controls, `busy === "<kind>"`
                        on both labels — see `BusyKind`. The two tests are
                        deliberately different: one action in flight holds
                        EVERY control, while only the control whose action is
                        actually running changes what it says. */}
                    <div className="flex flex-wrap gap-2">
                      <button
                        type="button"
                        onClick={handleStart}
                        disabled={busy !== null}
                        className={menuButtonClass({
                          tone: "emerald",
                          size: "sm",
                          disabled: busy !== null,
                        })}
                      >
                        {busy === "start"
                          ? t("detail.startRoundBusy")
                          : t("detail.startRound")}
                      </button>
                      <button
                        type="button"
                        onClick={handleEnd}
                        disabled={busy !== null}
                        className={menuButtonClass({
                          tone: "red",
                          size: "sm",
                          disabled: busy !== null,
                        })}
                      >
                        {busy === "end"
                          ? t("detail.endTournamentBusy")
                          : t("detail.endTournament")}
                      </button>
                    </div>
                  </div>
                </MenuPanel>
              )}

              {/* `canPlayerAct`, not `roles.has("player")`. The broker
                  permanently refuses a second drop by design, and no
                  credential is cleared by a successful one — so a button
                  gated on possession alone can only ever produce an alert. */}
              {canPlayerAct && (
                <div className="flex flex-wrap gap-2">
                  <button
                    type="button"
                    onClick={handleDrop}
                    disabled={busy !== null}
                    className={menuButtonClass({
                      tone: "amber",
                      size: "sm",
                      disabled: busy !== null,
                    })}
                  >
                    {busy === "drop" ? t("detail.dropBusy") : t("detail.drop")}
                  </button>
                </div>
              )}

              <section className="flex flex-col gap-2">
                <h2 className="text-sm font-semibold uppercase tracking-wide text-slate-400">
                  {t("detail.yourPairing")}
                </h2>
                {mine === null ? (
                  <p className="text-sm text-gray-500">{t("detail.noPairing")}</p>
                ) : (
                  // The ONLY place `onReport` is ever supplied. `canPlayerAct`
                  // carries C2 (a dropped entrant keeps a live pairing in a pod
                  // with >=2 active seats, so neither `myPairing` nor
                  // `isReportable` refuses there) and `[mine]` carries C3.
                  <PairingsList
                    pairings={[mine]}
                    onReport={canPlayerAct ? setReporting : undefined}
                  />
                )}
              </section>

              <section className="flex flex-col gap-2">
                <h2 className="text-sm font-semibold uppercase tracking-wide text-slate-400">
                  {t("standings.heading")}
                </h2>
                <TournamentStandingsTable standings={view.standings} />
              </section>

              <section className="flex flex-col gap-2">
                <h2 className="text-sm font-semibold uppercase tracking-wide text-slate-400">
                  {t("pairings.heading")}
                </h2>
                {/* No `onReport`: reporting is player-authority and
                    seat-scoped, so it belongs on the viewer's own pairing
                    alone. */}
                <PairingsList pairings={view.pairings} />
              </section>

              {freshPairing !== null && (
                /* `busy !== null`, for the same reason the buttons above use
                   it: `submitting` is what disables the dialog's own submit
                   control, and gating it on `"report"` alone let a Drop or a
                   round start in flight leave the dialog live — and, in the
                   other direction, let a later action's `setBusy` clear the
                   flag with the report itself unanswered.
                   The cost, taken knowingly: `submitting` is ONE boolean
                   driving both the disabled state and the label, so a dialog
                   opened while some other action is in flight reads
                   "Submitting…" for that other action. Correct gating beats a
                   precise label here, and splitting the prop would mean
                   changing `ReportResultDialog`'s frozen interface. */
                <ReportResultDialog
                  isOpen
                  pairing={freshPairing}
                  submitting={busy !== null}
                  onSubmit={handleReport}
                  onCancel={() => setReporting(null)}
                />
              )}
            </>
          )}
        </div>
      </MenuShell>
    </div>
  );
}
