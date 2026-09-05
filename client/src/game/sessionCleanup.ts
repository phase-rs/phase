import { abandonPendingDispatches } from "./dispatch.ts";
import { useGameStore } from "../stores/gameStore.ts";
import { useUiStore } from "../stores/uiStore.ts";

/**
 * Drop engine prompt + UI overlay state without disposing the WASM adapter.
 * Used on game-session boundaries (concede, provider unmount/remount) so
 * deferred store resets or async `initGame` cannot leave `ManaPayment`,
 * `pendingAbilityChoice`, or a roll animation bleeding into the next session.
 *
 * Abandoning the dispatch pipeline is part of the boundary, not an errand on the
 * side. `isAnimating` is a module-level mutex shared by local dispatches and
 * inbound remote updates, so a submit promise that never settles holds it for the
 * rest of the page session — the *next* game is then unresponsive from its first
 * click. `abandonPendingDispatches` releases it and resolves the queue.
 *
 * The generation it bumps is also an epoch this clear can lean on — partially.
 * The clear writes the prompt + legal-action fields without advancing
 * `lastCommittedSeq`, so a commit landing afterwards still wins the store's own
 * `seq` gate. Of the four `commitEngineSnapshot` call sites in `dispatch.ts`, two
 * capture the generation and decline once it is bumped: `processAction` (via
 * `isDispatchContextCurrent`) and `processRemoteUpdateInner` (via
 * `isCurrentDispatchGeneration`). Those can no longer re-populate the prompts this
 * function just cleared.
 *
 * `dispatchInteraction` and `restoreGameState` commit outside the generation gate
 * and still can. That race pre-dates this mechanism and is not fixed here; closing
 * it means threading the generation through those two paths, which is a change to
 * the single-writer invariant rather than to this boundary.
 */
export function clearPromptOverlayState(): void {
  abandonPendingDispatches();
  useGameStore.setState({
    waitingFor: null,
    legalActions: [],
    autoPassRecommended: false,
    endContinuousEffectOffers: [],
    manaPaymentShortcutActions: [],
    spellCosts: {},
    legalActionsByObject: {},
    activationBlockReasons: {},
    restoredStackAutomation: null,
  });
  useUiStore.getState().setPendingAbilityChoice(null);
  useUiStore.getState().setEnchantmentsDialogPlayer(null);
  useUiStore.getState().setAttachmentFanHost(null);
  useUiStore.getState().setMobileHandGesture(null);
  useUiStore.getState().resetDiceRoll();
  useUiStore.getState().resetScryOutcome();
  // The per-game "Manual mana" toggle must never leak into the next game.
  useUiStore.getState().setManualManaOverride(false);
  // The ephemeral hand hide-filter is a per-game focus aid — reset it too.
  useUiStore.getState().setHandFilter("none");
}
