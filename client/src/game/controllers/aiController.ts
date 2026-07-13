import { AI_BASE_DELAY_MS, AI_DELAY_VARIANCE_MS, PLAYER_ID } from "../../constants/game";
import { useGameStore } from "../../stores/gameStore";
import type { GameAction, GameState, WaitingFor } from "../../adapter/types";
import { AdapterError, AdapterErrorCode } from "../../adapter/types";
import { pressureMultiplier, STACK_PRESSURE_ELEVATED } from "../../utils/stackPressure";
import { effectiveStackPressure } from "../../utils/stackThroughput";
import { debugLog } from "../debugLog";
import { dispatchAction } from "../dispatch";
import { attemptStateRehydrate, isEnginePanic, notifyEngineLost, routePanic } from "../engineRecovery";
import type { OpponentController } from "./types";

/**
 * Hard stop on AI controller after this many total consecutive failures on
 * the same WaitingFor key — pre-fallback *and* post-fallback failures both
 * count. Previously the controller would spin indefinitely once post-fallback
 * failures started accumulating, generating 300k+ log lines per minute.
 */
const MAX_TOTAL_FAILURES = 6;

/** Per-seat config: each AI player has its own difficulty. Multiple seats
 *  can share a difficulty; the map is keyed by `playerId` so lookups match
 *  the `waiting_for.data.player` value that drives scheduling. */
export interface AISeatBinding {
  playerId: number;
  difficulty: string;
}

export interface AIControllerConfig {
  seats: AISeatBinding[];
}

export interface AIController extends OpponentController {
  start(): void;
  stop(): void;
  dispose(): void;
}

function isStateLost(err: unknown): boolean {
  return err instanceof AdapterError && err.code === AdapterErrorCode.STATE_LOST;
}

function choiceTypeKey(choiceType: string | Record<string, unknown>): string {
  if (typeof choiceType === "string") return choiceType;
  return Object.keys(choiceType)[0] ?? "Unknown";
}

function describeAiCardPredicateGuess(
  action: GameAction,
  waitingFor: WaitingFor | null | undefined,
  gameState: GameState | null | undefined,
): string | null {
  if (action.type !== "ChooseOption" || waitingFor?.type !== "NamedChoice") return null;
  if (choiceTypeKey(waitingFor.data.choice_type) !== "CardPredicateGuess") return null;

  const sourceId = waitingFor.data.source_id;
  const sourceName = sourceId == null ? null : gameState?.objects?.[sourceId]?.name;
  return sourceName == null
    ? `guesses ${action.data.choice}`
    : `guesses ${action.data.choice} for ${sourceName}`;
}

function waitingForFingerprint(waitingFor: WaitingFor | null | undefined): string {
  return JSON.stringify(waitingFor ?? null);
}

function waitingForDebugLabel(waitingFor: WaitingFor | null | undefined): string {
  if (waitingFor == null) return "none";
  const data = (waitingFor as { data?: { player?: number } }).data;
  const player = data?.player == null ? "unknown" : String(data.player);
  if (waitingFor.type !== "NamedChoice") return `${waitingFor.type} for player ${player}`;
  return `${waitingFor.type}/${choiceTypeKey(waitingFor.data.choice_type)} for player ${player}`;
}

