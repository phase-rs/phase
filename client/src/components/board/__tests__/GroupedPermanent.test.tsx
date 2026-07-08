import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPendingCast,
  buildTargetSelectionProgress,
  buildTargetSelectionSlot,
  buildTargetSelectionWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { toCardProps } from "../../../viewmodel/cardProps.ts";
import type { GroupedPermanent as GroupedPermanentType } from "../../../viewmodel/battlefieldProps.ts";
import { BattlefieldRow } from "../BattlefieldRow.tsx";
import { BoardInteractionContext } from "../BoardInteractionContext.tsx";
import { GroupedPermanentDisplay } from "../GroupedPermanent.tsx";

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
}));

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: ({ cardName }: { cardName: string }) => (
    <div aria-label={cardName} style={{ height: "var(--card-h)", width: "var(--card-w)" }} />
  ),
}));

function makeObject(id: number): GameObject {
  return buildGameObject({
    id,
    card_id: 100,
    name: "Saproling",
    power: 1,
    toughness: 1,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Saproling"] },
    color: ["Green"],
    base_power: 1,
    base_toughness: 1,
    base_color: ["Green"],
    timestamp: id,
  });
}

function makeState(waitingFor: WaitingFor): GameState {
  const objects = buildObjectMap(
    ...[1, 2, 3, 4, 5].map((id) => makeObject(id)),
  );
  return buildGameState({
    objects,
    battlefield: [1, 2, 3, 4, 5],
    waiting_for: waitingFor,
  });
}

function makeGroup(): GroupedPermanentType {
  return {
    name: "Saproling",
    ids: [1, 2, 3, 4, 5],
    count: 5,
    representative: toCardProps(makeObject(1)),
  };
}

function renderGroup(options: {
  boardChoiceObjectIds?: Set<number>;
  validAttackerIds?: Set<number>;
  validTargetObjectIds?: Set<number>;
  committedAttackerIds?: Set<number>;
} = {}) {
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds: new Set(),
        boardChoiceObjectIds: options.boardChoiceObjectIds ?? new Set(),
        committedAttackerIds: options.committedAttackerIds ?? new Set(),
        incomingAttackerCounts: new Map(),
        manaTappableObjectIds: new Set(),
        selectableSacrificeObjectIds: new Set(),
        selectableManaCostCreatureIds: new Set(),
        undoableTapObjectIds: new Set(),
        validAttackerIds: options.validAttackerIds ?? new Set(),
        validTargetObjectIds: options.validTargetObjectIds ?? new Set(),
      }}
    >
      <GroupedPermanentDisplay
        group={makeGroup()}
        rowType="creatures"
        manualExpanded={false}
        onExpand={vi.fn()}
      />
    </BoardInteractionContext.Provider>,
  );
}

function renderCreatureRow() {
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds: new Set(),
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
      <BattlefieldRow groups={[makeGroup()]} rowType="creatures" />
    </BoardInteractionContext.Provider>,
  );
}

