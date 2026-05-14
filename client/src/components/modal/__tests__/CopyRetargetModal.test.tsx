import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, TargetRef, WaitingFor } from "../../../adapter/types.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeObject(id: number, name: string) {
  return {
    id,
    card_id: 1,
    owner: 0,
    controller: 0,
    zone: "Battlefield" as const,
    tapped: false,
    face_down: false,
    flipped: false,
    transformed: false,
    damage_marked: 0,
    dealt_deathtouch_damage: false,
    attached_to: null,
    attachments: [],
    counters: {},
    name,
    power: 1,
    toughness: 1,
    loyalty: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
    mana_cost: { type: "NoCost" as const },
    keywords: [],
    abilities: [],
    trigger_definitions: [],
    replacement_definitions: [],
    static_definitions: [],
    color: [],
    base_power: 1,
    base_toughness: 1,
    base_keywords: [],
    base_color: [],
    timestamp: 1,
    entered_battlefield_turn: 1,
  };
}

function makeState(waitingFor: WaitingFor): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 40, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects: {
      42: makeObject(42, "Walking Ballista"),
      43: makeObject(43, "Hangarback Walker"),
      44: makeObject(44, "Arcbound Worker"),
    },
    next_object_id: 100,
    battlefield: [42, 43, 44],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: waitingFor,
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 2,
    eliminated_players: [],
  } as unknown as GameState;
}

function setWaitingFor(targetSlots: { current: TargetRef; legal_alternatives: TargetRef[] }[]) {
  const waitingFor: WaitingFor = {
    type: "CopyRetarget",
    data: {
      player: 0,
      copy_id: 7,
      target_slots: targetSlots,
    },
  };
  useGameStore.setState({
    gameMode: "online",
    gameState: makeState(waitingFor),
    waitingFor,
  });
}

describe("CopyRetargetModal (via CardChoiceModal)", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("defaults to current targets and dispatches SelectTargets", () => {
    setWaitingFor([
      { current: { Object: 42 }, legal_alternatives: [{ Object: 42 }, { Object: 43 }] },
      { current: { Player: 1 }, legal_alternatives: [{ Player: 1 }, { Object: 44 }] },
    ]);
    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectTargets",
      data: { targets: [{ Object: 42 }, { Player: 1 }] },
    });
  });

  it("chooses legal alternatives per slot", () => {
    setWaitingFor([
      { current: { Object: 42 }, legal_alternatives: [{ Object: 42 }, { Object: 43 }] },
      { current: { Player: 1 }, legal_alternatives: [{ Player: 1 }, { Object: 44 }] },
    ]);
    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: /Hangarback Walker/ }));
    fireEvent.click(screen.getByRole("button", { name: /Arcbound Worker/ }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectTargets",
      data: { targets: [{ Object: 43 }, { Object: 44 }] },
    });
  });

  it("does not offer current target when populated alternatives exclude it", () => {
    setWaitingFor([{ current: { Object: 42 }, legal_alternatives: [{ Object: 43 }] }]);
    render(<CardChoiceModal />);

    expect(screen.queryByRole("button", { name: /Walking Ballista/ })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectTargets",
      data: { targets: [{ Object: 43 }] },
    });
  });

  it("keeps current target when alternatives are empty", () => {
    setWaitingFor([{ current: { Object: 42 }, legal_alternatives: [] }]);
    render(<CardChoiceModal />);

    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectTargets",
      data: { targets: [{ Object: 42 }] },
    });
  });

  it("renders nothing when the current player cannot act", () => {
    useMultiplayerStore.setState({ activePlayerId: 1 });
    setWaitingFor([{ current: { Object: 42 }, legal_alternatives: [{ Object: 43 }] }]);

    const { container } = render(<CardChoiceModal />);

    expect(container).toBeEmptyDOMElement();
  });
});
