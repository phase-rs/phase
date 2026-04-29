/**
 * Transparent engine-state recovery for the STATE_LOST failure mode.
 *
 * Background: the WASM engine keeps game state in a thread-local
 * `RefCell<Option<GameState>>`. When that cell becomes `None` mid-session
 * (worker restart, PWA update activation race, panic recovery, or an
 * explicit `clear_game_state()` without a corresponding store reset), every
 * subsequent engine call fails with the Rust sentinel `NOT_INITIALIZED: ...`.
 * The React store still holds a valid `gameState` snapshot from before the
 * loss — this module uses that snapshot to transparently repopulate the
 * engine so the user never sees the error.
 *
 * Mode matrix:
 * - `ai` / `local` → `adapter.restoreState(storeState)`. Supported here.
 * - `p2p-host`     → Host recovery requires the full resume-from-persisted
 *                    session path (guest seat reservations, WebRTC grace
 *                    windows, multiplayer-flag flip via
 *                    `resumeMultiplayerHostState`). The P2PHostAdapter
 *                    doesn't expose those on the EngineAdapter interface,
 *                    and a mid-recovery hiccup could desync seats. We
 *                    punt to the Layer 3 reload path, which triggers the
 *                    host's existing `initialize()` resume flow from IDB.
 * - `p2p-join`     → Guests do not hold authoritative WASM state; their
 *                    failure mode is host-disconnect, handled by the P2P
 *                    adapter's auto-reconnect loop. Skipped here.
 * - `online` (WS)  → Server is authoritative; local rehydrate is impossible.
 *                    The WS adapter has its own reconnection logic.
 *
 * Recovery is best-effort. `false` means the caller must escalate to the
 * user-facing Layer 3 reload prompt via `notifyEngineLost`.
 */
import { debugLog } from "./debugLog";
import { useGameStore } from "../stores/gameStore";
import { loadCheckpoints } from "../services/gamePersistence";
import { AdapterError, AdapterErrorCode, type GameState } from "../adapter/types";

/**
 * Attempt to repopulate the engine's thread-local state from the last-known
 * store snapshot (or, if the store is also empty, the most recent IDB
 * checkpoint). Returns `true` when recovery succeeded and the caller may
 * safely retry its original engine call; `false` when recovery failed or
 * the current mode is not locally recoverable.
 */
export async function attemptStateRehydrate(): Promise<boolean> {
  const { adapter, gameState, gameMode, gameId } = useGameStore.getState();

  if (!adapter) {
    debugLog("engine-recovery: no adapter", "warn");
    return false;
  }

  // Only AI/local games rehydrate locally. Other modes either can't
  // (guest/WS) or need a full resume flow (p2p-host). See module header.
  if (gameMode !== "ai" && gameMode !== "local") {
    debugLog(`engine-recovery: mode=${gameMode} not locally recoverable`, "warn");
    return false;
  }

  // Prefer the live store snapshot. Fall back to IDB only if the store
  // has also been cleared (rare — only happens if something has nuked
  // the in-memory state without a full reload).
  let snapshot: GameState | null = gameState;
  let usedIdbFallback = false;
  if (!snapshot && gameId) {
    try {
      const checkpoints = await loadCheckpoints(gameId);
      if (checkpoints.length > 0) {
        snapshot = checkpoints[checkpoints.length - 1];
        usedIdbFallback = true;
      }
    } catch (err) {
      debugLog(`engine-recovery: IDB checkpoint load failed: ${err}`, "warn");
    }
  }

  if (!snapshot) {
    debugLog(
      `engine-recovery: no usable state (store=${!!gameState} idb=${usedIdbFallback})`,
      "warn",
    );
    return false;
  }

  try {
    // `restoreState` on WasmAdapter pushes state JSON into the worker's
    // thread-local via `restore_game_state`. Safe when the thread-local is
    // None (our case — we got here because it WAS None). The engine refuses
    // restore when `MULTIPLAYER_MODE` is set, but we've already short-
    // circuited non-ai/non-local modes above.
    await adapter.restoreState(snapshot);
    debugLog(
      `engine-recovery: rehydrated from ${usedIdbFallback ? "IDB" : "store"}`,
      "warn",
    );
    return true;
  } catch (err) {
    debugLog(
      `engine-recovery: restoreState threw: ${err instanceof Error ? err.message : String(err)}`,
      "error",
    );
    return false;
  }
}

