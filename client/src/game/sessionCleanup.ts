import { useGameStore } from "../stores/gameStore.ts";
import { useUiStore } from "../stores/uiStore.ts";

/**
 * Drop engine prompt + UI-dialog overlay state without disposing the WASM
 * adapter. Used on game-session boundaries (concede, provider unmount/remount)
 * so deferred store resets or async `initGame` cannot leave `ManaPayment` /
 * `pendingAbilityChoice` bleed into the next session (issue #2369).
 *
 * Documented exemption from the `commitEngineSnapshot` single-writer invariant:
 * this is a session-boundary CLEAR, not a live-game commit. It writes the prompt
 * + legal-action fields without advancing `lastCommittedSeq`, so a commit already
 * in flight can re-populate the prompts it just cleared. That race pre-dates this
 * mechanism and is neither worsened nor fixed by it — the clear has no snapshot to
 * gate on, and gating it would require inventing an epoch it doesn't have.
 */
export function clearPromptOverlayState(): void {
  useGameStore.setState({
    waitingFor: null,
    legalActions: [],
    autoPassRecommended: false,
    manaPaymentShortcutActions: [],
    spellCosts: {},
    legalActionsByObject: {},
    resolutionProgress: null,
    isResolvingAll: false,
  });
  useUiStore.getState().setPendingAbilityChoice(null);
  useUiStore.getState().setEnchantmentsDialogPlayer(null);
  useUiStore.getState().setAttachmentFanHost(null);
  // The per-game "Manual mana" toggle must never leak into the next game.
  useUiStore.getState().setManualManaOverride(false);
  // The ephemeral hand hide-filter is a per-game focus aid — reset it too.
  useUiStore.getState().setHandFilter("none");
}
