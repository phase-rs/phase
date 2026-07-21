/**
 * Tier 1 telemetry event wiring (docs/telemetry-proposal.md §4 Tier 1).
 *
 * `installTelemetry()` is called once from `main.tsx` alongside the other
 * `install*` bootstraps. It registers the global JS-error listeners, the
 * page-lifecycle flush hooks, and the game-store subscriptions that emit
 * `stuck_decision` and `game_end`. It deliberately imports NO React — it runs
 * outside the component tree.
 *
 * The other two Tier 1 events fire from their own subsystems, which call
 * `trackEvent` directly: `engine_panic` from `game/engineRecovery.ts`
 * (`routePanic`) and `chunk_reload` from `pwa/chunkReloadHandler.ts`.
 */
import { STUCK_DEBOUNCE_MS } from "../constants/stuckDecision";
import { useGameStore } from "../stores/gameStore";
import { flushNow, installTelemetryLifecycle, trackEvent } from "./telemetry";

/** Coarse route: the first path segment, so we bucket by area (deck / game /
 *  draft / menu) without capturing ids or query strings. The single route
 *  vocabulary shared by `js_error.route`, `session_start`, and `route_view` —
 *  every route-keyed dashboard query joins on this format. Defaults to the
 *  current location; callers with a specific pathname (e.g. the route-change
 *  component) pass it explicitly. */
export function coarseRoute(pathname?: string): string {
  if (pathname === undefined && typeof window === "undefined") return "";
  const path = pathname ?? window.location.pathname;
  const segment = path.split("/").filter(Boolean)[0];
  return segment ? `/${segment}` : "/";
}

/** Extract the reportable shape of a thrown value. `top_frame` is the first
 *  stack line (the throw site); the full stack is intentionally NOT sent. */
function errorFields(
  err: unknown,
  message: string | undefined,
  source: "boundary" | "window" | "unhandledrejection",
): Record<string, unknown> {
  const error = err instanceof Error ? err : undefined;
  const stack = error?.stack ?? "";
  const topFrame = stack.split("\n").find((line) => line.trim().startsWith("at"))?.trim();
  return {
    name: error?.name ?? "Error",
    message: error?.message ?? message ?? String(err),
    top_frame: topFrame,
    source,
    route: coarseRoute(),
  };
}

/** Report a caught render error from the React ErrorBoundary (Part E). Exposed
 *  so the boundary — which does import React — routes through the same shape. */
export function reportBoundaryError(err: unknown): void {
  trackEvent("js_error", errorFields(err, undefined, "boundary"));
}

function installErrorListeners(): void {
  window.addEventListener("error", (event) => {
    trackEvent("js_error", errorFields(event.error, event.message, "window"));
  });
  window.addEventListener("unhandledrejection", (event) => {
    trackEvent("js_error", errorFields(event.reason, undefined, "unhandledrejection"));
  });
}

/**
 * Emit `stuck_decision` once per continuous stuck episode, using the same
 * persistence debounce as the user-facing toast (independent of the toast's
 * session-suppression latch). A transient wedge that clears within the window
 * emits nothing.
 */
function installStuckDecisionTracking(): void {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let emittedForEpisode = false;

  useGameStore.subscribe(
    (s) => s.stuckDiagnostic,
    (diagnostic) => {
      if (!diagnostic) {
        // Episode ended — reset so the next genuine wedge can emit.
        if (timer !== null) {
          clearTimeout(timer);
          timer = null;
        }
        emittedForEpisode = false;
        return;
      }
      if (emittedForEpisode || timer !== null) return;
      timer = setTimeout(() => {
        timer = null;
        const current = useGameStore.getState();
        if (!current.stuckDiagnostic) return;
        emittedForEpisode = true;
        trackEvent("stuck_decision", {
          waiting_for_kind: current.stuckDiagnostic.waitingForKind,
          game_mode: current.gameMode,
          phase: current.gameState?.phase,
        });
      }, STUCK_DEBOUNCE_MS);
    },
  );
}

