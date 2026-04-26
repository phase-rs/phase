import type { ObjectId } from "../adapter/types.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { useCanActForWaitingState } from "./usePlayerId.ts";

/**
 * Whether `id` is a legal *object* target for the current `WaitingFor` state
 * AND the local player is the one prompted to choose. The second gate is
 * load-bearing for multiplayer: in a 2P game where the opponent is choosing
 * targets, their `current_legal_targets` is filtered into the local player's
 * state too, so without the gate every targetable surface on either side of
 * the board would glow and dispatch a `ChooseTarget` the engine would reject
 * (or, worse, accept out-of-turn).
 *
 * Single authority for the four target-like prompts the engine emits, mirroring
 * the bulk-derivation block at `GameBoard.boardInteractionState` (which exists
 * for board-row perf and continues to live there). Off-board surfaces —
 * `StackEntry`, `ZoneViewer.CardSlot`, `AttachmentChip` — consume this hook
 * directly so they don't each rederive the same `Set<ObjectId>` and re-implement
 * the player gate.
 *
 * Returns `false` when no relevant prompt is active. Player-target prompts
 * (`PlayerHud`/`OpponentHud`) read `current_legal_targets` separately because
 * they dispatch `ChooseTarget { Player }`, not `{ Object }`.
 */
export function useIsValidObjectTarget(id: ObjectId): boolean {
  const canAct = useCanActForWaitingState();
  return useGameStore((s) => {
    if (!canAct) return false;
    const wf = s.waitingFor;
    if (!wf) return false;
    if (wf.type === "TargetSelection" || wf.type === "TriggerTargetSelection") {
      return wf.data.selection.current_legal_targets.some(
        (t) => "Object" in t && t.Object === id,
      );
    }
    if (wf.type === "CopyTargetChoice") return wf.data.valid_targets.includes(id);
    if (wf.type === "ExploreChoice") return wf.data.choosable.includes(id);
    return false;
  });
}
