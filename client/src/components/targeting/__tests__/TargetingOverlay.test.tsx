import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { GameState } from "../../../adapter/types.ts";
import { buildGameObject, buildGameObjectWithCoreTypes } from "../../../test/factories/gameObjectFactory.ts";
import { TargetingOverlay } from "../TargetingOverlay.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 1,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: {
      type: "TriggerTargetSelection",
      data: {
        player: 0,
        target_slots: [{ legal_targets: [{ Player: 1 }], optional: false }],
        selection: {
          current_slot: 0,
          current_legal_targets: [{ Player: 1 }],
        },
      },
    },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
    seat_order: [0, 1],
    format_config: {
      format: "Standard",
      starting_life: 20,
      min_players: 2,
      max_players: 2,
      deck_size: 60,
      singleton: false,
      command_zone: false,
      commander_damage_threshold: null,
      range_of_influence: null,
      team_based: false,
      uses_commander: false,

      allow_debug_actions: false,
    },
    eliminated_players: [],
    ...overrides,
  };
}

describe("TargetingOverlay", () => {
  beforeEach(() => {
    act(() => {
      useMultiplayerStore.setState({ activePlayerId: 0 });
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("does not render player target buttons (handled by HUD components)", () => {
    const dispatch = vi.fn().mockResolvedValue([]);

    act(() => {
      useGameStore.setState({
        gameState: createGameState(),
        waitingFor: createGameState().waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.queryByRole("button", { name: /Target Player/i })).toBeNull();
    expect(screen.getByText("a player")).toBeInTheDocument();
  });

  it("dispatches null target when the active engine slot is optional and skipped", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      waiting_for: {
        type: "TargetSelection",
        data: {
          player: 0,
          pending_cast: {
            object_id: 5,
            card_id: 10,
            ability: { targets: [] },
            cost: { type: "NoCost" },
          },
          target_slots: [{ legal_targets: [], optional: true }],
          selection: {
            current_slot: 0,
            current_legal_targets: [],
          },
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    fireEvent.click(screen.getByRole("button", { name: "Skip" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: null },
    });
  });

  it("allows cancelling tap-creatures spell costs", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      objects: {
        "7": buildGameObjectWithCoreTypes(["Creature"], {
          id: 7,
          name: "Memnite",
        }),
      },
      waiting_for: {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "TapCreatures" },
          choices: [7],
          count: 1,
          min_count: 0,
          resume: {
            type: "Spell",
            Spell: {
              object_id: 5,
              card_id: 10,
              ability: { targets: [] },
              cost: { type: "NoCost" },
            },
          },
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(dispatch).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it("confirms selected creatures for mana ability costs", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      objects: {
        "4": buildGameObjectWithCoreTypes(["Land"], {
          id: 4,
          name: "Holdout Settlement",
        }),
        "7": buildGameObjectWithCoreTypes(["Creature"], {
          id: 7,
          name: "Memnite",
        }),
      },
      waiting_for: {
        type: "PayCost",
        data: {
          player: 0,
          kind: { type: "TapCreatures" },
          choices: [7],
          count: 1,
          min_count: 0,
          resume: {
            type: "ManaAbility",
            ManaAbility: {
              player: 0,
              source_id: 4,
              ability_index: 1,
              resume: "Priority",
            },
          },
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    act(() => {
      useUiStore.setState({ selectedCardIds: [7] });
    });

    fireEvent.click(screen.getByRole("button", { name: "Confirm Tap (1/1)" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: [7] },
    });
  });

  it("confirms aggregate-power board choices when the selected power is high enough", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      objects: {
        "20": buildGameObjectWithCoreTypes(["Artifact"], {
          id: 20,
          name: "Vehicle",
        }),
        "21": buildGameObjectWithCoreTypes(["Creature"], {
          id: 21,
          name: "Pilot One",
          power: 2,
        }),
        "22": buildGameObjectWithCoreTypes(["Creature"], {
          id: 22,
          name: "Pilot Two",
          power: 3,
        }),
      },
      waiting_for: {
        type: "CrewVehicle",
        data: {
          player: 0,
          vehicle_id: 20,
          crew_power: 4,
          eligible_creatures: [21, 22],
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    act(() => {
      useUiStore.setState({ selectedCardIds: [21, 22] });
    });

    fireEvent.click(screen.getByRole("button", { name: "Confirm (5/4 power)" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "CrewVehicle",
      data: { vehicle_id: 20, creature_ids: [21, 22] },
    });
  });

  it("informs the player when the target slot is a spell on the stack", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const stackSpellTarget = buildGameObjectWithCoreTypes(["Instant"], {
      id: 8,
      card_id: 8,
      name: "Lightning Bolt",
      zone: "Stack",
    });

    const counterspell = buildGameObjectWithCoreTypes(["Instant"], {
      id: 9,
      card_id: 9,
      name: "Counterspell",
      zone: "Stack",
    });

    const gameState = createGameState({
      objects: {
        "8": stackSpellTarget,
        "9": counterspell,
      },
      waiting_for: {
        type: "TargetSelection",
        data: {
          player: 0,
          pending_cast: {
            object_id: 9,
            card_id: 9,
            ability: { targets: [] },
            cost: { type: "NoCost" },
          },
          target_slots: [{
            legal_targets: [{ Object: 8 }],
            optional: false,
          }],
          selection: { current_slot: 0, current_legal_targets: [{ Object: 8 }] },
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.getByText("a spell")).toBeInTheDocument();
  });

  it("informs the player when the target slot is up to one nonland permanent", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const nonLandTarget = buildGameObject({
      id: 7,
      card_id: 7,
      name: "Nonland Artifact",
    });

    const sourceObject = buildGameObject({
      id: 9,
      name: "Deceit",
    });

    const gameState = createGameState({
      objects: {
        "7": nonLandTarget,
        "9": sourceObject,
      },
      waiting_for: {
        type: "TriggerTargetSelection",
        data: {
          player: 0,
          target_slots: [{
            legal_targets: [{ Object: 7 }],
            optional: true,
          }],
          selection: { current_slot: 0, current_legal_targets: [{ Object: 7 }] },
          source_id: 9,
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.getByText("up to one nonland permanent")).toBeInTheDocument();
  });

  it("shows Keep Current Targets button for CopyRetarget and dispatches KeepAllCopyTargets", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      waiting_for: {
        type: "CopyRetarget",
        data: {
          player: 0,
          copy_id: 233,
          target_slots: [
            { current: { Player: 0 }, legal_alternatives: [{ Player: 0 }, { Player: 1 }] },
          ],
          current_slot: 0,
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    const btn = screen.getByRole("button", { name: "Keep Current Targets" });
    expect(btn).toBeInTheDocument();
    fireEvent.click(btn);

    expect(dispatch).toHaveBeenCalledWith({
      type: "KeepAllCopyTargets",
    });
  });

  it("hides Keep Current Targets button when the copy has no current target", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      waiting_for: {
        type: "CopyRetarget",
        data: {
          player: 0,
          copy_id: 231,
          target_slots: [
            {
              legal_alternatives: [{ Object: 61 }, { Object: 91 }],
            },
          ],
          current_slot: 0,
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.queryByRole("button", { name: "Keep Current Targets" })).toBeNull();
  });

  it("renders mana symbols in trigger descriptions", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const sourceObject = buildGameObjectWithCoreTypes(["Instant"], {
      id: 9,
      card_id: 9,
      name: "Deceit",
      color: ["Blue"],
      base_color: ["Blue"],
    });

    const gameState = createGameState({
      objects: {
        "9": sourceObject,
      },
      waiting_for: {
        type: "TriggerTargetSelection",
        data: {
          player: 0,
          target_slots: [{ legal_targets: [{ Player: 1 }], optional: true }],
          selection: { current_slot: 0, current_legal_targets: [{ Player: 1 }] },
          source_id: 9,
          description: "~ costs {U}{U}",
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.getByText("up to one player")).toBeInTheDocument();
    expect(screen.getAllByAltText("U")).toHaveLength(2);
  });

  it("prefixes the prompt with the active slot's mode label when present", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      waiting_for: {
        type: "TriggerTargetSelection",
        data: {
          player: 0,
          target_slots: [{ legal_targets: [{ Player: 1 }], optional: false }],
          mode_labels: ["Deal 2 damage to any target."],
          selection: { current_slot: 0, current_legal_targets: [{ Player: 1 }] },
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(
      screen.getByText("Deal 2 damage to any target. — a player"),
    ).toBeInTheDocument();
  });

  it("renders mana symbols and source names in active mode labels", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const sourceObject = buildGameObjectWithCoreTypes(["Instant"], {
      id: 9,
      card_id: 9,
      name: "Kozilek's Command",
      color: [],
      base_color: [],
    });
    const gameState = createGameState({
      objects: {
        "9": sourceObject,
      },
      waiting_for: {
        type: "TargetSelection",
        data: {
          player: 0,
          pending_cast: {
            object_id: 9,
            card_id: 9,
            ability: { targets: [] },
            cost: { type: "NoCost" },
          },
          target_slots: [{ legal_targets: [{ Player: 1 }], optional: false }],
          mode_labels: ["Target player creates a token with \"Sacrifice ~: Add {C}.\""],
          selection: { current_slot: 0, current_legal_targets: [{ Player: 1 }] },
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.getByText(/Sacrifice Kozilek's Command: Add/)).toBeInTheDocument();
    expect(screen.getByAltText("C")).toBeInTheDocument();
    expect(screen.queryByText(/Sacrifice ~:/)).toBeNull();
  });

  it("renders the populate creature-token prompt", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      waiting_for: {
        type: "PopulateChoice",
        data: { player: 0, source_id: 1, valid_tokens: [10, 11] },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(
      screen.getByText("Choose a creature token to populate"),
    ).toBeInTheDocument();
  });

  it("renders the plain prompt when no mode label is present", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      waiting_for: {
        type: "TriggerTargetSelection",
        data: {
          player: 0,
          target_slots: [{ legal_targets: [{ Player: 1 }], optional: false }],
          selection: { current_slot: 0, current_legal_targets: [{ Player: 1 }] },
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.getByText("a player")).toBeInTheDocument();
    expect(screen.queryByText(/—/)).toBeNull();
  });

  // Regression for issue #3681 (Inferno Titan): a trigger that divides an effect
  // among "one, two, or three targets" surfaces multiple slots. The prompt must
  // report progress ("Choose target 1 of 3") instead of always reading
  // "a creature", which misled players into selecting only one target.
  it("shows 'Choose target N of M' for a multi-slot trigger (divide among targets)", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const bear = buildGameObjectWithCoreTypes(["Creature"], { id: 7, name: "Bear" });
    const elf = buildGameObjectWithCoreTypes(["Creature"], { id: 8, name: "Elf" });
    const titan = buildGameObjectWithCoreTypes(["Creature"], { id: 9, name: "Inferno Titan" });
    const legal = [{ Object: 7 }, { Object: 8 }, { Object: 9 }, { Player: 1 }];

    const gameState = createGameState({
      objects: { "7": bear, "8": elf, "9": titan },
      waiting_for: {
        type: "TriggerTargetSelection",
        data: {
          player: 0,
          target_slots: [
            { legal_targets: legal, optional: false },
            { legal_targets: legal, optional: true },
            { legal_targets: legal, optional: true },
          ],
          selection: { current_slot: 0, current_legal_targets: legal },
          source_id: 9,
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.getByText("Choose target 1 of 3")).toBeInTheDocument();
    expect(screen.queryByText("a creature")).toBeNull();
  });

  it("advances the slot progress as each target is chosen for a multi-slot trigger", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const legal = [{ Object: 7 }, { Object: 8 }, { Player: 1 }];

    const gameState = createGameState({
      objects: {
        "7": buildGameObjectWithCoreTypes(["Creature"], { id: 7, name: "Bear" }),
        "8": buildGameObjectWithCoreTypes(["Creature"], { id: 8, name: "Elf" }),
      },
      waiting_for: {
        type: "TriggerTargetSelection",
        data: {
          player: 0,
          target_slots: [
            { legal_targets: legal, optional: false },
            { legal_targets: legal, optional: true },
            { legal_targets: legal, optional: true },
          ],
          selection: { current_slot: 1, current_legal_targets: legal },
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.getByText("Choose target 2 of 3")).toBeInTheDocument();
  });
});
