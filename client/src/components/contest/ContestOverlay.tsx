import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { FocusEvent, KeyboardEvent, MouseEvent, PointerEvent, Ref } from "react";
import { useTranslation } from "react-i18next";

import type {
  ContestKind,
  ContestResult,
  ContestState,
  GameEvent,
  GameLogEntry,
  PlayerId,
} from "../../adapter/types.ts";
import { playContestCue } from "../../game/contestAudio.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { CONTEST_RESULT_BANNER_MS, useContestStore } from "../../stores/contestStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { GameMode } from "../../stores/gameStore.ts";
import { getOpponentDisplayName } from "../../stores/multiplayerStore.ts";

/** Wall-clock cadence of the local countdown redraw. Cosmetic only. */
const COUNTDOWN_REDRAW_MS = 100;

function ContestGlyph({ kind }: { kind: ContestKind }) {
  // Decorative: the contest is always named in text as well.
  return kind === "Staring" ? (
    <svg viewBox="0 0 64 40" className="h-10 w-16" aria-hidden="true" focusable="false">
      <path
        d="M4 20C14 6 26 2 32 2s18 4 28 18C50 34 38 38 32 38S14 34 4 20Z"
        fill="none"
        stroke="currentColor"
        strokeWidth="3"
      />
      <circle cx="32" cy="20" r="9" fill="currentColor" />
    </svg>
  ) : (
    <svg viewBox="0 0 64 40" className="h-10 w-16" aria-hidden="true" focusable="false">
      <path
        d="M4 18C16 8 24 10 32 14c8-4 16-6 28 4-8 14-18 20-28 20S12 32 4 18Z"
        fill="none"
        stroke="currentColor"
        strokeWidth="3"
      />
      <path d="M8 19c16 4 32 4 48 0" fill="none" stroke="currentColor" strokeWidth="3" />
    </svg>
  );
}

/** Clicks the engine has counted for `player`, preferring the fresher live counter.
 *  Live counters belong to one contest; another contest's are never shown. */
function countedClicks(
  contest: ContestState,
  progress: { contest: number | null; clicks: Record<number, number> },
  player: PlayerId,
): number {
  const confirmed = contest.contestants.find((c) => c.player === player)?.clicks ?? 0;
  const live = progress.contest === contest.id ? (progress.clicks[player] ?? 0) : 0;
  return Math.max(confirmed, live);
}

/**
 * The contestants this client plays: its own seat, or — in human hotseat, where one
 * client seats every non-AI player — each of them. The established seating
 * configuration (`aiSeatIds`, `gameMode`) decides; nothing is guessed from who is
 * "not me".
 */
function localContestSeats(
  contest: ContestState,
  gameMode: GameMode | null,
  me: PlayerId,
  aiSeatIds: readonly PlayerId[],
): PlayerId[] {
  const seats = contest.contestants.map((c) => c.player);
  if (gameMode === "local") return seats.filter((seat) => !aiSeatIds.includes(seat));
  return seats.filter((seat) => seat === me);
}

/** Display-only: the localized sentence for a settled contest. */
function useResultText(result: ContestResult, me: PlayerId): string {
  const { t } = useTranslation("game");
  if ("Cancelled" in result) return t("physicalContest.result.cancelled");
  if (result.Outcome === "Tie") return t("physicalContest.result.tie");
  const { winner } = result.Outcome.Decided;
  if (winner === me) return t("physicalContest.result.won");
  if (result.Outcome.Decided.loser === me) return t("physicalContest.result.lost");
  return t("physicalContest.result.winner", { name: getOpponentDisplayName(winner) });
}

function resultCue(result: ContestResult, me: PlayerId) {
  if ("Cancelled" in result || result.Outcome === "Tie") return "neutral" as const;
  return result.Outcome.Decided.winner === me ? ("win" as const) : ("lose" as const);
}

/**
 * The large click target for one local seat. Pointer, keyboard (Space/Enter) and
 * assistive-technology activation all click; the state is carried by text and a
 * dashed border as well as colour.
 */
