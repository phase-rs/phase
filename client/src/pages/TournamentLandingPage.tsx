import { useCallback, useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import type { TournamentSummary } from "../adapter/types";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { useInShell } from "../components/chrome/ShellContext";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuPanel, MenuShell } from "../components/menu/MenuShell";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { CreateTournamentForm } from "../components/tournament/CreateTournamentForm";
import { TournamentListItem } from "../components/tournament/TournamentListItem";
import type {
  CreateTournamentRequest,
  TournamentSubscriptionHandlers,
} from "../services/tournamentClient";
import { useMultiplayerStore } from "../stores/multiplayerStore";
import {
  failureLabel,
  viewerRelation,
  viewerRoles,
  type FailureLabel,
} from "./tournamentPageState";

/**
 * The tournament landing page: the broker's open-tournament list, a creation
 * form, and join-by-code.
 *
 * Composition only. Every rendered fact is the broker's — the list arrives
 * whole on `TournamentListUpdate` and is rendered in the exact array order
 * given, never sorted, filtered or re-ranked here.
 */
export function TournamentLandingPage() {
  const { t } = useTranslation("tournament");
  const navigate = useNavigate();
  const embedded = useInShell();
  const joinCodeId = useId();
  const joinNameId = useId();

  // `null` is "no list has arrived yet", which is a different fact from `[]`
  // ("the broker says there are none") and renders different copy.
  const [tournaments, setTournaments] = useState<TournamentSummary[] | null>(
    null,
  );
  const [offline, setOffline] = useState(false);
  const [failure, setFailure] = useState<FailureLabel | null>(null);
  const [busy, setBusy] = useState<"create" | "join" | null>(null);
  const [joinCode, setJoinCode] = useState("");
  const [joinName, setJoinName] = useState("");

  /**
   * Whether this page is still on screen, readable from an async continuation.
   *
   * `handleCreate` and `handleJoin` both `await` an RPC and then write, and one
   * of those writes is a `navigate()` — which, unlike a `setState`, is NOT a
   * silent no-op after unmount. A viewer who clicks Create and then leaves for
   * another route before the broker answers would otherwise be yanked to
   * `/tournament/<code>` from wherever they had gone.
   *
   * This is `TournamentPage`'s `shownCode` guard in the shape this page needs.
   * There the identity worth scoping to is the `:code` route param, so the
   * comparison is against the code on screen; this is the list page and has no
   * `:code`, so the identity is simply "still mounted" — the same
   * effect-cleanup `cancelled` idiom the subscription effect below already uses
   * (`components/lobby/LobbyView.tsx`), lifted to a ref because a promise, un-
   * like a subscription handler, has no cleanup that could detach it.
   *
   * Declining these writes strands nothing. `createTournament` /
   * `joinTournament` have already recorded the minted credential in the store
   * before returning, so the tournament stays reachable from the list (badged
   * "Organizer"/"Entered") and by code — only this page's own navigation and
   * alert are dropped.
   *
   * Re-armed on every run rather than only on the first: `StrictMode`
   * (`App.tsx`) deliberately mounts, cleans up and re-mounts in development, so
   * a ref initialised once at `useRef(true)` and only ever cleared would leave
   * the guard permanently closed after that second mount.
   */
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  // One selector per action, as `MultiplayerPage` does — zustand actions are
  // stable across renders, so these are safe effect dependencies.
  const subscribeTournaments = useMultiplayerStore(
    (s) => s.subscribeTournaments,
  );
  const createTournament = useMultiplayerStore((s) => s.createTournament);
  const joinTournament = useMultiplayerStore((s) => s.joinTournament);
  const tournamentCredentials = useMultiplayerStore(
    (s) => s.tournamentCredentials,
  );

  useEffect(() => {
    let cancelled = false;
    let detach: (() => void) | null = null;

    // Built here rather than per render: `tournamentSubscribers` is a `Set`
    // keyed by object identity, so one handlers object must serve the whole
    // subscription lifetime. Churning it would thrash the shared
    // acquire/release refcount and, in the worst ordering, send
    // `UnsubscribeLobby` while another subscriber is still live.
    const handlers: TournamentSubscriptionHandlers = {
      onListUpdate: (list) => setTournaments(list),
      // `onTournamentRemoved` is deliberately NOT handled here. The store's
      // `tournamentListSnapshot` is a verbatim copy of the server's last list
      // push and there are no delta frames; filtering the list client-side
      // would invent a delta protocol the broker does not speak. The next
      // `TournamentListUpdate` replaces the list wholesale.
    };

    void (async () => {
      const d = await subscribeTournaments(handlers);
      // Unmounted while the connect was still in flight: detach anyway, or the
      // subscription leaks for the life of the socket (#4615). This is
      // `LobbyView.tsx`'s idiom verbatim.
      if (cancelled) {
        d?.();
        return;
      }
      if (d === null) {
        setOffline(true);
        return;
      }
      detach = d;
    })();

    return () => {
      cancelled = true;
      detach?.();
    };
  }, [subscribeTournaments]);

  const handleCreate = useCallback(
    async (req: CreateTournamentRequest) => {
      setBusy("create");
      setFailure(null);
      const r = await createTournament(req);
      // Every write below belongs to a page the viewer may already have left —
      // see `mounted`. The guard covers the whole continuation, `navigate`
      // included, because that is the write with a visible effect after unmount.
      if (!mounted.current) return;
      setBusy(null);
      if (!r.ok) {
        setFailure(failureLabel(r));
        return;
      }
      // The broker mints the code; the reply is the only authority for which
      // tournament was created, so nothing client-side may substitute for it.
      navigate(`/tournament/${r.value.code}`);
    },
    [createTournament, navigate],
  );

  const handleJoin = useCallback(async () => {
    setBusy("join");
    setFailure(null);
    // No client-side pre-validation of the code or the display name: the
    // broker validates both, and a second copy of its rules here would drift.
    const r = await joinTournament(joinCode, joinName);
    // Scoped exactly as `handleCreate`'s continuation is, and for the same
    // reason: a stale join must not navigate a page the viewer has left.
    if (!mounted.current) return;
    setBusy(null);
    if (!r.ok) {
      setFailure(failureLabel(r));
      return;
    }
    navigate(`/tournament/${r.value.code}`);
  }, [joinTournament, joinCode, joinName, navigate]);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      {/* Inside the shell the scene + particles are rendered once by AppShell. */}
      {!embedded && <MenuParticles />}
      <ScreenChrome onBack={() => navigate("/")} />

      <MenuShell
        eyebrow={t("page.eyebrow")}
        title={t("page.landingTitle")}
        description={t("page.landingDescription")}
        layout="stacked"
        contentWidthClass="max-w-3xl"
      >
        <div className="flex w-full flex-col gap-6">
          {(failure !== null || offline) && (
            <div
              role="alert"
              className="rounded-[10px] border border-red-300/20 bg-red-500/10 px-4 py-3 text-sm text-red-200"
            >
              {failure !== null
                ? "message" in failure
                  ? t(failure.key, { message: failure.message })
                  : t(failure.key)
                : t("errors.connectionLost")}
            </div>
          )}

          <section className="flex w-full flex-col gap-3">
            <h2 className="text-sm font-semibold uppercase tracking-wide text-slate-400">
              {t("list.heading")}
            </h2>
            {tournaments === null ? (
              <p className="text-sm text-gray-500">{t("list.loading")}</p>
            ) : tournaments.length === 0 ? (
              <p className="text-sm text-gray-500">{t("list.empty")}</p>
            ) : (
              <ul className="flex flex-col gap-2">
                {tournaments.map((summary) => {
                  // The badge lives in this `<li>`, not inside
                  // `TournamentListItem` — that component's props are frozen.
                  // `"spectating"` is suppressed because a list where every
                  // row reads "Spectating" is noise; "Organizer"/"Entered" is
                  // the at-a-glance fact the badge exists for.
                  const relation = viewerRelation(
                    viewerRoles(tournamentCredentials[summary.code]),
                  );
                  return (
                    <li
                      key={summary.code}
                      className="flex items-center gap-2"
                    >
                      {relation !== "spectating" && (
                        <span className="flex-shrink-0 rounded-[5px] border border-amber-300/20 bg-amber-500/15 px-1.5 py-0.5 text-xs font-semibold text-amber-200">
                          {t(`labels.${relation}`)}
                        </span>
                      )}
                      <TournamentListItem
                        summary={summary}
                        onOpen={(code) => navigate(`/tournament/${code}`)}
                      />
                    </li>
                  );
                })}
              </ul>
            )}
          </section>

          <MenuPanel>
            <CreateTournamentForm
              submitting={busy === "create"}
              onSubmit={(req) => {
                void handleCreate(req);
              }}
            />
          </MenuPanel>

          <MenuPanel>
            <form
              className="flex flex-col gap-3"
              onSubmit={(event) => {
                event.preventDefault();
                void handleJoin();
              }}
            >
              <h2 className="text-sm font-semibold uppercase tracking-wide text-slate-400">
                {t("join.heading")}
              </h2>
              <label className="flex flex-col gap-1 text-xs text-slate-400">
                <span id={joinCodeId}>{t("join.codeLabel")}</span>
                <input
                  aria-labelledby={joinCodeId}
                  value={joinCode}
                  onChange={(event) => setJoinCode(event.target.value)}
                  placeholder={t("join.codePlaceholder")}
                  className="rounded-[8px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-white"
                />
              </label>
              <label className="flex flex-col gap-1 text-xs text-slate-400">
                <span id={joinNameId}>{t("join.displayNameLabel")}</span>
                <input
                  aria-labelledby={joinNameId}
                  value={joinName}
                  onChange={(event) => setJoinName(event.target.value)}
                  placeholder={t("join.displayNamePlaceholder")}
                  className="rounded-[8px] border border-white/10 bg-black/30 px-3 py-2 text-sm text-white"
                />
              </label>
              <button
                type="submit"
                disabled={busy === "join"}
                className={menuButtonClass({
                  tone: "emerald",
                  size: "sm",
                  disabled: busy === "join",
                  className: "self-start",
                })}
              >
                {busy === "join" ? t("join.submitting") : t("join.submit")}
              </button>
            </form>
          </MenuPanel>
        </div>
      </MenuShell>
    </div>
  );
}
