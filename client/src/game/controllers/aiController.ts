import { AI_BASE_DELAY_MS, AI_DELAY_VARIANCE_MS, PLAYER_ID } from "../../constants/game";
import { useGameStore } from "../../stores/gameStore";
import { profileForSeat, useLlmStore } from "../../stores/llmStore";
import { executeLlmRequest } from "../../services/llm/llmClient";
import { reportLlmFailure } from "../../services/llm/diagnostics";
import { endpointOf } from "../../services/llm/types";
import type { AiActionProposal, GameAction, GameState, WaitingFor } from "../../adapter/types";
import { AdapterError, AdapterErrorCode } from "../../adapter/types";
import { pressureMultiplier } from "../../utils/stackPressure";
import { effectiveStackPressure } from "../../utils/stackThroughput";
import {
  clearAiDecisionDiagnostic,
  recordAiDecisionDiagnostic,
} from "../aiDecisionDiagnostics";
import { debugLog } from "../debugLog";
import { dispatchAiActionProposal } from "../dispatch";
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
  /**
   * Index of this seat in `preferencesStore.aiSeats` / `llmStore.seatBindings`
   * (0 = first AI opponent). Present so the controller can look up an LLM
   * binding at DECISION time rather than copying one in: the player may edit or
   * delete a profile mid-game, and a stale copy would keep calling a key they
   * revoked.
   *
   * Absent for callers that do not configure per-seat opponents; such a seat is
   * always heuristic.
   */
  llmSeatIndex?: number;
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
  _gameState: GameState | null | undefined,
): string | null {
  if (action.type !== "ChooseOption" || waitingFor?.type !== "NamedChoice") return null;
  if (choiceTypeKey(waitingFor.data.choice_type) !== "CardPredicateGuess") return null;

  const sourceName = waitingFor.data.source?.prompt.display_name ?? null;
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

/**
 * How many engine-authored log entries an LLM decision is given as history.
 *
 * The engine decides how many of these it actually renders (by difficulty, via
 * `phase_llm::prompt::history_window`); this is only the bound on what crosses
 * the boundary, so a thousand-entry Commander game does not serialize its whole
 * log on every priority pass.
 */
const LLM_HISTORY_TRANSFER_LIMIT = 120;

/**
 * Consecutive LLM failures a seat may take before the controller stops trying
 * the provider for the rest of the session.
 *
 * Without this, a provider that is down, rate-limited, or simply hanging costs
 * the full request timeout on EVERY decision, turning one misconfiguration into
 * a permanently unplayable game. The seat keeps playing throughout — it just
 * plays with the engine AI, which is what it was already falling back to.
 */
const MAX_CONSECUTIVE_LLM_FAILURES = 3;

/**
 * Run one LLM-driven decision for `playerId`.
 *
 * Returns `null` for EVERY failure — no configured profile, an adapter without
 * the capability, a network or provider error, a reply the engine would not
 * bind to a legal option, a decision that moved on mid-flight. The caller then
 * takes the ordinary heuristic path, so an LLM seat degrades to a normal AI
 * seat rather than stalling the game.
 *
 * The engine owns everything of consequence here: it builds the prompt, it
 * builds the HTTP request, and it is the only thing that turns a reply back
 * into an action. This function performs the call and passes bytes along.
 */
async function llmActionProposal(
  playerId: number,
  difficulty: string,
  llmSeatIndex: number | undefined,
  signal: AbortSignal,
): Promise<AiActionProposal | null> {
  if (llmSeatIndex == null) return null;
  const profile = profileForSeat(useLlmStore.getState(), llmSeatIndex);
  if (!profile) return null;

  const { adapter, logHistory } = useGameStore.getState();
  if (!adapter?.buildLlmDecisionRequest || !adapter.getAiActionProposalFromLlmResponse) {
    return null;
  }

  const history = (logHistory ?? []).slice(-LLM_HISTORY_TRANSFER_LIMIT);
  const built = await adapter.buildLlmDecisionRequest(
    difficulty,
    playerId,
    JSON.stringify(endpointOf(profile)),
    JSON.stringify(history),
  );
  if (!built?.request || !built.fingerprint) {
    // The engine's refusal text can carry a PROVIDER-authored diagnostic, and
    // the game log is prompt-renderable. Only the Phase-authored summary is
    // logged; the detail goes to the console.
    reportLlmFailure(`LLM opponent (seat ${llmSeatIndex}) could not build a request`, built?.error);
    return null;
  }

  const { status, body } = await executeLlmRequest(built.request, { signal });
  // Status travels with the body so the engine can refuse a non-2xx reply
  // however it parses — an error page or gateway failure must never be bound to
  // a game action.
  const resolved = await adapter.getAiActionProposalFromLlmResponse(
    playerId,
    built.fingerprint,
    profile.provider,
    status,
    body,
  );
  if (!resolved?.proposal) {
    reportLlmFailure(`LLM opponent (seat ${llmSeatIndex}) reply was refused`, resolved?.error);
    return null;
  }
  // The model's reasoning is deliberately NOT logged. `debugLog` writes a
  // `visibility: "Public"` entry into `logHistory` -- the shared game log, which
  // is also the history this engine feeds back into later prompts. Publishing a
  // seat's private deliberation there would leak it to every player and echo it
  // into subsequent decisions. It stays on `resolved.reasoning` for a future
  // private channel (the local-only AI decision receipt is the established one).
  return resolved.proposal;
}

export function createAIController(config: AIControllerConfig): AIController {
  let active = false;
  let pending = false;
  let timeoutId: ReturnType<typeof setTimeout> | null = null;
  let unsubscribe: (() => void) | null = null;
  let attemptGeneration = 0;

  interface AIAttempt {
    generation: number;
    gameSessionGeneration: number;
    waitingForFingerprint: string;
    playerId: number;
  }

  // Failure tracking on the same WaitingFor state to break infinite loops.
  // `MAX_CONSECUTIVE_FAILURES` gates the normal→fallback transition; the
  // separate `MAX_TOTAL_FAILURES` hard-stops the controller so post-fallback
  // failures (e.g., engine rejecting even the safe fallback) cannot spin.
  let lastWaitingForKey: string | null = null;
  let consecutiveFailures = 0;
  let totalFailures = 0;
  let lastDispatchError: string | null = null;
  const MAX_CONSECUTIVE_FAILURES = 3;

  const difficultyByPlayerId = new Map(config.seats.map((s) => [s.playerId, s.difficulty]));
  // Only the INDEX is retained, never the profile: the lookup happens at
  // decision time so an edited or deleted profile takes effect immediately.
  const llmSeatIndexByPlayerId = new Map(
    config.seats.map((s) => [s.playerId, s.llmSeatIndex]),
  );
  const aiPlayerIds = new Set(difficultyByPlayerId.keys());
  /** Aborts an in-flight LLM call whose decision is no longer current. */
  let llmAbort: AbortController | null = null;
  /** Consecutive LLM failures per seat, reset by the first success. */
  const llmFailures = new Map<number, number>();
  /** Seats whose provider has failed enough to be given up on this session. */
  const llmDisabled = new Set<number>();

  function recordLlmOutcome(playerId: number, succeeded: boolean): void {
    if (succeeded) {
      llmFailures.delete(playerId);
      return;
    }
    const failures = (llmFailures.get(playerId) ?? 0) + 1;
    llmFailures.set(playerId, failures);
    if (failures >= MAX_CONSECUTIVE_LLM_FAILURES) {
      llmDisabled.add(playerId);
      debugLog(
        `LLM opponent (player ${playerId}) failed ${failures} times in a row; `
          + "this seat will use the engine AI for the rest of the game",
        "warn",
      );
    }
  }

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

  function authorizedAiPlayer(
    waitingFor: WaitingFor,
    state: GameState,
  ): number | null {
    const mulliganPid = aiPendingForMulligan(
      waitingFor as { type: string; data?: { pending?: { player: number }[] } },
    );
    if (mulliganPid !== null) return mulliganPid;
    if (
      waitingFor.type === "MulliganDecision" ||
      waitingFor.type === "OpeningHandBottomCards"
    ) {
      return null;
    }
    if (waitingFor.type === "ResolveAllConsent") {
      const { representative } = waitingFor.data;
      return aiPlayerIds.has(representative) ? representative : null;
    }
    if (
      !("data" in waitingFor) ||
      !waitingFor.data ||
      (!("player" in waitingFor.data) &&
        waitingFor.type !== "LoopShortcut" &&
        waitingFor.type !== "PrecastCopyShortcutOffer")
    ) {
      return null;
    }
    return state.priority_player === PLAYER_ID ? null : state.priority_player;
  }

  function beginAttempt(waitingFor: WaitingFor, playerId: number): AIAttempt {
    const store = useGameStore.getState();
    const attempt: AIAttempt = {
      generation: ++attemptGeneration,
      gameSessionGeneration: store.gameSessionGeneration,
      waitingForFingerprint: waitingForFingerprint(waitingFor),
      playerId,
    };
    pending = true;
    return attempt;
  }

  function isAttemptCurrent(attempt: AIAttempt): boolean {
    if (!active || attempt.generation !== attemptGeneration) return false;
    const store = useGameStore.getState();
    if (store.gameSessionGeneration !== attempt.gameSessionGeneration) return false;
    const state = store.gameState;
    const waitingFor = state?.waiting_for ?? null;
    if (!state || !waitingFor) return false;
    if (waitingForFingerprint(waitingFor) !== attempt.waitingForFingerprint) return false;
    if (authorizedAiPlayer(waitingFor, state) !== attempt.playerId) return false;
    return true;
  }

  function finishAttempt(attempt: AIAttempt): boolean {
    if (attempt.generation !== attemptGeneration) return false;
    pending = false;
    return true;
  }

  function invalidateAttempt(): void {
    attemptGeneration++;
    pending = false;
    clearAiDecisionDiagnostic();
    // A decision that moved on must not keep a provider call alive: the engine
    // would refuse the stale reply anyway, and the socket is worth reclaiming.
    llmAbort?.abort();
    llmAbort = null;
    if (timeoutId != null) {
      clearTimeout(timeoutId);
      timeoutId = null;
    }
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
    // For all other states, the engine-authored `priority_player` is the
    // authorized submitter, including controlled turns (CR 723.5).
    const mulliganPid = aiPendingForMulligan(
      waitingFor as { type: string; data?: { pending?: { player: number }[] } },
    );
    const waitingPlayerId = authorizedAiPlayer(waitingFor, state);
    if (waitingPlayerId === null) return;

    // Reset failure counters when the WaitingFor state changes (type or player).
    // `consecutiveFailures` gates normal→fallback escalation; `totalFailures`
    // is the absolute hard stop that kills the controller.
    const key = waitingForKey(waitingFor, mulliganPid);
    if (key !== lastWaitingForKey) {
      lastWaitingForKey = key;
      consecutiveFailures = 0;
      totalFailures = 0;
      lastDispatchError = null;
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
      notifyEngineLost(`ai-controller-stuck:${waitingFor.type}`);
      stop();
      return;
    }

    const useTacticalFallback = consecutiveFailures >= MAX_CONSECUTIVE_FAILURES;
    if (useTacticalFallback && !useGameStore.getState().adapter?.getAiTacticalActionProposal) {
      debugLog(
        `AI controller halted after ${MAX_CONSECUTIVE_FAILURES} failed proposals on ${waitingFor.type}; no tactical fallback is available`,
        "error",
      );
      notifyEngineLost(`ai-controller-stuck:${waitingFor.type}`);
      stop();
      return;
    }

    scheduleAction(waitingPlayerId, useTacticalFallback);
  }

  function scheduleAction(playerId: number, useTacticalFallback: boolean) {
    if (pending) return;

    // Start computing immediately — in parallel with the artificial delay.
    // This turns additive latency (delay + compute) into max(delay, compute),
    // which matters most for deeper engine-owned searches.
    const { adapter, gameState } = useGameStore.getState();
    // Each seat has its own difficulty — a controller driving three AI players
    // can simultaneously run Easy, Medium, and VeryHard policies.
    const difficulty = difficultyByPlayerId.get(playerId) ?? "Medium";
    const waitingForType = gameState?.waiting_for?.type;
    const scheduledWaitingFor = gameState?.waiting_for ?? null;
    if (!scheduledWaitingFor) return;
    const getProposal = useTacticalFallback
      ? adapter?.getAiTacticalActionProposal
      : adapter?.getAiActionProposal;
    if (!getProposal) return;
    const attempt = beginAttempt(scheduledWaitingFor, playerId);
    const waitingFor = waitingForDebugLabel(scheduledWaitingFor);
    recordAiDecisionDiagnostic({
      stage: "awaiting-proposal",
      playerId,
      difficulty,
      waitingFor,
    });
    // Defer invocation into the promise chain. `Promise.resolve(call())`
    // evaluates `call()` first, so a synchronous adapter exception used to
    // bypass the timeout callback's catch/finally and strand `pending = true`.
    const heuristicProposal = (): Promise<AiActionProposal | null> =>
      Promise.resolve().then(() => getProposal.call(adapter, difficulty, playerId));
    // An LLM seat is tried first and falls back to the heuristic AI on any
    // failure. The tactical-fallback path deliberately skips the LLM entirely:
    // it exists to recover a seat whose proposals keep failing, and adding a
    // network round trip to a recovery path is the wrong trade.
    const llmSeatIndex = llmSeatIndexByPlayerId.get(playerId);
    let proposalPromise: Promise<AiActionProposal | null>;
    if (useTacticalFallback || llmSeatIndex == null || llmDisabled.has(playerId)) {
      proposalPromise = heuristicProposal();
    } else {
      llmAbort?.abort();
      const abort = new AbortController();
      llmAbort = abort;
      proposalPromise = llmActionProposal(playerId, difficulty, llmSeatIndex, abort.signal)
        .catch((error) => {
          reportLlmFailure(
            `LLM opponent (player ${playerId}) failed; using the engine AI`,
            error,
          );
          return null;
        })
        .then((proposal) => {
          // A cancelled call is not the provider's fault — the decision simply
          // moved on — so it must not count toward giving up on the seat.
          if (!abort.signal.aborted) recordLlmOutcome(playerId, proposal != null);
          return proposal ?? heuristicProposal();
        });
    }
    // Suppress unhandled-rejection warnings if stop() cancels the timeout
    // before it fires and nothing else awaits this promise.
    proposalPromise.catch(() => {});

    // Mulligan is a binary keep/mulligan decision with no strategic complexity to
    // humanize — skip the artificial delay so the decision resolves as soon as the
    // engine returns (computation is near-instant after our optimizations).
    const isMulligan =
      waitingForType === "MulliganDecision" ||
      waitingForType === "OpeningHandBottomCards";
    // Stack pressure scales only the artificial humanization delay; it never
    // owns or skips the AI decision. Rate-driven pressure keeps low-depth,
    // high-churn loops from paying a full 500–900ms beat on every cycle
    // (Rapid → ~75ms).
    const stackLen = gameState?.stack?.length ?? 0;
    const baseDelay = isMulligan ? 0 : AI_BASE_DELAY_MS + Math.random() * AI_DELAY_VARIANCE_MS;
    const delay = Math.round(baseDelay * pressureMultiplier(effectiveStackPressure(stackLen)));
    timeoutId = setTimeout(async () => {
      timeoutId = null;
      let failed = false;
      try {
        let proposal: AiActionProposal | null;
        try {
          proposal = await proposalPromise;
        } catch (err) {
          if (!isAttemptCurrent(attempt)) return;
          // Engine panic: re-running the same AI search against the same
          // (deterministic) state will re-panic. This is the path the
            // user-reported AI retry came from — short-circuit
          // with the captured panic so the modal can show the real cause.
          if (isEnginePanic(err)) {
            await routePanic("ai-getAction-panic", err.panic);
            if (!isAttemptCurrent(attempt)) return;
            throw err;
          }
          if (!isStateLost(err)) throw err;
          // Engine lost state between scheduleAction and the timeout firing.
          // Try to rehydrate from the store snapshot and recompute the AI
          // action once. If recovery fails (or the retry still throws because
          // restoreState silently failed in the worker), escalate to the
          // user-prompt path.
          debugLog("AI proposal lookup hit STATE_LOST; attempting rehydrate", "warn");
          if (!isAttemptCurrent(attempt)) return;
          const recovered = await attemptStateRehydrate();
          if (!isAttemptCurrent(attempt)) return;
          if (!recovered) {
            notifyEngineLost("ai-getAction");
            throw err;
          }
          try {
            if (!isAttemptCurrent(attempt)) return;
            const retryAdapter = useGameStore.getState().adapter;
            const retryGetProposal = useTacticalFallback
              ? retryAdapter?.getAiTacticalActionProposal
              : retryAdapter?.getAiActionProposal;
            if (!retryGetProposal) return;
            proposal = await retryGetProposal.call(retryAdapter, difficulty, playerId);
          } catch (retryErr) {
            if (!isAttemptCurrent(attempt)) return;
            if (isEnginePanic(retryErr)) {
              await routePanic("ai-getAction-retry-panic", retryErr.panic);
              if (!isAttemptCurrent(attempt)) return;
            } else {
              notifyEngineLost("ai-getAction-retry");
            }
            throw retryErr;
          }
        }
        // Re-check the complete attempt identity after every await. A matching
        // WaitingFor payload in a new game/session is still stale.
        if (!isAttemptCurrent(attempt)) {
          const currentWaitingFor = useGameStore.getState().gameState?.waiting_for ?? null;
          debugLog(
            `AI ignored stale ${proposal?.action.type ?? "proposal"} for player ${playerId + 1}: waitingFor changed from ${waitingForDebugLabel(scheduledWaitingFor)} to ${waitingForDebugLabel(currentWaitingFor)}`,
            "info",
          );
          return;
        }
        const currentGameState = useGameStore.getState().gameState;
        const currentWaitingFor = currentGameState?.waiting_for ?? null;
        if (proposal == null) {
          debugLog(
            `AI returned no engine-bounded proposal for player ${playerId} (waitingFor: ${currentWaitingFor?.type ?? "none"})`,
            "warn",
          );
          failed = true;
          return;
        }
        const guess = describeAiCardPredicateGuess(proposal.action, currentWaitingFor, currentGameState);
        if (guess != null) {
          debugLog(`AI player ${playerId + 1} randomly ${guess}`, "info");
        }
        // The proposal carries the engine-derived authorized actor. The UI
        // never substitutes `playerId` or reconstructs an action from the
        // prompt, which keeps controlled turns and simultaneous decisions in
        // the authority boundary.
        if (!isAttemptCurrent(attempt)) return;
        recordAiDecisionDiagnostic({
          stage: "submitting-proposal",
          playerId,
          difficulty,
          waitingFor,
        });
        const submission = await dispatchAiActionProposal(proposal);
        if (!isAttemptCurrent(attempt)) return;
        // The proposal boundary returns a tagged stale result without mutating
        // the store. That is a normal race, not a failed AI decision: leave
        // the counters untouched and let the final scheduler re-query the
        // engine's newly issued finite domain.
        if (submission.status === "stale") {
          debugLog(`AI proposal was stale for player ${playerId + 1}; re-querying`, "info");
          return;
        }
      } catch (e) {
        if (!isAttemptCurrent(attempt)) return;
        lastDispatchError = e instanceof Error ? e.message : String(e);
        recordAiDecisionDiagnostic({
          stage: "failed",
          playerId,
          difficulty,
          waitingFor,
          error: lastDispatchError,
        });
        debugLog(`AI error choosing action: ${lastDispatchError}`);
        failed = true;
      } finally {
        if (finishAttempt(attempt)) {
          if (failed) {
            consecutiveFailures++;
            totalFailures++;
          }
          if (active) {
            checkAndSchedule();
            if (!pending) clearAiDecisionDiagnostic();
          }
        }
      }
    }, delay);
  }

  function start() {
    active = true;
    clearAiDecisionDiagnostic();
    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
    debugLog(`AI controller started (configured seats: [${[...aiPlayerIds].join(",")}], dynamic for all non-human)`, "warn");
    // Event-driven design: subscribe to WaitingFor changes and let each
    // seat's turn naturally surface via the store. This means reconnect
    // is implicit — whichever seat holds priority after a reconnect
    // triggers `checkAndSchedule`, regardless of how many AI seats the
    // controller supervises. No per-seat iteration needed; the bug that
    // previously stalled P3/P4 was caused by an action-only API accepting a
    // default `playerId` elsewhere, not by this loop.
    let observedWaitingFor = useGameStore.getState().waitingFor;
    let observedSessionGeneration = useGameStore.getState().gameSessionGeneration;
    unsubscribe = useGameStore.subscribe(
      (s) => s,
      () => {
        if (!active) return;
        const store = useGameStore.getState();
        const waitingForChanged = store.waitingFor !== observedWaitingFor;
        const sessionChanged = store.gameSessionGeneration !== observedSessionGeneration;
        observedWaitingFor = store.waitingFor;
        observedSessionGeneration = store.gameSessionGeneration;

        if (waitingForChanged || sessionChanged) {
          invalidateAttempt();
          // A new snapshot gets a fresh failure budget even for an A→A
          // transition whose serialized WaitingFor payload is identical.
          lastWaitingForKey = null;
          checkAndSchedule();
        }
      },
    );
    checkAndSchedule();
  }

  function stop() {
    active = false;
    invalidateAttempt();
    clearAiDecisionDiagnostic();
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