function ContestButton({
  seat,
  caption,
  isActive,
  pulse,
  onClick: click,
  buttonRef,
}: {
  seat: PlayerId;
  caption: string | null;
  isActive: boolean;
  pulse: number;
  onClick: (seat: PlayerId) => void;
  buttonRef?: Ref<HTMLButtonElement>;
}) {
  const { t } = useTranslation("game");
  const onPointerDown = (event: PointerEvent<HTMLButtonElement>) => {
    if (event.pointerType === "mouse" && event.button !== 0) return;
    event.preventDefault();
    click(seat);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key !== " " && event.key !== "Enter") return;
    event.preventDefault();
    // The activation keys belong to the contest while it is on screen: they must not
    // bubble to the page's global Space / Enter shortcuts (pass priority, auto-pass).
    event.stopPropagation();
    // A held key auto-repeats; one press is one click.
    if (!event.repeat) click(seat);
  };
  // Assistive-technology activation arrives as a click with no pointer or key
  // event (`detail === 0`); pointer and keyboard activation are handled above.
  const onActivate = (event: MouseEvent<HTMLButtonElement>) => {
    if (event.detail === 0) click(seat);
  };
  return (
    <div className="flex w-full flex-col items-center gap-1">
      {caption && <span className="text-sm font-semibold text-slate-200">{caption}</span>}
      <button
        ref={buttonRef}
        type="button"
        aria-disabled={!isActive}
        onPointerDown={onPointerDown}
        onKeyDown={onKeyDown}
        onClick={onActivate}
        className={[
          "min-h-32 w-full touch-manipulation select-none rounded-2xl border-4 px-6 text-2xl font-black",
          "focus-visible:outline-4 focus-visible:outline-offset-4 focus-visible:outline-white",
          isActive
            ? "border-white bg-emerald-600 active:bg-emerald-500"
            : "cursor-not-allowed border-dashed border-slate-500 bg-slate-800 text-slate-400",
        ].join(" ")}
      >
        <span key={pulse} className="inline-block motion-safe:animate-pulse-once">
          {isActive ? t("physicalContest.button.active") : t("physicalContest.button.wait")}
        </span>
      </button>
    </div>
  );
}

/**
 * Eye to Eye / Mouth to Mouth. The Comprehensive Rules define no physical
 * contests, so an online game adapts them: both players keep clicking and the
 * first to stop for the inactivity timeout loses. The engine/host owns the
 * rules; this overlay renders the engine's contest state and relays clicks.
 */
