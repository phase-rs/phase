import { useCallback } from "react";
import { useNavigate } from "react-router";

import { clearPromptOverlayState } from "../game/sessionCleanup";
import { clearGame, useGameStore } from "../stores/gameStore";
import { useDraftStore } from "../stores/draftStore";
import { useMultiplayerDraftStore } from "../stores/multiplayerDraftStore";
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
 *  2. `isDraftPodMatch` — pod-match concede (P2P adapter sendConcede +
 *     multiplayer draft store concession report).
 *  3. Default — AI / local / p2p-host / p2p-join: dispatch `Concede` to the
 *     engine, then clear local state and navigate home.
 *
 * Online mode (`isOnlineMode && onConcede`) is intentionally NOT handled
 * here — the menu calls `onConcede()` directly to preserve the existing
 * confirmation-dialog UX.
 */
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
      const adapter = useGameStore.getState().adapter as {
        sendConcede?: () => void | Promise<void>;
      } | null;
      // Host's sendConcede (p2p-adapter.ts:1062) is async — it awaits
      // concedePlayer (engine dispatch) then fans out player_conceded to
      // every guest's PeerJS data channel. Guest/WS versions are sync
      // void-returners; Promise.resolve() normalizes both shapes so we
      // serialize the chain and never tear down the adapter mid-fan-out.
      const sendPromise = adapter?.sendConcede
        ? Promise.resolve(adapter.sendConcede())
        : Promise.resolve();
      void sendPromise
        .catch((err) => {
          console.error("[useConcedeHandler] sendConcede failed:", err);
        })
        .then(() => useMultiplayerDraftStore.getState().reportActiveMatchConcession())
        .then(() => {
          clearGame(gameId);
          navigate("/draft-pod");
        })
        .catch((err) => {
          // User intent is to leave — strand them on the conceded screen
          // is a worse outcome than logging the store-mutation failure.
          console.error("[useConcedeHandler] failed to report draft pod concession:", err);
          clearGame(gameId);
          navigate("/draft-pod");
        });
      return;
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
        await clearGame(gameId);
        navigate("/");
      })
      .catch(async (err) => {
        console.error("[useConcedeHandler] concede dispatch failed:", err);
        // Still clear + navigate on failure — the user has decided to leave.
        clearPromptOverlayState();
        await clearGame(gameId);
        navigate("/");
      });
  }, [gameId, isDraft, isDraftPodMatch, navigate]);
}
