import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, SubmitResult } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildEngineAdapterMock } from "../../../test/factories/engineAdapterFactory.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPlayers,
  buildPriorityWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { BoardInteractionContext } from "../BoardInteractionContext.tsx";
import { PermanentCard } from "../PermanentCard.tsx";

/*
 * Issue #6016 — the production click path for an activated ability. A click on
 * an activatable permanent dispatches the engine-offered `ActivateAbility`
 * through the real `dispatchAction` → `applySpellPaymentPreference` pipeline, so
 * a saved Manual preference (or the per-game override) reaches the adapter as
 * `payment_mode: Manual`. The engine owns everything after that: its reducer
 * rows (activation_manual_mana_payment.rs) prove the window that Manual opens.
 * Only the adapter and display-only card art are substituted here.
 */

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => <div aria-label={cardName} />,
}));

const SPEAKER = 1;

const offeredActivation: GameAction = {
  type: "ActivateAbility",
  data: { source_id: SPEAKER, ability_index: 0 },
};

function speakerState() {
  const speaker = buildGameObject({
    id: SPEAKER,
    card_id: 100,
    zone: "Battlefield",
    name: "Formidable Speaker",
    power: 2,
    toughness: 2,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Elf", "Druid"] },
    mana_cost: { type: "Cost", shards: ["Green"], generic: 2 },
    color: ["Green"],
    base_power: 2,
    base_toughness: 2,
    base_color: ["Green"],
    entered_battlefield_turn: null,
  });
  return buildGameState({
    players: buildPlayers([0, 1]),
    objects: buildObjectMap(speaker),
    battlefield: [SPEAKER],
    exile: [],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
  });
}

function renderSpeaker() {
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds: new Set([SPEAKER]),
        blockableAttackerIds: new Set(),
        boardChoiceObjectIds: new Set(),
        committedAttackerIds: new Set(),
        incomingAttackerCounts: new Map(),
        manaTappableObjectIds: new Set(),
        selectableSacrificeObjectIds: new Set(),
        selectableManaCostCreatureIds: new Set(),
        undoableTapObjectIds: new Set(),
        validAttackerIds: new Set(),
        validTargetObjectIds: new Set(),
      }}
    >
      <PermanentCard objectId={SPEAKER} />
    </BoardInteractionContext.Provider>,
  );
}

function installAdapter() {
  const state = speakerState();
  const adapter = buildEngineAdapterMock(state, {
    submitAction: vi.fn().mockResolvedValue({ events: [], log_entries: [] } satisfies SubmitResult),
  });
  act(() => {
    useGameStore.setState({
      adapter,
      gameState: state,
      waitingFor: state.waiting_for,
      gameMode: "ai",
      legalActions: [offeredActivation],
      legalActionsByObject: { [String(SPEAKER)]: [offeredActivation] },
    });
  });
  return adapter;
}

function clickSpeaker(container: HTMLElement) {
  fireEvent.click(container.querySelector(`[data-object-id="${SPEAKER}"]`) as HTMLElement);
}

describe("PermanentCard activation payment preference", () => {
  beforeEach(() => {
    window.matchMedia = ((query: string) => ({
      matches: query === "(hover: hover)" || query === "(any-hover: hover)",
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })) as unknown as typeof window.matchMedia;
    act(() => {
      usePreferencesStore.setState({
        spellPaymentMode: "auto",
        animationSpeedMultiplier: 0,
        battlefieldCardDisplay: "full_card",
        showKeywordStrip: false,
      });
      useUiStore.setState({ manualManaOverride: false, pendingAbilityChoice: null });
    });
  });

  afterEach(() => {
    cleanup();
    localStorage.clear();
    act(() => useGameStore.getState().reset());
  });

  it("submits the clicked activation as Manual under a persisted manual preference", async () => {
    // The saved preference is restored through the real persistence layer.
    const persisted = { ...usePreferencesStore.getState(), spellPaymentMode: "manual" };
    localStorage.setItem(
      "phase-preferences",
      JSON.stringify({ state: persisted, version: usePreferencesStore.persist.getOptions().version }),
    );
    await act(async () => {
      await usePreferencesStore.persist.rehydrate();
    });
    expect(usePreferencesStore.getState().spellPaymentMode).toBe("manual");
    const adapter = installAdapter();

    const { container } = renderSpeaker();
    clickSpeaker(container);

    await vi.waitFor(() => expect(adapter.submitAction).toHaveBeenCalledOnce());
    expect(adapter.submitAction).toHaveBeenCalledWith(
      {
        type: "ActivateAbility",
        data: { source_id: SPEAKER, ability_index: 0, payment_mode: { type: "Manual" } },
      },
      0,
    );
  });

  it("submits the clicked activation as Manual under the per-game override", async () => {
    act(() => useUiStore.setState({ manualManaOverride: true }));
    const adapter = installAdapter();

    const { container } = renderSpeaker();
    clickSpeaker(container);

    await vi.waitFor(() => expect(adapter.submitAction).toHaveBeenCalledOnce());
    expect(adapter.submitAction).toHaveBeenCalledWith(
      {
        type: "ActivateAbility",
        data: { source_id: SPEAKER, ability_index: 0, payment_mode: { type: "Manual" } },
      },
      0,
    );
  });

  it("submits the offered activation unchanged under a saved auto preference", async () => {
    const adapter = installAdapter();

    const { container } = renderSpeaker();
    clickSpeaker(container);

    await vi.waitFor(() => expect(adapter.submitAction).toHaveBeenCalledOnce());
    expect(adapter.submitAction).toHaveBeenCalledWith(offeredActivation, 0);
  });

  it("never stamps the human's preference onto another seat's action", async () => {
    act(() => usePreferencesStore.setState({ spellPaymentMode: "manual" }));
    const adapter = installAdapter();

    await act(async () => {
      await dispatchAction(offeredActivation, 1);
    });

    expect(adapter.submitAction).toHaveBeenCalledWith(offeredActivation, 1);
  });
});