function ContestPanel({ contest }: { contest: ContestState }) {
  const { t } = useTranslation("game");
  const me = usePlayerId();
  const adapter = useGameStore((s) => s.adapter);
  const gameMode = useGameStore((s) => s.gameMode);
  const aiSeatIds = useGameStore((s) => s.aiSeatIds);
  const progress = useContestStore((s) => s.progress);
  const localSeats = localContestSeats(contest, gameMode, me, aiSeatIds);
  const isParticipant = localSeats.length > 0;
  const paused = contest.paused_since_ms != null;
  const isActive = contest.phase === "Active";
  const isPreparing = contest.phase === "Pending" || contest.phase === "Preparing";

  // Highest sequence used locally per seat, for the contest it was counted in. The
  // next click is strictly above both it and what the engine has confirmed, so
  // duplicate/stale protection never rejects a genuine click — and a new contest
  // starts its own sequence from zero, never inheriting the last one's.
  const seqRef = useRef<{ contest: number; bySeat: Record<number, number> }>({
    contest: contest.id,
    bySeat: {},
  });
  const [pulse, setPulse] = useState(0);

  // Focus lifecycle of the modal. A timed contest cannot wait for the player to Tab
  // to the button, so focus moves to the first local control as the contest opens
  // (spectators have none, so the dialog itself) and returns to where it was when the
  // contest closes.
  const dialogRef = useRef<HTMLDivElement>(null);
  const firstControlRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    (firstControlRef.current ?? dialogRef.current)?.focus();
    return () => {
      if (previous?.isConnected) previous.focus();
    };
  }, []);
  // The dialog is modal: focus that leaves it (a backdrop click, Tab past the last
  // control) comes back, so the activation keys never land on the page behind.
  const keepFocusInside = (event: FocusEvent<HTMLDivElement>) => {
    if (event.relatedTarget instanceof Node && event.currentTarget.contains(event.relatedTarget)) {
      return;
    }
    (firstControlRef.current ?? event.currentTarget).focus();
  };

  const click = useCallback(
    (seat: PlayerId) => {
      if (!isActive || !adapter) return;
      if (seqRef.current.contest !== contest.id) seqRef.current = { contest: contest.id, bySeat: {} };
      const confirmed = contest.contestants.find((c) => c.player === seat)?.last_seq ?? 0;
      const seq = Math.max(seqRef.current.bySeat[seat] ?? 0, confirmed) + 1;
      seqRef.current.bySeat[seat] = seq;
      // A server (or a native host's server) hosts the contest and stamps the click
      // itself; otherwise this client hosts it, so stamp with its own clock — the
      // timebase the local driver advances the engine in.
      const routed = adapter.submitContestClick?.(contest.id, seq) ?? false;
      if (!routed) {
        if (!adapter.driveContest) return;
        useContestStore
          .getState()
          .bufferClick({ contest: contest.id, player: seat, seq, at_ms: performance.now() });
      }
      setPulse((n) => n + 1);
    },
    [adapter, contest.contestants, contest.id, isActive],
  );

  // Countdown: milliseconds until GO as of this snapshot, drawn down by local
  // elapsed time. The host's clock is never compared against ours.
  const snapshotAt = useMemo(
    () => ({ at: performance.now(), clock: contest.clock_ms }),
    [contest.clock_ms],
  );
  const [now, setNow] = useState(() => performance.now());
  useEffect(() => {
    if (!isPreparing || paused) return;
    const id = window.setInterval(() => setNow(performance.now()), COUNTDOWN_REDRAW_MS);
    return () => window.clearInterval(id);
  }, [isPreparing, paused]);
  const untilGoMs =
    contest.go_at_ms == null
      ? contest.config.countdown_ms
      : Math.max(
          0,
          contest.go_at_ms -
            snapshotAt.clock -
            (paused ? 0 : Math.max(0, now - snapshotAt.at)),
        );
  const countdown = Math.ceil(untilGoMs / 1000);

  // Audio cues on phase edges and each whole countdown second.
  const lastCountdown = useRef<number | null>(null);
  useEffect(() => {
    if (contest.phase === "Preparing" && countdown > 0 && lastCountdown.current !== countdown) {
      lastCountdown.current = countdown;
      playContestCue("tick");
    }
  }, [contest.phase, countdown]);
  useEffect(() => {
    if (isActive) playContestCue("go");
  }, [isActive]);

  const [first, second] = contest.contestants;
  const name = (player: PlayerId) =>
    player === me ? t("physicalContest.you") : getOpponentDisplayName(player);
  const status = paused
    ? t("physicalContest.status.paused")
    : !isParticipant
      ? t("physicalContest.status.spectating")
      : contest.phase === "Pending"
        ? t("physicalContest.status.pending")
        : contest.phase === "Preparing"
          ? t("physicalContest.status.preparing")
          : contest.phase === "Active"
            ? t("physicalContest.status.go")
            : t("physicalContest.status.resolving");

  return (
    <div
      ref={dialogRef}
      tabIndex={-1}
      onBlur={keepFocusInside}
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/75 p-4 outline-none"
      role="dialog"
      aria-modal="true"
      aria-labelledby="contest-title"
    >
      <div className="flex w-full max-w-md flex-col items-center gap-4 rounded-2xl border border-white/20 bg-slate-900 p-5 text-center text-white shadow-2xl">
        <ContestGlyph kind={contest.kind} />
        <h2 id="contest-title" className="text-xl font-semibold">
          {contest.kind === "Staring"
            ? t("physicalContest.title.staring")
            : t("physicalContest.title.breathHolding")}
        </h2>
        <p className="text-sm text-slate-300">
          {t("physicalContest.versus", {
            first: name(first.player),
            second: name(second.player),
          })}
        </p>
        <p className="text-sm text-slate-300">
          {t("physicalContest.instructions", {
            seconds: contest.config.inactivity_timeout_ms / 1000,
          })}
        </p>

        {/* Announced to assistive tech on each phase change, never per click. */}
        <p role="status" aria-live="polite" className="text-lg font-bold">
          {status}
        </p>
        {isPreparing && !paused && (
          <p className="text-6xl font-black tabular-nums" aria-hidden="true">
            {countdown}
          </p>
        )}

        {localSeats.map((seat, index) => (
          <ContestButton
            key={seat}
            buttonRef={index === 0 ? firstControlRef : undefined}
            seat={seat}
            // Hotseat seats two humans at one screen, so each gets a captioned button.
            caption={localSeats.length > 1 ? getOpponentDisplayName(seat) : null}
            isActive={isActive}
            pulse={pulse}
            onClick={click}
          />
        ))}

        <ul className="flex w-full justify-around text-base" aria-label={t("physicalContest.versus", {
          first: name(first.player),
          second: name(second.player),
        })}>
          {[first, second].map((contestant) => (
            <li key={contestant.player} className="tabular-nums">
              {t("physicalContest.clicks", {
                name: name(contestant.player),
                count: countedClicks(contest, progress, contestant.player),
              })}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

/** Banner announcing a settled contest to both players after the engine moves on. */
function ContestResultBanner() {
  const announcement = useContestStore((s) => s.announcement);
  const dismiss = useContestStore((s) => s.dismissAnnouncement);
  const me = usePlayerId();
  const id = announcement?.id;
  useEffect(() => {
    if (id == null) return;
    const timer = window.setTimeout(() => dismiss(id), CONTEST_RESULT_BANNER_MS);
    return () => window.clearTimeout(timer);
  }, [id, dismiss]);
  useEffect(() => {
    if (announcement) playContestCue(resultCue(announcement.result, me));
    // Cue once per announcement.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [announcement?.id]);
  if (!announcement) return null;
  return <ResultBannerBody announcement={announcement} me={me} />;
}

function ResultBannerBody({
  announcement,
  me,
}: {
  announcement: NonNullable<ReturnType<typeof useContestStore.getState>["announcement"]>;
  me: PlayerId;
}) {
  const text = useResultText(announcement.result, me);
  return (
    <div
      role="status"
      aria-live="polite"
      className="pointer-events-none fixed inset-x-0 top-24 z-50 flex justify-center px-4"
    >
      <p className="rounded-xl border border-white/30 bg-slate-900/95 px-5 py-3 text-lg font-bold text-white shadow-xl">
        {text}
      </p>
    </div>
  );
}

/** Interval between local-host ticks. Short against the contest's 2s inactivity timeout. */
const LOCAL_HOST_TICK_MS = 50;

/**
 * Local engines (single-player WASM, P2P host) have no server to host the running
 * contest, so this client drives it: every tick it hands the engine its own monotonic
 * clock, the clicks collected since the last tick, and the seats the engine clicks
 * for. The engine decides everything; this only supplies time and input, then
 * commits the resulting snapshot. Server transports expose no `driveContest`.
 */
function useLocalContestHost(active: boolean) {
  const adapter = useGameStore((s) => s.adapter);
  useEffect(() => {
    const drive = adapter?.driveContest?.bind(adapter);
    if (!active || !adapter || !drive) return;
    let stopped = false;
    let inFlight = false;
    // Accepted transitions whose snapshot read failed. The engine has moved on (its
    // next drive reports NotInContest), so nothing can recreate their events: they are
    // kept until a commit lands, in order and exactly once, and delivered before the
    // engine is driven any further.
    let owed: { generation: number; events: GameEvent[]; logEntries: GameLogEntry[] } | null = null;
    const commitOwed = async (): Promise<boolean> => {
      if (!owed) return true;
      const snapshot = await adapter.getSnapshot();
      const current = useGameStore.getState();
      if (stopped || current.adapter !== adapter || current.gameSessionGeneration !== owed.generation) {
        // A reset or new game replaced this session: the transition belongs to a game
        // that is gone, and committing it would overwrite the new one.
        owed = null;
        return false;
      }
      current.commitEngineSnapshot(snapshot, { events: owed.events, logEntries: owed.logEntries });
      owed = null;
      return true;
    };
    const tick = async () => {
      if (stopped || inFlight) return;
      inFlight = true;
      try {
        // The session this tick belongs to: a reset or new game while the engine call
        // is in flight must not have this tick's snapshot committed over it.
        const { gameSessionGeneration: generation, aiSeatIds } = useGameStore.getState();
        if (owed && owed.generation !== generation) owed = null;
        if (owed && !(await commitOwed())) return;
        const clicks = useContestStore.getState().drainClicks();
        // The seats the engine clicks for are the established AI seat bindings
        // (empty in human hotseat); a P2P host replaces them with its pregame seats.
        let result;
        try {
          result = await drive(performance.now(), aiSeatIds, clicks);
        } catch (error) {
          // The engine may never have seen these clicks: keep them for the next tick,
          // where the contest's sequence check rejects any it did accept.
          useContestStore.getState().restoreClicks(clicks);
          throw error;
        }
        // An adapter that publishes its own transitions has already committed this one.
        if (
          stopped ||
          result.published === true ||
          (result.step !== "Updated" && result.step !== "Settled")
        ) {
          return;
        }
        owed = {
          generation,
          events: [...(owed?.events ?? []), ...result.events],
          logEntries: [...(owed?.logEntries ?? []), ...result.log_entries],
        };
        await commitOwed();
      } catch (error) {
        // A failed tick is dropped, not fatal: the next one re-reads the engine's own
        // clock, and the contest's lifecycle never depends on a single call landing.
        // A transition the engine accepted stays in `owed` for that next tick.
        console.error("[contest] local host tick failed:", error);
      } finally {
        inFlight = false;
      }
    };
    const timer = window.setInterval(() => void tick(), LOCAL_HOST_TICK_MS);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [active, adapter]);
}

/**
 * Mount once in the game page: renders the running contest, then announces its
 * result. Records the engine's `PhysicalContestResolved` event so the result
 * outlives the `WaitingFor` that carried it.
 */
export function ContestOverlay() {
  const waitingFor = useGameStore((s) => s.waitingFor);
  const events = useGameStore((s) => s.events);
  const announce = useContestStore((s) => s.announce);
  useLocalContestHost(waitingFor?.type === "PhysicalContest");

  useEffect(() => {
    for (const event of events) {
      if (event.type === "PhysicalContestResolved") {
        announce(event.data.kind, event.data.participants, event.data.result);
      }
    }
  }, [events, announce]);

  return (
    <>
      {waitingFor?.type === "PhysicalContest" && (
        <ContestPanel contest={waitingFor.data.contest} />
      )}
      <ContestResultBanner />
    </>
  );
}
