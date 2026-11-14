/**
 * Self-healing for a stale game screen — event-armed, no polling.
 *
 * The screen renders the last snapshot committed through the dispatch
 * pipeline, and every delivery path hands a fresh snapshot to that pipeline
 * exactly once. A delivery whose processing rejects is gone — nothing
 * retries it — so the screen keeps showing the previous state while the
 * engine (and every other client it serves) has moved on. Observed as a
 * pod-draft match host frozen on the "opponent is deciding their opening
 * hand" overlay after both players kept, while the guest played on (#7836).
 *
 * Recovery needs no wire traffic: the adapter already holds the newest
 * state this client is entitled to (the host's adapter asks its own engine;
 * a guest's adapter caches the last inbound state-bearing message).
 *
 * Causality, not a loop: every committed snapshot arms ONE deferred check;
 * the next commit replaces it. A check that finds screen and adapter in
 * agreement disarms — nothing runs again until the next commit. Only a
 * check that finds a persistent divergence re-commits the adapter snapshot
 * through the ordinary remote-update pipeline (dispatch mutex and the
 * store's commit gate still apply), and that commit arms the next check.
 * Steady state costs nothing; the arm delay doubles as transient-blip
 * protection (same reasoning as `STUCK_DEBOUNCE_MS`).
 */
import type { EngineSnapshot, GameState } from "../adapter/types";
import { debugLog } from "./debugLog";
import { isDispatchIdle, processRemoteUpdate } from "./dispatch";
import { useGameStore } from "../stores/gameStore";

/** Quiet time after a commit before its one-shot divergence check fires. */
export const WATCHDOG_ARM_DELAY_MS = 10_000;

/**
 * The slice of the state a viewer can see go stale. `waiting_for` carries the
 * pending-decision sets (the frozen overlay's data source); the rest pins the
 * game's coarse position so a stall outside any decision point still differs.
 *
 * Heuristic for the DEFERRED check only: a lost update that changes none of
 * these fields (a land play alters just a hand and the battlefield) is
 * invisible here. The positive-knowledge path (`resyncFromAdapter`) therefore
 * recommits without consulting this. The complete detector is an engine-owned
 * state revision surfaced through every transport — an engine + protocol
 * change, and out of scope here.
 */
export function stateFingerprint(state: GameState): string {
  return JSON.stringify({
    waiting_for: state.waiting_for,
    priority_player: state.priority_player,
    turn_number: state.turn_number,
    phase: state.phase,
    stack_len: state.stack.length,
  });
}

async function readAdapterSnapshot(): Promise<EngineSnapshot | null> {
  const { adapter } = useGameStore.getState();
  if (!adapter) return null;
  try {
    return await adapter.getSnapshot();
  } catch {
    // A guest without a cached state yet, or an adapter mid-teardown —
    // nothing to compare against, so nothing to heal.
    return null;
  }
}

/**
 * One unconditional recommit of the adapter's current snapshot. For the
 * delivery-failure handlers: a caught rejection is positive knowledge that
 * exactly one update was lost, so they re-sync immediately, no arm delay —
 * and without the fingerprint gate: the lost update may have changed only
 * state outside the coarse fingerprint, and the display layer must not judge
 * game-state equality. The store's commit gate still orders the commit.
 */
export async function resyncFromAdapter(reason: string): Promise<void> {
  const adapterBefore = useGameStore.getState().adapter;
  const snapshot = await readAdapterSnapshot();
  if (!snapshot) return;
  const { adapter, gameState } = useGameStore.getState();
  // The await may cross a game teardown/swap — a snapshot from the old
  // adapter must not be committed over the new game (same guard as the
  // deferred check).
  if (!gameState || adapter !== adapterBefore) return;
  debugLog(`stale-screen resync (${reason})`, "warn");
  await processRemoteUpdate(snapshot, [], undefined);
}

/**
 * Fire-and-forget form for the delivery-failure handlers: a resync that
 * itself rejects must not become an unhandled rejection with the screen
 * silently stale — log it; the armed watchdog (or the next successful
 * delivery) is the retry path.
 */
export function resyncFromAdapterSafely(reason: string): void {
  resyncFromAdapter(reason).catch((err) => {
    debugLog(
      `stale-screen resync failed (${reason}): ${err instanceof Error ? err.message : String(err)}`,
      "warn",
    );
  });
}

export interface StaleStateWatchdog {
  start(): void;
  stop(): void;
}

export function createStaleStateWatchdog(): StaleStateWatchdog {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let unsubscribe: (() => void) | null = null;
  let checking = false;
  // Lifecycle generation: stop() bumps it, and an in-flight check compares
  // its captured value after every await — a check that outlives its own
  // watchdog must neither recommit nor re-arm.
  let generation = 0;

  function disarm(): void {
    if (timer != null) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function arm(): void {
    disarm();
    timer = setTimeout(() => {
      timer = null;
      if (checking) return; // a still-running check re-arms on its own
      checking = true;
      check()
        .catch((err) => {
          // A rejected recommit committed nothing, so no store subscription
          // re-arms — re-arm here (only while active) or the stale screen
          // would outlive its own watchdog.
          debugLog(
            `stale-screen watchdog check failed: ${err instanceof Error ? err.message : String(err)}`,
            "warn",
          );
          if (unsubscribe) arm();
        })
        .finally(() => {
          checking = false;
        });
    }, WATCHDOG_ARM_DELAY_MS);
  }

  async function check(): Promise<void> {
    // A busy pipeline will normally re-arm through its own commit, but a
    // queue that drains through rejections commits nothing — keep our own
    // re-arm so that case still gets its check.
    if (!isDispatchIdle()) {
      arm();
      return;
    }
    const gen = generation;
    const adapterBefore = useGameStore.getState().adapter;
    const snapshot = await readAdapterSnapshot();
    const { adapter, gameState } = useGameStore.getState();
    // The await may cross a game teardown/swap or the watchdog's own
    // stop() — a snapshot from the old adapter or a stopped lifecycle
    // must not be compared to (or committed over) the live game.
    if (!snapshot || !gameState || adapter !== adapterBefore) return;
    if (gen !== generation) return;
    if (!isDispatchIdle()) {
      arm();
      return;
    }
    if (stateFingerprint(snapshot.state) === stateFingerprint(gameState)) return;
    debugLog("stale-screen resync (deferred check: snapshot diverged)", "warn");
    await processRemoteUpdate(snapshot, [], undefined);
  }

  return {
    start(): void {
      if (unsubscribe) return;
      unsubscribe = useGameStore.subscribe(
        (s) => s.gameState,
        () => arm(),
      );
      // The commit that installed this game predates start() — give it its
      // one check too.
      arm();
    },
    stop(): void {
      generation += 1;
      disarm();
      if (unsubscribe) {
        unsubscribe();
        unsubscribe = null;
      }
    },
  };
}
