import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState, WaitingFor } from "../../../adapter/types.ts";
import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import type { GroupedPermanent as GroupedPermanentType } from "../../../viewmodel/battlefieldProps.ts";
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
  return {
    id,
    card_id: 100,
    owner: 0,
    controller: 0,
    zone: "Battlefield",
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name: "Saproling",
    power: 1,
    toughness: 1,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Saproling"] },
    mana_cost: { type: "NoCost" },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: ["Green"],
    base_power: 1,
    base_toughness: 1,
    base_keywords: [],
    base_color: ["Green"],
    timestamp: id,
    entered_battlefield_turn: null,
  };
}

function makeState(waitingFor: WaitingFor): GameState {
  const objects = Object.fromEntries(
    [1, 2, 3, 4, 5].map((id) => [id, makeObject(id)]),
  );
  return {
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    objects,
    battlefield: [1, 2, 3, 4, 5],
    exile: [],
    stack: [],
    combat: null,
    waiting_for: waitingFor,
  } as unknown as GameState;
}

function makeGroup(): GroupedPermanentType {
  return {
    name: "Saproling",
    ids: [1, 2, 3, 4, 5],
    count: 5,
    representative: {} as GroupedPermanentType["representative"],
  };
}

function renderGroup(options: {
  validAttackerIds?: Set<number>;
  validTargetObjectIds?: Set<number>;
  committedAttackerIds?: Set<number>;
} = {}) {
  return render(
    <BoardInteractionContext.Provider
      value={{
        activatableObjectIds: new Set(),
        committedAttackerIds: options.committedAttackerIds ?? new Set(),
        incomingAttackerCounts: new Map(),
        manaTappableObjectIds: new Set(),
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
        onCollapse={vi.fn()}
      />
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
    const waitingFor = {
      type: "TargetSelection",
      data: {
        player: 0,
        pending_cast: {},
        target_slots: [],
        selection: {
          current_legal_targets: [{ Object: 1 }, { Object: 2 }, { Object: 3 }],
          selected_targets: [],
        },
      },
    } as unknown as WaitingFor;
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