export function createAIController(config: AIControllerConfig): AIController {
  let active = false;
  let pending = false;
  let timeoutId: ReturnType<typeof setTimeout> | null = null;
  let unsubscribe: (() => void) | null = null;

  // Failure tracking on the same WaitingFor state to break infinite loops.
  // `MAX_CONSECUTIVE_FAILURES` gates the normal→fallback transition; the
  // separate `MAX_TOTAL_FAILURES` hard-stops the controller so post-fallback
  // failures (e.g., engine rejecting even the safe fallback) cannot spin.
  let lastWaitingForKey: string | null = null;
  let consecutiveFailures = 0;
  let totalFailures = 0;
  const MAX_CONSECUTIVE_FAILURES = 3;

  const difficultyByPlayerId = new Map(config.seats.map((s) => [s.playerId, s.difficulty]));
  const aiPlayerIds = new Set(difficultyByPlayerId.keys());

  /**
   * Stable identity key for a WaitingFor — type + player so Priority{0} ≠ Priority{1}.
   *
   * For simultaneous-mulligan states (`MulliganDecision`,
   * `OpeningHandBottomCards`)
   * `data.player` is undefined, so falling back to -1 would collapse every
   * pending seat to the same key. We instead key by the AI seat that the
   * controller is currently driving, so failure counters reset between seats
   * and a failing P0 submission does not consume P1's budget.
   */
  function waitingForKey(wf: WaitingFor, drivingPlayerId: number | null): string {
    const data = (wf as { data?: { player?: number } }).data;
    const player = drivingPlayerId ?? data?.player ?? -1;
    return `${wf.type}:${player}`;
  }

  /**
   * CR 103.5: For simultaneous mulligan states, return the first AI-controlled
   * player in `pending` so the AI controller can act for them. Returns null
   * if no AI player is pending (the local human still owes a decision).
   */
  function aiPendingForMulligan(wf: {
    type: string;
    data?: { pending?: { player: number }[] };
  }): number | null {
    if (
      wf.type !== "MulliganDecision" &&
      wf.type !== "OpeningHandBottomCards"
    ) {
      return null;
    }
    const pending = wf.data?.pending ?? [];
    for (const entry of pending) {
      if (entry.player !== PLAYER_ID && aiPlayerIds.has(entry.player)) {
        return entry.player;
      }
    }
    return null;
  }

  function checkAndSchedule() {
    if (!active || pending) return;

    const state = useGameStore.getState().gameState;
    if (!state?.waiting_for) return;

    const waitingFor = state.waiting_for;

    // Game over -- stop scheduling
    if (waitingFor.type === "GameOver") return;

    // CR 103.5: Simultaneous mulligan — pending may contain multiple players;
    // route to the first AI seat that still owes a decision/bottom selection.
    // For all other states, use the single-player `data.player` path.
    let waitingPlayerId: number;
    const mulliganPid = aiPendingForMulligan(
      waitingFor as { type: string; data?: { pending?: { player: number }[] } },
    );
    if (mulliganPid !== null) {
      waitingPlayerId = mulliganPid;
    } else if (
      waitingFor.type === "MulliganDecision" ||
      waitingFor.type === "OpeningHandBottomCards"
    ) {
      // Local human is pending (or no AI players left in pending) — do nothing.
      return;
    } else {
      // Check if it's an AI player's turn — any non-human player is AI.
      // This is dynamic rather than gating on a static set so that
      // restoreGameState (debug panel import) with a different player count
      // works without rebuilding the controller.
      if (
        !("data" in waitingFor) ||
        !waitingFor.data ||
        // CR 732.2a: LoopShortcut carries `proposer`, not `player`; still route
        // via the engine-derived `priority_player` like every `player in` state.
        (!("player" in waitingFor.data) && waitingFor.type !== "LoopShortcut")
      )
        return;
      // CR 723.5: Under a turn-control effect (Emrakul, the Promised End /
      // Worst Fears / Mindslaver) the seat that must *submit* this decision is
      // the authorized submitter, NOT the semantic acting player
      // (`waiting_for.data.player`, which is the controlled seat). The engine is
      // the single authority for this and re-derives `priority_player` to the
      // authorized submitter (see `game/public_state.rs`
      // `sync_priority_player_from_waiting_for`). Driving the AI off
      // `data.player` would dispatch as the controlled seat, which the engine
      // rejects with `WrongPlayer` — the controller then burns through its
      // failure budget and hard-stops via `notifyEngineLost`, which surfaced as
      // a game crash when a human gained control of an AI's turn (#2012).
      //
      // Using the authorized submitter keeps the AI silent while a human
      // controls the turn (submitter is the human → bail), and makes the AI act
      // as the controller seat when an AI gains control of another seat's turn.
      // In games with no turn-control effect, `priority_player === data.player`
      // for every single-acting state, so this is a no-op.
      waitingPlayerId = state.priority_player;
      if (waitingPlayerId === PLAYER_ID) return;
    }

    // Stack-pressure deferral applies ONLY to discretionary Priority decisions.
    // Under pressure the batch-resolve path (gameLoopController → dispatchResolveAll
    // → engine `resolve_all`) drains the stack by passing priority, so the AI
    // controller steps aside to avoid racing it. But `resolve_all` is a
    // priority-only loop: it breaks the instant `waiting_for` leaves Priority
    // (engine-wasm/src/lib.rs) and hands any mandatory mid-resolution choice
    // (EffectZoneChoice, ChooseManaColor, scry/surveil, discard, resolution
    // targeting, …) back to the frontend. Those choices belong to THIS controller
    // and are exactly what lets the stack drain. Deferring them on stack size
    // deadlocks the game: stack ≥ 10 → AI won't choose → stack never shrinks →
    // batch path never restarts (it only fires on Priority). So skip only when
    // the AI's pending decision is Priority itself.
    const stackLen = state.stack?.length ?? 0;
    if (waitingFor.type === "Priority" && stackLen >= STACK_PRESSURE_ELEVATED) return;

    // Reset failure counters when the WaitingFor state changes (type or player).
    // `consecutiveFailures` gates normal→fallback escalation; `totalFailures`
    // is the absolute hard stop that kills the controller.
    const key = waitingForKey(waitingFor, mulliganPid);
    if (key !== lastWaitingForKey) {
      lastWaitingForKey = key;
      consecutiveFailures = 0;
      totalFailures = 0;
    }

    // Hard stop: if we've burned through both the normal and fallback paths
    // on the same key without progress, the engine is unrecoverably stuck
    // for this seat. Surface to the user instead of spinning. Previously
    // there was no absolute cap — fallback failures could loop indefinitely,
    // generating log storms.
    if (totalFailures >= MAX_TOTAL_FAILURES) {
      debugLog(
        `AI controller halting: ${totalFailures} failures on ${waitingFor.type}`,
        "error",
      );
      notifyEngineLost("ai-controller-stuck");
      stop();
      return;
    }

    if (consecutiveFailures >= MAX_CONSECUTIVE_FAILURES) {
      debugLog(
        `AI stuck: ${MAX_CONSECUTIVE_FAILURES} consecutive failures on ${waitingFor.type}, dispatching fallback`,
        "warn",
      );
      // Guard against re-entry: set pending so subscription callbacks during
      // the fallback dispatch don't trigger another fallback cascade.
      pending = true;
      // Resolve a guaranteed-legal escape action. A hardcoded empty combat
      // declaration is NOT always legal — CR 508.1d / CR 701.15b require
      // goaded / "attacks if able" creatures to be declared. Instead, ask the
      // engine for its legal-action list (the single authority for legality).
      // Non-priority legal actions are already scoped to the current
      // WaitingFor; Priority fallback keeps preferring PassPriority as the
      // least invasive escape.
      // CancelCast escapes a stuck casting flow; PassPriority is the final
      // fallthrough — never dispatch `undefined`.
      const fallbackPromise: Promise<GameAction> = state.has_pending_cast
        ? Promise.resolve<GameAction>({ type: "CancelCast" })
        : (() => {
            const { adapter } = useGameStore.getState();
            if (!adapter) return Promise.resolve<GameAction>({ type: "PassPriority" });
            return adapter.getLegalActions().then((result) => {
              if (waitingFor.type === "Priority") {
                return (
                  result.actions.find((a) => a.type === "PassPriority") ??
                  { type: "PassPriority" }
                );
              }
              return result.actions[0] ?? { type: "PassPriority" };
            });
          })();
      // Dispatch the fallback as the authorized submitter being unstuck —
      // NEVER as the local human (which `checkAndSchedule` already excludes via
      // the `waitingPlayerId === PLAYER_ID` early-return above, CR 723.5). The
      // engine guard would reject a non-authorized actor. A rejection from
      // getLegalActions routes into the existing .catch() below.
      fallbackPromise
        .then((fallback) => dispatchAction(fallback, waitingPlayerId))
        .then(() => {
          consecutiveFailures = 0;
          totalFailures = 0;
        })
        .catch((e) => {
          // Increment both counters to prevent infinite fallback retry.
          consecutiveFailures++;
          totalFailures++;
          debugLog(
            `AI fallback also failed (${consecutiveFailures}/${totalFailures}): ${e instanceof Error ? e.message : String(e)}`,
            "warn",
          );
        })
        .finally(() => {
          pending = false;
          if (active) checkAndSchedule();
        });
      return;
    }

    scheduleAction(waitingPlayerId);
  }

  function scheduleAction(playerId: number) {
    if (pending) return;
    pending = true;

    // Start computing immediately — in parallel with the artificial delay.
    // This turns additive latency (delay + compute) into max(delay, compute),
    // which matters most for VeryHard where the pool search takes 1-2 seconds.
    const { adapter, gameState } = useGameStore.getState();
    // Each seat has its own difficulty — a controller driving three AI players
    // can simultaneously run Easy, Medium, and VeryHard policies.
    const difficulty = difficultyByPlayerId.get(playerId) ?? "Medium";
    const waitingForType = gameState?.waiting_for?.type;
    const scheduledWaitingFor = gameState?.waiting_for ?? null;
    const scheduledWaitingForFingerprint = waitingForFingerprint(scheduledWaitingFor);
    const actionPromise: Promise<GameAction | null> = Promise.resolve(
      adapter?.getAiAction(difficulty, playerId, waitingForType) ?? null,
    );
    // Suppress unhandled-rejection warnings if stop() cancels the timeout
    // before it fires and nothing else awaits this promise.
    actionPromise.catch(() => {});

    // Mulligan is a binary keep/mulligan decision with no strategic complexity to
    // humanize — skip the artificial delay so the decision resolves as soon as the
    // engine returns (computation is near-instant after our optimizations).
    const isMulligan =
      waitingForType === "MulliganDecision" ||
      waitingForType === "OpeningHandBottomCards";
    // Collapse the humanization delay under stack pressure. The depth-based skip
    // gate (checkAndSchedule) only fires at Elevated depth, which a 0↔1 trigger
    // loop never reaches — so without this the AI pays a full 500–900ms beat on
    // every oscillation cycle. Rate-driven pressure shrinks it (Rapid → ~75ms).
    const stackLen = gameState?.stack?.length ?? 0;
    const baseDelay = isMulligan ? 0 : AI_BASE_DELAY_MS + Math.random() * AI_DELAY_VARIANCE_MS;
    const delay = Math.round(baseDelay * pressureMultiplier(effectiveStackPressure(stackLen)));
    timeoutId = setTimeout(async () => {
      timeoutId = null;
      if (!active) {
        pending = false;
        return;
      }
      let failed = false;
      try {
        let action: GameAction | null;
        try {
          action = await actionPromise;
        } catch (err) {
          // Engine panic: re-running the same AI search against the same
          // (deterministic) state will re-panic. This is the path the
          // user-reported "ai-getAction-retry" came from — short-circuit
          // with the captured panic so the modal can show the real cause.
          if (isEnginePanic(err)) {
            await routePanic("ai-getAction-panic", err.panic);
            throw err;
          }
          if (!isStateLost(err)) throw err;
          // Engine lost state between scheduleAction and the timeout firing.
          // Try to rehydrate from the store snapshot and recompute the AI
          // action once. If recovery fails (or the retry still throws because
          // restoreState silently failed in the worker), escalate to the
          // user-prompt path.
          debugLog("AI getAiAction hit STATE_LOST; attempting rehydrate", "warn");
          const recovered = await attemptStateRehydrate();
          if (!recovered) {
            notifyEngineLost("ai-getAction");
            throw err;
          }
          try {
            action = await adapter!.getAiAction(difficulty, playerId, waitingForType);
          } catch (retryErr) {
            if (isEnginePanic(retryErr)) {
              await routePanic("ai-getAction-retry-panic", retryErr.panic);
            } else {
              notifyEngineLost("ai-getAction-retry");
            }
            throw retryErr;
          }
        }
        // Re-check active after await — the AI computation may have completed
        // after stop() was called, and dispatching a stale action from the old
        // game into a new game session would corrupt state.
        if (!active) return;
        const currentGameState = useGameStore.getState().gameState;
        const currentWaitingFor = currentGameState?.waiting_for ?? null;
        if (waitingForFingerprint(currentWaitingFor) !== scheduledWaitingForFingerprint) {
          debugLog(
            `AI ignored stale ${action?.type ?? "action"} for player ${playerId + 1}: waitingFor changed from ${waitingForDebugLabel(scheduledWaitingFor)} to ${waitingForDebugLabel(currentWaitingFor)}`,
            "info",
          );
          return;
        }
        if (action == null) {
          debugLog(
            `AI getAiAction returned null for player ${playerId} (waitingFor: ${currentWaitingFor?.type ?? "none"})`,
            "warn",
          );
          failed = true;
          return;
        }
        const guess = describeAiCardPredicateGuess(action, currentWaitingFor, currentGameState);
        if (guess != null) {
          debugLog(`AI player ${playerId + 1} randomly ${guess}`, "info");
        }
        // Pass `playerId` (the AI seat we're driving) as actor. The engine
        // guard in `apply` verifies actor matches the authorized submitter;
        // dispatching as the human here would be rejected.
        // dispatch.ts has its own STATE_LOST recovery; any error that reaches
        // here after that retry is genuinely unrecoverable for this attempt.
        await dispatchAction(action, playerId);
        // Successful dispatch — reset both failure counters
        consecutiveFailures = 0;
        totalFailures = 0;
      } catch (e) {
        debugLog(`AI error choosing action: ${e instanceof Error ? e.message : String(e)}`);
        failed = true;
      } finally {
        if (failed) {
          consecutiveFailures++;
          totalFailures++;
        }
        pending = false;
        if (active) checkAndSchedule();
      }
    }, delay);
  }

  function start() {
    active = true;
    debugLog(`AI controller started (configured seats: [${[...aiPlayerIds].join(",")}], dynamic for all non-human)`, "warn");
    // Event-driven design: subscribe to WaitingFor changes and let each
    // seat's turn naturally surface via the store. This means reconnect
    // is implicit — whichever seat holds priority after a reconnect
    // triggers `checkAndSchedule`, regardless of how many AI seats the
    // controller supervises. No per-seat iteration needed; the bug that
    // previously stalled P3/P4 was caused by `getAiAction` accepting a
    // default `playerId` elsewhere, not by this loop.
    unsubscribe = useGameStore.subscribe(
      (s) => s.waitingFor,
      () => {
        if (active) checkAndSchedule();
      },
    );
    checkAndSchedule();
  }

  function stop() {
    active = false;
    if (timeoutId != null) {
      clearTimeout(timeoutId);
      timeoutId = null;
    }
    pending = false;
  }

  function dispose() {
    stop();
    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
  }

  return { start, stop, dispose };
}