/**
 * Emit `game_end` once per game when the state's `waiting_for` first becomes
 * `GameOver`. Deduped by `gameId` (multiplayer emits one per participating
 * client — counts are per-client sessions, not per game). `spectate` mode is
 * skipped entirely. Every payload value is an engine-provided field forwarded
 * verbatim; `winner_kind` uses the client-owned `aiSeatIds` binding.
 */
function installGameEndTracking(): void {
  let lastEmittedGameId: string | null = null;

  useGameStore.subscribe(
    (s) => s.waitingFor,
    (waitingFor) => {
      if (!waitingFor || waitingFor.type !== "GameOver") return;
      const { gameId, gameMode, gameState, aiSeatIds } = useGameStore.getState();
      if (gameMode === "spectate") return;
      if (gameId === null || gameId === lastEmittedGameId) return;
      lastEmittedGameId = gameId;

      const winner = waitingFor.data.winner;
      // Online games can have server-hosted AI seats the client can't identify
      // (guests never see them), so a winner's human/AI nature is unknown here —
      // emit null rather than mislabel a server AI winner as "human" (AE data is
      // unrecoverable). `result` still distinguishes draws and `game_mode`
      // segments the queries.
      const winnerKind =
        winner === null || gameMode === "online"
          ? null
          : aiSeatIds.includes(winner)
            ? "ai"
            : "human";
      trackEvent("game_end", {
        turn_count: gameState?.turn_number,
        result: winner === null ? "draw" : "winner",
        winner_kind: winnerKind,
        game_mode: gameMode,
        unimplemented_oracle_ids: (gameState?.unimplemented_oracle_ids ?? []).slice(0, 20),
        pending_trigger_abandons: (gameState?.pending_trigger_abandons ?? []).slice(0, 20),
      });
      // Flush promptly so a game_end isn't stranded if the user leaves the
      // results screen before the batch timer fires.
      flushNow();
    },
  );
}

/**
 * Emit `game_start` once per game when a new non-null `gameId` first has a
 * non-null `gameState`. Mirrors `game_end`'s edge-trigger machinery
 * (closure-scoped last-emitted gameId, spectate skip): counts are per-client
 * sessions, not per game.
 *
 * Resume semantics: a mid-game reload re-emits `game_start` in the new session
 * (the closure state resets on page load). This matches `game_end`'s existing
 * per-client-session semantics — a resumed-and-finished game stays balanced
 * because `game_end` re-emits in the same session too; only a reload-then-
 * abandon skews starts-vs-ends, which is itself signal.
 */
function installGameStartTracking(): void {
  let lastEmittedGameId: string | null = null;

  useGameStore.subscribe(
    (s) => s.gameState,
    (gameState) => {
      if (!gameState) return;
      const { gameId, gameMode, aiSeatIds } = useGameStore.getState();
      if (gameMode === "spectate") return;
      if (gameId === null || gameId === lastEmittedGameId) return;
      lastEmittedGameId = gameId;

      trackEvent("game_start", {
        game_mode: gameMode,
        format: gameState.format_config?.format,
        player_count: gameState.players.length,
        ai_count: aiSeatIds.length,
      });
    },
  );
}

/** Guards `session_start` to exactly one emit per page load, even if
 *  `installTelemetry` is ever called twice (hot reload). */
let sessionStarted = false;

/** Install all telemetry hooks. Idempotent-safe to call once at boot. */
export function installTelemetry(): void {
  installTelemetryLifecycle();
  installErrorListeners();
  installStuckDecisionTracking();
  installGameEndTracking();
  installGameStartTracking();
  // Once per page load: the session's opening route. Version/platform/build
  // ride the batch envelope, so `route` is the only field.
  if (!sessionStarted) {
    sessionStarted = true;
    trackEvent("session_start", { route: coarseRoute() });
  }
}
