import { useCallback } from "react";
import { useNavigate } from "react-router";

import { clearPromptOverlayState } from "../game/sessionCleanup";
import { clearGame, useGameStore } from "../stores/gameStore";
import { useDraftStore } from "../stores/draftStore";
import { useMultiplayerDraftStore } from "../stores/multiplayerDraftStore";
import { supportsMatchConcede } from "../adapter/types";
import { getPlayerId } from "./usePlayerId";

export interface ConcedeHandlerOptions {
  gameId: string;
  isOnlineMode: boolean;
  isDraft: boolean;
  isDraftPodMatch: boolean;
  /**
   * Online-only callback that opens the confirmation dialog. When provided
   * and `isOnlineMode` is true, the menu wires this directly; the hook does
   * NOT invoke it. Online concede confirmation routes through this callback
   * upstream of the hook.
   */
  onConcede?: () => void;
}

/**
 * Unified concede handler for the game menu's "Concede" action.
 *
 * CR 104.3a: A player can concede the game at any time. A player who concedes
 *   leaves the game. That player loses the game.
 * CR 800.4a: When a player leaves a multiplayer game, all permanents/spells/
 *   abilities owned by that player leave the game, and SBAs (CR 704) then
 *   resolve the resulting game state.
 *
 * In AI/local/p2p modes the previous implementation cleared local game
 * persistence and navigated home without ever dispatching `GameAction::Concede`
 * to the engine. Because `WasmAdapter` is a module-level singleton kept alive
 * across sessions for V8 TurboFan re-warm (see adapter/wasm-adapter.ts), the
 * conceded `GameState` survived in the worker's `RefCell<Option<GameState>>`
 * and remained fully playable on navigation back. This hook fixes that by
 * awaiting a real `Concede` dispatch before clearing local state.
 *
 * Branches (priority order):
 *  1. `isDraft` — quick-draft single-match concede.
 *  2. `isDraftPodMatch` — a transport-installed whole-match capability settles
 *     a pod match when one is bound. When none is (the Commander pod launch,
 *     whose N-seat game has no pairwise settlement to report), this FALLS
 *     THROUGH to the default engine-level `Concede` below rather than refusing:
 *     CR 104.3a — the conceding player leaves the game and loses it — and
 *     CR 800.4a for the resulting elimination, with the remaining players
 *     playing on. This reverses #7920's refusal, which was correct while every
 *     `draft-match` game was a bound 1v1 and would otherwise leave the in-game
 *     Concede button silently inert for every seat of a Commander game.
 *     `boundMatchConcede` is deliberately NOT bound for that launch instead: it
 *     is a 1v1 SETTLEMENT primitive that reports a match result, early-returns
 *     on the null `matchPairing` a Commander launch leaves, and is one-shot.
 *  3. Default — AI / local / p2p-host / p2p-join: dispatch `Concede` to the
 *     engine, then clear local state and navigate home.
 *
 * Online mode (`isOnlineMode && onConcede`) is intentionally NOT handled
 * here — the menu calls `onConcede()` directly to preserve the existing
 * confirmation-dialog UX.
 */
/**
 * Drop the pod-side record of a Commander launch after this client has conceded,
 * WITHOUT tearing the transport down.
 *
 * CR 104.3a: the conceding player leaves the game and loses it. CR 800.4a: the
 * rest of the table plays on. That second half is why this is deliberately NOT
 * `endCommanderSession()`. In this host-authoritative P2P topology the host's
 * adapter IS the game for everyone else, so disposing it because the host
 * conceded would end three other players' game — a rules violation, and a worse
 * defect than the object it would reclaim. The surviving adapter is
 * load-bearing, not a leak.
 *
 * What it does fix: `commanderLaunch` outlives the game it describes, so a
 * conceder returning to the pod would meet a `CompleteView` rendering the
 * launch-in-flight state forever — Launch disabled, Cancel inert because its
 * in-flight handle was already released. Clearing the two fields drops that
 * wedge and leaves the pod offering a fresh launch.
 *
 * Called from inside the dispatch continuation, never before it: the store
 * write is local and harmless on its own, but keeping it downstream of the
 * awaited dispatch preserves the one ordering that matters — the concession
 * reaches the host before this client stops caring about the game.
 */
function releaseCommanderPodState(): void {
  const { commanderLaunch } = useMultiplayerDraftStore.getState();
  if (!commanderLaunch) return;
  useMultiplayerDraftStore.setState({ commanderLaunch: null, commanderSeat: null });
}

export function useConcedeHandler({
  gameId,
  isOnlineMode: _isOnlineMode,
  isDraft,
  isDraftPodMatch,
  onConcede: _onConcede,
}: ConcedeHandlerOptions): () => void {
  const navigate = useNavigate();

  return useCallback(() => {
    if (isDraft) {
      void useDraftStore
        .getState()
        .recordMatchResult(gameId, "loss")
        .then(() => {
          clearGame(gameId);
          navigate("/draft/quick?resume=1");
        });
      return;
    }

    if (isDraftPodMatch) {
      const adapter = useGameStore.getState().adapter;
      if (supportsMatchConcede(adapter)) {
        adapter.sendMatchConcede();
        return;
      }
      // Unbound: fall through to the engine-level Concede below. Reachable
      // only from the Commander pod launch — every `startMatch` branch binds
      // the capability — so this cannot change any existing pod match.
    }

    // Default: AI / local / p2p-host / p2p-join (when no online dialog).
    // Awaiting the dispatch BEFORE clearGame + navigate is the bug fix —
    // it forces the engine to process Concede and run SBAs (CR 704 / 704.5a)
    // before the local persistence layer drops the game ID. Without the
    // await, the WasmAdapter singleton retains the conceded game and the
    // user can resume it by navigating back.
    void useGameStore
      .getState()
      .dispatch({ type: "Concede", data: { player_id: getPlayerId() } })
      .then(async () => {
        clearPromptOverlayState();
        releaseCommanderPodState();
        await clearGame(gameId);
        navigate("/");
      })
      .catch(async (err) => {
        console.error("[useConcedeHandler] concede dispatch failed:", err);
        // Still clear + navigate on failure — the user has decided to leave.
        clearPromptOverlayState();
        releaseCommanderPodState();
        await clearGame(gameId);
        navigate("/");
      });
  }, [gameId, isDraft, isDraftPodMatch, navigate]);
}
