import { beforeEach, describe, expect, it } from "vitest";

import type { GameState } from "../../adapter/types";
import { clearPromptOverlayState } from "../sessionCleanup";
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";

describe("clearPromptOverlayState", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    useUiStore.setState({
      pendingAbilityChoice: null,
      enchantmentsDialogPlayer: null,
      manualManaOverride: false,
    });
  });

  it("clears convoke ManaPayment and UI dialogs without disposing the adapter", () => {
    const adapter = { dispose: () => {} };
    useGameStore.setState({
      adapter: adapter as never,
      waitingFor: {
        type: "ManaPayment",
        data: { player: 0, convoke_mode: "Convoke" },
      },
      legalActions: [{ type: "PassPriority" }],
      autoPassRecommended: true,
      spellCosts: { "1": { type: "Cost", shards: ["G"], generic: 0 } },
      legalActionsByObject: { 1: [{ type: "TapForConvoke", data: { object_id: 1, mana_type: "Green" } }] },
      resolutionProgress: { resolved: 2, total: 5 },
      isResolvingAll: true,
      gameState: { waiting_for: { type: "ManaPayment", data: { player: 0, convoke_mode: "Convoke" } } } as GameState,
    });
    useUiStore.setState({
      pendingAbilityChoice: {
        objectId: 1,
        actions: [{ type: "TapForConvoke", data: { object_id: 1, mana_type: "Green" } }],
      },
      enchantmentsDialogPlayer: 0,
    });

    clearPromptOverlayState();

    const state = useGameStore.getState();
    expect(state.waitingFor).toBeNull();
    expect(state.legalActions).toEqual([]);
    expect(state.autoPassRecommended).toBe(false);
    expect(state.spellCosts).toEqual({});
    expect(state.legalActionsByObject).toEqual({});
    expect(state.resolutionProgress).toBeNull();
    expect(state.isResolvingAll).toBe(false);
    expect(state.adapter).toBe(adapter);
    expect(state.gameState).not.toBeNull();
    expect(useUiStore.getState().pendingAbilityChoice).toBeNull();
    expect(useUiStore.getState().enchantmentsDialogPlayer).toBeNull();
  });

  it("resets the per-game manualManaOverride toggle so it can't leak across games", () => {
    useUiStore.setState({ manualManaOverride: true });

    clearPromptOverlayState();

    expect(useUiStore.getState().manualManaOverride).toBe(false);
  });
});