/**
 * Subscribe to engine-loss events surfaced to the user. Consumers (e.g. the
 * root React app) render a modal when the listener fires; the user's
 * response triggers a hard reload, which `GameProvider` handles at startup
 * by resuming from IDB (or from the persisted P2P host session for hosts).
 *
 * `panic` is the captured Rust panic message (file:line + payload) when the
 * loss was caused by an engine crash — present for ENGINE_PANIC, absent for
 * transient STATE_LOST. The modal switches between "real crash + report"
 * and "transient loss + reload" based on this field.
 */
export interface EngineLostEvent {
  reason: string;
  panic?: string;
}

type EngineLostListener = (event: EngineLostEvent) => void;
const engineLostListeners = new Set<EngineLostListener>();
const nonFatalPanicListeners = new Set<EngineLostListener>();

export function onEngineLost(listener: EngineLostListener): () => void {
  engineLostListeners.add(listener);
  return () => engineLostListeners.delete(listener);
}

/**
 * Subscribe to non-fatal engine panics — a Rust panic happened but the
 * engine kept working (either the panic was in a side path like trace
 * instrumentation, or a rehydrate from the store snapshot succeeded).
 * The user sees a dismissible toast, not the blocking "Engine crashed"
 * modal. Report-link + diagnostic still available so a triager can act
 * on the panic if it was load-bearing.
 */
export function onNonFatalPanic(listener: EngineLostListener): () => void {
  nonFatalPanicListeners.add(listener);
  return () => nonFatalPanicListeners.delete(listener);
}

/**
 * Escalate to the Layer 3 user-prompt path. Called when
 * `attemptStateRehydrate` returns false during a dispatch or AI action — or
 * when the AI controller hits its hard-failure cap. A single reload carries
 * the user back to a clean state: `GameProvider` resumes from IDB on mount.
 *
 * De-duped: the handler in `EngineLostModal` only shows the modal once per
 * tab session. Repeated calls within the same session are no-ops.
 */
export function notifyEngineLost(reason: string, panic?: string): void {
  for (const fn of engineLostListeners) fn({ reason, panic });
}

/**
 * Route a captured panic through a rehydrate-first triage: if the engine's
 * state survived (or can be rebuilt from the store snapshot), emit a
 * non-fatal notification so the user keeps playing. Only on rehydrate
 * failure does the blocking "Engine crashed" modal fire.
 *
 * Covers the common case where the panic happened in a side path
 * (instrumentation, a debug assertion in a helper) and the engine's
 * thread-local game state is still intact — the user saw the scary modal
 * even though nothing was actually broken. This keeps the modal reserved
 * for genuinely unrecoverable crashes.
 */
export async function routePanic(reason: string, panic?: string): Promise<void> {
  const rehydrated = await attemptStateRehydrate().catch(() => false);
  if (rehydrated) {
    for (const fn of nonFatalPanicListeners) fn({ reason, panic });
    return;
  }
  notifyEngineLost(reason, panic);
}

/**
 * True when the error is a Rust panic that was captured by the worker's
 * panic hook. Caller MUST treat this as terminal — retrying the same
 * input will re-panic. Defined here so `dispatch.ts` and `aiController.ts`
 * share one classifier instead of duplicating it.
 *
 * Tightened to `instanceof AdapterError` so a non-adapter object that
 * happens to share the field shape (deserialized wire payload, mocked
 * error in a test, etc.) doesn't trigger panic-flow short-circuits.
 */
export function isEnginePanic(err: unknown): err is AdapterError & { panic: string } {
  return (
    err instanceof AdapterError
    && err.code === AdapterErrorCode.ENGINE_PANIC
    && typeof err.panic === "string"
  );
}