describe("GroupedPermanentDisplay collapsed creature groups", () => {
  beforeEach(() => {
    const waitingFor: WaitingFor = {
      type: "DeclareAttackers",
      data: { player: 0, valid_attacker_ids: [1, 2, 3, 4, 5] },
    };
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
      legalActions: [],
      legalActionsByObject: {},
      spellCosts: {},
    });
    useUiStore.setState({
      selectedObjectId: null,
      hoveredObjectId: null,
      inspectedObjectId: null,
      combatMode: null,
      selectedAttackers: [],
      blockerAssignments: new Map(),
      combatClickHandler: null,
      selectedCardIds: [],
      pendingAbilityChoice: null,
    });
    usePreferencesStore.setState({
      battlefieldCardDisplay: "full_card",
      showKeywordStrip: false,
      tapRotation: "classic",
    });
    vi.mocked(dispatchAction).mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders five matching creatures as one representative with a prominent count badge", () => {
    const { container } = renderGroup();

    expect(container.querySelectorAll("[data-object-id]")).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Expand Saproling group" })).toHaveTextContent("×5");
  });

  it("regroups manually expanded duplicate creature groups from a stable row control", () => {
    const { container } = renderCreatureRow();

    fireEvent.click(screen.getByRole("button", { name: "Expand Saproling group" }));

    expect(container.querySelectorAll("[data-object-id]")).toHaveLength(5);

    fireEvent.click(screen.getByRole("button", { name: "Regroup duplicate creature groups" }));

    expect(container.querySelectorAll("[data-object-id]")).toHaveLength(1);
  });

  it("opens an attacker picker that replaces only this group's selected attackers", () => {
    useUiStore.setState({ combatMode: "attackers", selectedAttackers: [99] });
    renderGroup({ validAttackerIds: new Set([1, 2, 3, 4, 5]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));
    fireEvent.click(screen.getByRole("button", { name: "+1" }));

    expect(useUiStore.getState().selectedAttackers).toEqual([99, 1]);

    fireEvent.click(screen.getByRole("button", { name: "All" }));

    expect(useUiStore.getState().selectedAttackers).toEqual([99, 1, 2, 3, 4, 5]);
  });

  it("dispatches a concrete target choice from the picker", () => {
    const waitingFor = buildTargetSelectionWaitingFor({
      data: {
        player: 0,
        pending_cast: buildPendingCast(),
        target_slots: [
          buildTargetSelectionSlot({
            legal_targets: [{ Object: 1 }, { Object: 2 }, { Object: 3 }],
          }),
        ],
        selection: buildTargetSelectionProgress({
          current_legal_targets: [{ Object: 1 }, { Object: 2 }, { Object: 3 }],
        }),
      },
    });
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
    });
    renderGroup({ validTargetObjectIds: new Set([1, 2, 3]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));
    fireEvent.click(screen.getByRole("button", { name: "#3" }));

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Object: 3 } },
    });
  });

  it("dispatches a concrete equip target from the picker", () => {
    const waitingFor: WaitingFor = {
      type: "EquipTarget",
      data: {
        player: 0,
        equipment_id: 42,
        valid_targets: [1, 2, 3],
      },
    };
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
    });
    renderGroup({ validTargetObjectIds: new Set([1, 2, 3]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));
    fireEvent.click(screen.getByRole("button", { name: "#2" }));

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "Equip",
      data: { equipment_id: 42, target_id: 2 },
    });
  });

  it("dispatches an immediate board choice from a collapsed group picker", () => {
    const waitingFor: WaitingFor = {
      type: "StationTarget",
      data: {
        player: 0,
        spacecraft_id: 42,
        eligible_creatures: [1, 2, 3],
      },
    };
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
    });
    renderGroup({ boardChoiceObjectIds: new Set([1, 2, 3]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));
    // All eligible creatures in a collapsed group are visually identical, so the
    // picker resolves with a single labelled action instead of a #1..#N list.
    fireEvent.click(screen.getByRole("button", { name: "Station" }));

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "ActivateStation",
      data: { spacecraft_id: 42, creature_id: 1 },
    });
  });

  it("sacrifices one of many identical tokens with a single action (no #1-#N list) — #4375", () => {
    const waitingFor: WaitingFor = {
      type: "EffectZoneChoice",
      data: {
        player: 0,
        cards: [1, 2, 3, 4, 5],
        count: 1,
        source_id: 99,
        effect_kind: "Sacrifice",
        zone: "Battlefield",
        destination: null,
      },
    };
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
    });
    renderGroup({ boardChoiceObjectIds: new Set([1, 2, 3, 4, 5]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));

    // No numbered per-token list — the indistinguishable tokens collapse to one
    // action button labelled by intent.
    expect(screen.queryByRole("button", { name: "#1" })).toBeNull();
    expect(screen.queryByRole("button", { name: "#5" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Sacrifice" }));

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [1] },
    });
  });

  it("picks a quantity of identical tokens via the stepper for a multi sacrifice — #4375", () => {
    const waitingFor: WaitingFor = {
      type: "EffectZoneChoice",
      data: {
        player: 0,
        cards: [1, 2, 3, 4, 5],
        count: 2,
        source_id: 99,
        effect_kind: "Sacrifice",
        zone: "Battlefield",
        destination: null,
      },
    };
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
    });
    renderGroup({ boardChoiceObjectIds: new Set([1, 2, 3, 4, 5]) });

    fireEvent.click(screen.getByRole("button", { name: "Choose Saproling token" }));

    // Count stepper replaces the #1..#N toggle grid.
    expect(screen.queryByRole("button", { name: "#1" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: "+1" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchAction).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [1, 2] },
    });
  });

  it("auto-expands committed attackers during blocker declaration", () => {
    const waitingFor: WaitingFor = {
      type: "DeclareBlockers",
      data: {
        player: 0,
        valid_blocker_ids: [1, 2, 3, 4, 5],
        valid_block_targets: { 1: [99], 2: [99], 3: [99], 4: [99], 5: [99] },
      },
    };
    useGameStore.setState({
      gameState: makeState(waitingFor),
      waitingFor,
    });
    useUiStore.setState({ combatMode: "blockers" });
    const { container } = renderGroup({ committedAttackerIds: new Set([2]) });

    expect(container.querySelectorAll("[data-object-id]")).toHaveLength(5);
  });
});
