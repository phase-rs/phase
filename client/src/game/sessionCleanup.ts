import { useGameStore } from "../stores/gameStore.ts";
import { useUiStore } from "../stores/uiStore.ts";

/**
 * Drop engine prompt + UI-dialog overlay state without disposing the WASM
 * adapter. Used on game-session boundaries (concede, provider unmount/remount)
 * so deferred store resets or async `initGame` cannot leave `ManaPayment` /
 * `pendingAbilityChoice` bleed into the next session (issue #2369).
 */
export function clearPromptOverlayState(): void {
  useGameStore.setState({
    waitingFor: null,
    legalActions: [],
    autoPassRecommended: false,
    spellCosts: {},
    legalActionsByObject: {},
    resolutionProgress: null,
  });
  useUiStore.getState().setPendingAbilityChoice(null);
  useUiStore.getState().setEnchantmentsDialogPlayer(null);
}
