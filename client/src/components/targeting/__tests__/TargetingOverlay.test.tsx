import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import type { GameState } from "../../../adapter/types.ts";
import { buildGameObject, buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildPendingCast,
  buildTargetSelectionProgress,
  buildTargetSelectionSlot,
  buildTargetSelectionWaitingFor,
  buildTriggerTargetSelectionWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { TargetingOverlay } from "../TargetingOverlay.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return buildGameState({
    waiting_for: buildTriggerTargetSelectionWaitingFor({
      data: {
        player: 0,
        target_slots: [buildTargetSelectionSlot({ legal_targets: [{ Player: 1 }] })],
        selection: buildTargetSelectionProgress({
          current_legal_targets: [{ Player: 1 }],
        }),
      },
    }),
    ...overrides,
  });
}

describe("TargetingOverlay", () => {
  beforeEach(() => {
    act(() => {
      // `playerNames` is reset too: the overlay's player-target controls label
      // themselves from it, so a name left behind by another suite would change
      // the button text under these tests.
      useMultiplayerStore.setState({ activePlayerId: 0, playerNames: new Map() });
    });
  });

  afterEach(() => {
    cleanup();
  });

  // This inverts a shipped decision. The overlay used to render NO player target
  // controls, leaving the PlayerHud/OpponentHud seat glow as the only way to
  // commit one — so a player asked for a player target had nothing to click
  // inside the overlay and no statement of where to click. The seat glow stays
  // as an equivalent second path; this asserts the overlay path exists.
  it("commits a legal player target in one click from the overlay", () => {
    const dispatch = vi.fn().mockResolvedValue([]);

    act(() => {
      useGameStore.setState({
        gameState: createGameState(),
        waitingFor: createGameState().waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.getByText("a player")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Choose: Opp 2" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseTarget",
      data: { target: { Player: 1 } },
    });
  });

  it("uses the native keep-tapped skip action for an untap decision", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const permanent = buildGameObject({ id: 10, zone: "Battlefield", tapped: true });
    const gameState = createGameState({
      objects: buildObjectMap(permanent),
      battlefield: [10],
      waiting_for: { type: "UntapChoice", data: { player: 0, candidates: [10] } },
    });
    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for, dispatch });
    });

    render(<TargetingOverlay />);

    fireEvent.click(screen.getByRole("button", { name: "Keep tapped" }));
    expect(dispatch).toHaveBeenCalledWith({
      type: "ChooseUntap",
      data: { object_id: 10, untap: false },
    });
  });

  it("labels an 'of an opponent's choice' slot for the announcing viewer", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    // CR 601.2c: the engine routes the prompt to the slot's announcer, who is the
    // local viewer of this overlay — so `chooser` equals the local player. The
    // hint must still render (the prior `chooser !== localPlayerId` guard hid it).
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
          target_slots: [
            { legal_targets: [{ Player: 1 }], optional: false, chooser: 0 },
          ],
          selection: {
            current_slot: 0,
            current_legal_targets: [{ Player: 1 }],
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

    expect(screen.getByText("(opponent's choice)")).toBeInTheDocument();
  });

  it("dispatches null target when the active engine slot is optional and skipped", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      waiting_for: {
        type: "TargetSelection",
        data: {
          player: 0,
          pending_cast: buildPendingCast({ object_id: 5, card_id: 10 }),
          target_slots: [buildTargetSelectionSlot({ optional: true })],
          selection: buildTargetSelectionProgress(),
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
      objects: buildObjectMap(
        buildGameObjectWithCoreTypes(["Creature"], {
          id: 7,
          name: "Memnite",
        }),
      ),
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
      objects: buildObjectMap(
        buildGameObjectWithCoreTypes(["Land"], {
          id: 4,
          name: "Holdout Settlement",
        }),
        buildGameObjectWithCoreTypes(["Creature"], {
          id: 7,
          name: "Memnite",
        }),
      ),
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
      objects: buildObjectMap(
        buildGameObjectWithCoreTypes(["Artifact"], {
          id: 20,
          name: "Vehicle",
        }),
        buildGameObjectWithCoreTypes(["Creature"], {
          id: 21,
          name: "Pilot One",
          power: 2,
        }),
        buildGameObjectWithCoreTypes(["Creature"], {
          id: 22,
          name: "Pilot Two",
          power: 3,
        }),
      ),
      waiting_for: {
        type: "CrewVehicle",
        data: {
          player: 0,
          vehicle_id: 20,
          crew_power: 4,
          eligible_creatures: [21, 22],
          contributions: [2, 3],
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

  it("cancels crew selection back to priority", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObjectWithCoreTypes(["Artifact"], {
          id: 20,
          name: "Vehicle",
        }),
        buildGameObjectWithCoreTypes(["Creature"], {
          id: 21,
          name: "Pilot One",
          power: 2,
        }),
      ),
      waiting_for: {
        type: "CrewVehicle",
        data: {
          player: 0,
          vehicle_id: 20,
          crew_power: 2,
          eligible_creatures: [21],
          contributions: [2],
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
      objects: buildObjectMap(stackSpellTarget, counterspell),
      waiting_for: buildTargetSelectionWaitingFor({
        data: {
          player: 0,
          pending_cast: buildPendingCast({ object_id: 9, card_id: 9 }),
          target_slots: [buildTargetSelectionSlot({ legal_targets: [{ Object: 8 }] })],
          selection: buildTargetSelectionProgress({ current_legal_targets: [{ Object: 8 }] }),
        },
      }),
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
      objects: buildObjectMap(nonLandTarget, sourceObject),
      waiting_for: buildTriggerTargetSelectionWaitingFor({
        data: {
          player: 0,
          target_slots: [buildTargetSelectionSlot({ legal_targets: [{ Object: 7 }], optional: true })],
          selection: buildTargetSelectionProgress({ current_legal_targets: [{ Object: 7 }] }),
          source_id: 9,
        },
      }),
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
      objects: buildObjectMap(sourceObject),
      waiting_for: buildTriggerTargetSelectionWaitingFor({
        data: {
          player: 0,
          target_slots: [buildTargetSelectionSlot({ legal_targets: [{ Player: 1 }], optional: true })],
          selection: buildTargetSelectionProgress({ current_legal_targets: [{ Player: 1 }] }),
          source_id: 9,
          description: "~ costs {U}{U}",
        },
      }),
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
    // The disclosure's accessible name has to say what the visible line says.
    // `RichLabel` renders {U} as a glyph whose alt text is the bare shard, so a
    // raw label would announce brace notation where the player sees a symbol.
    expect(
      screen.getByRole("button", { name: "Show the full description: Deceit costs UU" }),
    ).toBeInTheDocument();
  });

  it("shows the active trigger damage amount during target selection", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState({
      waiting_for: buildTriggerTargetSelectionWaitingFor({
        data: {
          player: 0,
          trigger_controller: 0,
          trigger_event: {
            type: "DamageDealt",
            data: { source_id: 9, target: { Object: 7 }, amount: 3, is_combat: true },
          },
          target_slots: [buildTargetSelectionSlot({ legal_targets: [{ Object: 7 }] })],
          selection: buildTargetSelectionProgress({ current_legal_targets: [{ Object: 7 }] }),
        },
      }),
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    expect(screen.getByText("This trigger: 3 damage")).toBeInTheDocument();
  });

  it("shows the active slot's mode label beside the instruction, not inside it", () => {
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

    // Two elements, not one string: the instruction names only what to click,
    // and the mode label qualifies it from the caption line below.
    expect(screen.getByText("a player")).toBeInTheDocument();
    expect(screen.getByText("Deal 2 damage to any target.")).toBeInTheDocument();
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

  // Regression for the Diluvian Primordial report. `mode_labels` carries raw
  // engine oracle text — full sentences — and while that text shared the
  // instruction's line, every modal prompt rendered two lines and the block
  // grew down over the opponent-HUD tab rail. What this pins is that the
  // instruction line holds the localized noun and slot counter ALONE, so no
  // mode label, however long, can change how tall the instruction is. The
  // label keeps its place on the caption line, ahead of the engine
  // description: that line elides from the tail, and the description is the
  // one entry the disclosure beside it opens in full anyway.
  //
  // The height bound itself is NOT asserted here. jsdom has no layout engine,
  // so it is verified by browser measurement instead (see the block's comment
  // in TargetingOverlay.tsx for the measured figures per viewport).
  it("keeps a long mode label off the instruction line", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const modeLabel =
      "Target player creates X 0/1 colorless Eldrazi Spawn creature tokens with "
      + "\"Sacrifice this token: Add {C}.\"";
    const legal = [{ Player: 0 }, { Player: 1 }];
    const sourceObject = buildGameObjectWithCoreTypes(["Instant"], {
      id: 9,
      card_id: 9,
      name: "Kozilek's Command",
      color: [],
      base_color: [],
    });

    const gameState = createGameState({
      objects: { "9": sourceObject },
      waiting_for: {
        type: "TargetSelection",
        data: {
          player: 0,
          pending_cast: {
            object_id: 9,
            card_id: 9,
            ability: {
              targets: [],
              description: "Choose two — this spell does several things at once.",
            },
            cost: { type: "NoCost" },
          },
          target_slots: [
            { legal_targets: legal, optional: false },
            { legal_targets: legal, optional: false },
          ],
          mode_labels: [modeLabel, null],
          selection: { current_slot: 0, current_legal_targets: legal },
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

    // An exact match, so it fails if any mode prose rejoins this line.
    expect(screen.getByText("a player")).toBeInTheDocument();
    const label = screen.getByText(/^Target player creates X 0\/1 colorless Eldrazi Spawn/);
    expect(label).toBeInTheDocument();
    // Ahead of the description on the caption line, because that line elides
    // from the tail and only the description has a disclosure that restores it.
    expect(label.parentElement?.textContent).toMatch(
      /Target player creates[\s\S]*Choose two — this spell does several things/,
    );
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
  // report progress ("1 of 3") instead of naming only the kind, which misled
  // players into selecting only one target. The kind stays primary — dropping it
  // left the player told how many picks remain but not what to pick.
  //
  // The two live on different lines now (the counter appended to the longest
  // localized nouns forced a second instruction line at phone widths), so both
  // are asserted separately. Neither may be dropped: the kind says what to
  // pick, the progress says that picking one is not finishing.
  it("names the target kind and the slot progress for a multi-slot trigger", () => {
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

    expect(screen.getByText("a nonland permanent")).toBeInTheDocument();
    expect(screen.getByText("Target 1 of 3")).toBeInTheDocument();
    expect(screen.queryByText("a creature")).toBeNull();
  });

  // The active slot accepts only players, so the prompt has to say so. Before
  // the multi-slot arm carried the noun it read "Choose target 1 of 2" — a
  // player asked for a player target was told how many picks remained but never
  // what kind of thing to pick.
  it("names the player kind for a multi-slot trigger whose slot takes only players", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const legal = [{ Player: 0 }, { Player: 1 }];

    const gameState = createGameState({
      waiting_for: {
        type: "TriggerTargetSelection",
        data: {
          player: 0,
          target_slots: [
            { legal_targets: legal, optional: false },
            { legal_targets: legal, optional: false },
          ],
          selection: { current_slot: 0, current_legal_targets: legal },
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
    expect(screen.getByText("Target 1 of 2")).toBeInTheDocument();
  });

  it("collapses a long engine description until the player expands it", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const description =
      "Whenever this creature attacks, choose target player. That player loses "
      + "2 life unless they sacrifice a nonland permanent or discard a card, and "
      + "if they do neither you draw a card at the beginning of the next end step.";

    const gameState = createGameState({
      waiting_for: buildTriggerTargetSelectionWaitingFor({
        data: {
          player: 0,
          target_slots: [buildTargetSelectionSlot({ legal_targets: [{ Player: 1 }] })],
          selection: buildTargetSelectionProgress({ current_legal_targets: [{ Player: 1 }] }),
          description,
        },
      }),
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
      });
    });

    render(<TargetingOverlay />);

    // Named by the action it performs, not by the ability prose. Without the
    // explicit label the button's text content wins the accessible-name
    // computation and a screen reader announces the whole description where
    // the action belongs.
    const disclosure = screen.getByRole("button", {
      name: `Show the full description: ${description}`,
    });
    // Collapsed, the description exists once: as the tail of the caption line,
    // where it is elided rather than readable.
    expect(screen.getAllByText(description)).toHaveLength(1);

    fireEvent.click(disclosure);
    expect(screen.getByRole("button", { name: `Show less: ${description}` })).toBe(disclosure);
    // TWO, not one: the caption-line copy stays put and the expanded panel adds
    // a second, readable copy. This count is what makes the test able to fail —
    // asserting only `aria-expanded` passes unchanged if the panel is deleted,
    // which is exactly the "the disclosure reveals nothing" defect this
    // affordance already regressed into once.
    expect(screen.getAllByText(description)).toHaveLength(2);

    fireEvent.click(disclosure);
    expect(screen.getByRole("button", { expanded: false })).toBe(disclosure);
    expect(screen.getAllByText(description)).toHaveLength(1);
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

    // KNOWN DEFERRED DEFECT, pinned deliberately (issue #7692): the slot's
    // legal targets are two creatures AND a player, but `inferTargetNoun` drops
    // the player from a mixed slot, so the prompt names a kind that excludes a
    // legal target while the same render puts a "Choose: Opp 2" button on screen
    // from the identical `current_legal_targets`. Fixing the inference is out of
    // scope here. The NOUN on the next line is what has to change when it is
    // fixed — do not read it as the intended wording, and do not close the
    // follow-up against it.
    expect(screen.getByText("a nonland permanent")).toBeInTheDocument();
    // Not part of that defect. The slot progress is correct; it is asserted on
    // its own only because it lives on the caption line now rather than inside
    // the instruction string.
    expect(screen.getByText("Target 2 of 3")).toBeInTheDocument();
  });
});
