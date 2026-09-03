import { useCallback, useEffect, useState } from "react";
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

/** Which control is mid-flight. One value, so two controls cannot both spin. */
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
    if (!r.ok) setFailure(failureLabel(r));
  }, [getTournament, code]);

  useEffect(() => {
    let cancelled = false;
    let detach: (() => void) | null = null;

    // Everything below is scoped to ONE code. Re-running for a different code
    // must not leave the previous tournament's view, removal or alert on
    // screen; React bails out of these when the value is already identical, so
    // this costs nothing on mount.
    setView(null);
    setRemoved(false);
    setFailure(null);
    setReporting(null);
    setOffline(false);

    // Built here, not per render: `tournamentSubscribers` is a `Set` keyed by
    // object identity, so exactly one handlers object must serve the whole
    // subscription lifetime or the shared acquire/release refcount thrashes.
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
   * `view`. The four gated RPCs settle on a `TournamentUpdate` **broadcast**
   * that carries no request-vs-broadcast discriminator
   * (`services/tournamentClient.ts`, module header part 4), so `{ok:true}` may
   * be another actor's view and `{ok:false}` may arrive after a foreign frame
   * already settled the promise. The alert is therefore **best-effort**, and
   * the rendered state is always the ambient subscription's. Do not "fix" this
   * into an authoritative signal.
   */
  const run = useCallback(
    async (
      kind: BusyKind,
      action: () => Promise<GatedTournamentRpcResult<TournamentUpdateReply>>,
    ): Promise<boolean> => {
      setBusy(kind);
      setFailure(null);
      const r = await action();
      setBusy(null);
      if (!r.ok) setFailure(failureLabel(r));
      return r.ok;
    },
    [],
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
        if (ok) setReporting(null);
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
                    <div className="flex flex-wrap gap-2">
                      <button
                        type="button"
                        onClick={handleStart}
                        disabled={busy === "start"}
                        className={menuButtonClass({
                          tone: "emerald",
                          size: "sm",
                          disabled: busy === "start",
                        })}
                      >
                        {busy === "start"
                          ? t("detail.startRoundBusy")
                          : t("detail.startRound")}
                      </button>
                      <button
                        type="button"
                        onClick={handleEnd}
                        disabled={busy === "end"}
                        className={menuButtonClass({
                          tone: "red",
                          size: "sm",
                          disabled: busy === "end",
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
                    disabled={busy === "drop"}
                    className={menuButtonClass({
                      tone: "amber",
                      size: "sm",
                      disabled: busy === "drop",
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
                <ReportResultDialog
                  isOpen
                  pairing={freshPairing}
                  submitting={busy === "report"}
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
