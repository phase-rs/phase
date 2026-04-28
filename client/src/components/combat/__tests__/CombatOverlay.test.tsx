import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { CombatOverlay } from "../CombatOverlay";
import { useGameStore } from "../../../stores/gameStore";
import { useUiStore } from "../../../stores/uiStore";
import type { GameState } from "../../../adapter/types";

// Mock useGameDispatch to return a spy
const mockDispatch = vi.fn().mockResolvedValue(undefined);
vi.mock("../../../hooks/useGameDispatch", () => ({
  useGameDispatch: () => mockDispatch,
}));

// Mock BlockerArrow since it reads DOM positions
vi.mock("../BlockerArrow", () => ({
  BlockerArrow: ({ blockerId, attackerId }: { blockerId: number; attackerId: number }) => (
    <div data-testid={`arrow-${blockerId}-${attackerId}`} />
  ),
}));

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "DeclareAttackers",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects: {
      "100": {
        id: 100, card_id: 1, owner: 0, controller: 0, zone: "Battlefield",
        tapped: false, face_down: false, flipped: false, transformed: false,
        damage_marked: 0, dealt_deathtouch_damage: false, attached_to: null,
        attachments: [], counters: {}, name: "Grizzly Bears", power: 2, toughness: 2,
        loyalty: null, card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Bear"] },
        mana_cost: { type: "Cost", shards: ["G", "G"], generic: 0 }, keywords: [], abilities: [],
        trigger_definitions: [], replacement_definitions: [], static_definitions: [],        color: ["Green"], base_power: 2, base_toughness: 2, base_keywords: [], base_color: ["Green"],
        timestamp: 1, entered_battlefield_turn: 1,
      },
      "101": {
        id: 101, card_id: 2, owner: 0, controller: 0, zone: "Battlefield",
        tapped: false, face_down: false, flipped: false, transformed: false,
        damage_marked: 0, dealt_deathtouch_damage: false, attached_to: null,
        attachments: [], counters: {}, name: "Elvish Mystic", power: 1, toughness: 1,
        loyalty: null, card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Elf", "Druid"] },
        mana_cost: { type: "Cost", shards: ["G"], generic: 0 }, keywords: [], abilities: [],
        trigger_definitions: [], replacement_definitions: [], static_definitions: [],        color: ["Green"], base_power: 1, base_toughness: 1, base_keywords: [], base_color: ["Green"],
        timestamp: 2, entered_battlefield_turn: 1,
      },
    },
    next_object_id: 102,
    battlefield: [100, 101],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: { type: "DeclareAttackers", data: { player: 0, valid_attacker_ids: [100, 101] } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 3,
    ...overrides,
  };
}

describe("CombatOverlay", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    // Reset stores
    useUiStore.setState({
      combatMode: null,
      selectedAttackers: [],
      blockerAssignments: new Map(),
      combatClickHandler: null,
    });
    const gameState = createGameState();
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders AttackerControls when mode is attackers", () => {
    render(<CombatOverlay mode="attackers" />);
    expect(screen.getByText("Attack All")).toBeInTheDocument();
    expect(screen.getByText("Skip")).toBeInTheDocument();
    expect(screen.getByText(/Confirm Attackers/)).toBeInTheDocument();
  });

  it("renders BlockerControls when mode is blockers", () => {
    render(<CombatOverlay mode="blockers" />);
    expect(screen.getByText(/Confirm Blockers/)).toBeInTheDocument();
  });

  it("calls setCombatMode on mount and clearCombatSelection on unmount", () => {
    const { unmount } = render(<CombatOverlay mode="attackers" />);

    // After mount, combatMode should be set
    expect(useUiStore.getState().combatMode).toBe("attackers");

    unmount();

    // After unmount, combat selection should be cleared
    expect(useUiStore.getState().combatMode).toBeNull();
    expect(useUiStore.getState().selectedAttackers).toEqual([]);
  });

  it("Attack All button calls selectAllAttackers with valid attacker IDs", () => {
    render(<CombatOverlay mode="attackers" />);

    fireEvent.click(screen.getByText("Attack All"));

    // Both creatures (100, 101) are untapped creatures controlled by player 0
    const attackers = useUiStore.getState().selectedAttackers;
    expect(attackers).toContain(100);
    expect(attackers).toContain(101);
    expect(attackers).toHaveLength(2);
  });

  it("Skip button dispatches DeclareAttackers with empty array", () => {
    render(<CombatOverlay mode="attackers" />);

    fireEvent.click(screen.getByText("Skip"));

    expect(mockDispatch).toHaveBeenCalledWith({
      type: "DeclareAttackers",
      data: { attacks: [] },
    });
  });

  it("Confirm Attackers dispatches DeclareAttackers with selected IDs as attacks", () => {
    // Pre-select an attacker
    useUiStore.setState({ selectedAttackers: [100] });

    render(<CombatOverlay mode="attackers" />);

    fireEvent.click(screen.getByText(/Confirm Attackers/));

    expect(mockDispatch).toHaveBeenCalledWith({
      type: "DeclareAttackers",
      data: { attacks: [[100, { type: "Player", data: 1 }]] },
    });
  });
});
