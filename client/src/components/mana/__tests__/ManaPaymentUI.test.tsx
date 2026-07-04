import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { ManaPaymentUI } from "../ManaPaymentUI";
import { useGameStore } from "../../../stores/gameStore";
import type { GameState } from "../../../adapter/types";
import { buildGameObjectWithCoreTypes, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildManaPaymentWaitingFor,
  buildPendingCast,
  buildStackEntry,
} from "../../../test/factories/gameStateFactory.ts";

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return buildGameState({
    waiting_for: buildManaPaymentWaitingFor(),
    ...overrides,
  });
}

function stackSpellObject(
  id: number,
  name: string,
  manaCost: GameState["objects"][number]["mana_cost"],
  coreTypes: string[] = ["Instant"],
) {
  return buildGameObjectWithCoreTypes(coreTypes, {
    id,
    card_id: id,
    name,
    mana_cost: manaCost,
    zone: "Stack",
  });
}

function spellStackEntry(objectId: number) {
  return buildStackEntry({
    id: objectId,
    source_id: objectId,
    controller: 0,
    kind: {
      type: "Spell",
      data: {
        card_id: objectId,
        ability: undefined,
        actual_mana_spent: 0,
      },
    },
  });
}

describe("ManaPaymentUI", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders cancel during mana payment when no top-stack spell cost can be inferred", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const gameState = createGameState();

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    expect(screen.getByText("Payment is still pending. Tap permanents or cancel this action.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(dispatch).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it("shows the convoke payment hint during convoke mana payment", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = stackSpellObject(
      300,
      "Venerated Loxodon",
      { type: "Cost", shards: ["White"], generic: 4 },
      ["Creature"],
    );
    const gameState = createGameState({
      objects: buildObjectMap(spellObj),
      stack: [spellStackEntry(300)],
      waiting_for: buildManaPaymentWaitingFor({
        data: { player: 0, convoke_mode: "Convoke" },
      }),
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    const { container } = render(<ManaPaymentUI />);

    expect(screen.getByText("Tap creatures to help pay.")).toBeInTheDocument();
    const outerShell = container.querySelector(".pointer-events-none.fixed");
    expect(outerShell).not.toBeNull();
    expect(outerShell?.querySelector(".pointer-events-auto")).not.toBeNull();
  });

  it("shows the delve payment hint during delve mana payment", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = stackSpellObject(
      301,
      "Dig Through Time",
      { type: "Cost", shards: ["Blue", "Blue"], generic: 6 },
    );
    const gameState = createGameState({
      objects: buildObjectMap(spellObj),
      stack: [spellStackEntry(301)],
      waiting_for: buildManaPaymentWaitingFor({
        data: { player: 0, convoke_mode: "Delve" },
      }),
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    expect(
      screen.getByText("Exile cards from your graveyard to help pay."),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("Tap creatures or artifacts to help pay."),
    ).not.toBeInTheDocument();
  });

  // CR 107.4f + CR 601.2f: When the engine reports PhyrexianPayment, clicking Pay
  // dispatches SubmitPhyrexianChoices with one choice per shard (default: PayMana).
  it("dispatches SubmitPhyrexianChoices with defaults for PhyrexianPayment", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = stackSpellObject(
      100,
      "Gitaxian Probe",
      { type: "Cost", shards: ["PhyrexianBlue"], generic: 0 },
    );

    const gameState = createGameState({
      objects: buildObjectMap(spellObj),
      stack: [spellStackEntry(100)],
      waiting_for: {
        type: "PhyrexianPayment",
        data: {
          player: 0,
          spell_object: 100,
          shards: [
            {
              shard_index: 0,
              color: "Blue",
              options: { type: "ManaOrLife" },
            },
          ],
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [
          { type: "CancelCast" },
          {
            type: "SubmitPhyrexianChoices",
            data: { choices: [{ type: "PayMana" }] },
          },
        ],
      });
    });

    render(<ManaPaymentUI />);
    fireEvent.click(screen.getByRole("button", { name: "Pay" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "SubmitPhyrexianChoices",
      data: { choices: [{ type: "PayMana" }] },
    });
  });

  // Issue #457 — CR 601.2f: ManaPaymentUI must display the engine-resolved
  // locked-in cost (`pending_cast.cost`), not the printed base `mana_cost`.
  // Call the Coppercoats is a Strive spell; with multiple target opponents the
  // engine inflates {2}{W} to {4}{W}{W}{W}. The panel must show the inflated total.
  it("displays the Strive-inflated pending_cast cost, not the printed mana_cost", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = stackSpellObject(
      400,
      "Call the Coppercoats",
      { type: "Cost", shards: ["White"], generic: 2 },
    );
    const gameState = createGameState({
      objects: buildObjectMap(spellObj),
      stack: [spellStackEntry(400)],
      // Engine-resolved locked-in total: {2}{W} + 2 × {1}{W} Strive surcharge.
      pending_cast: buildPendingCast({
        object_id: 400,
        cost: { type: "Cost", shards: ["White", "White", "White"], generic: 4 },
      }),
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    // ManaSymbol renders each shard as an <img> with alt={shard}. The inflated
    // total {4}{W}{W}{W} → generic shard "4" plus three "W" shards.
    expect(screen.getByAltText("4")).toBeInTheDocument();
    expect(screen.getAllByAltText("W")).toHaveLength(3);
    // The base printed cost generic of 2 must NOT appear.
    expect(screen.queryByAltText("2")).not.toBeInTheDocument();
  });

  // Regression guard — no Strive, no statics: pending_cast.cost equals the
  // printed mana_cost, so the panel renders the unchanged base cost.
  it("renders the base cost when pending_cast.cost equals the printed mana_cost", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = stackSpellObject(
      500,
      "Plain Spell",
      { type: "Cost", shards: ["White"], generic: 2 },
    );
    const gameState = createGameState({
      objects: buildObjectMap(spellObj),
      stack: [spellStackEntry(500)],
      pending_cast: buildPendingCast({
        object_id: 500,
        cost: { type: "Cost", shards: ["White"], generic: 2 },
      }),
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    expect(screen.getByAltText("2")).toBeInTheDocument();
    expect(screen.getByAltText("W")).toBeInTheDocument();
  });

  it("displays activated-ability mana cost from pending_cast.activation_cost when present", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const sourceObj = buildGameObjectWithCoreTypes(["Artifact"], {
      id: 700,
      name: "The Reality Chip",
      card_id: 7,
      mana_cost: { type: "Cost", shards: ["Blue"], generic: 2 },
      zone: "Battlefield",
      card_types: { core_types: ["Artifact"], subtypes: ["Equipment"], supertypes: [] },
    });

    const gameState = createGameState({
      objects: buildObjectMap(sourceObj),
      pending_cast: buildPendingCast({
        object_id: 700,
        // Spells use `cost`; activated abilities use `activation_cost`.
        cost: { type: "NoCost" },
        activation_cost: {
          type: "Mana",
          cost: { type: "Cost", shards: ["Blue"], generic: 2 },
        },
        activation_ability_index: 0,
      }),
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    // Should display activation cost {2}{U}.
    expect(screen.getByAltText("2")).toBeInTheDocument();
    expect(screen.getByAltText("U")).toBeInTheDocument();
  });

  // pending_cast absent — fall back to the stack spell object's mana_cost.
  it("falls back to the stack spell object mana_cost when pending_cast is absent", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = stackSpellObject(
      600,
      "Fallback Spell",
      { type: "Cost", shards: ["Blue"], generic: 3 },
    );
    const gameState = createGameState({
      objects: buildObjectMap(spellObj),
      stack: [spellStackEntry(600)],
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [{ type: "CancelCast" }, { type: "PassPriority" }],
      });
    });

    render(<ManaPaymentUI />);

    expect(screen.getByAltText("3")).toBeInTheDocument();
    expect(screen.getByAltText("U")).toBeInTheDocument();
  });

  // CR 107.4f: With PayLife toggled on a ManaOrLife shard, dispatch carries PayLife.
  it("dispatches PayLife when the shard toggle is flipped", () => {
    const dispatch = vi.fn().mockResolvedValue([]);
    const spellObj = stackSpellObject(
      200,
      "Dismember",
      {
        type: "Cost",
        shards: ["PhyrexianBlack", "PhyrexianBlack", "PhyrexianBlack"],
        generic: 1,
      },
    );

    const gameState = createGameState({
      objects: buildObjectMap(spellObj),
      stack: [spellStackEntry(200)],
      waiting_for: {
        type: "PhyrexianPayment",
        data: {
          player: 0,
          spell_object: 200,
          shards: [
            {
              shard_index: 0,
              color: "Black",
              options: { type: "ManaOrLife" },
            },
            {
              shard_index: 1,
              color: "Black",
              options: { type: "ManaOrLife" },
            },
            {
              shard_index: 2,
              color: "Black",
              options: { type: "ManaOrLife" },
            },
          ],
        },
      },
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
        dispatch,
        legalActions: [],
      });
    });

    render(<ManaPaymentUI />);

    // Three Phyrexian toggle buttons plus Pay and Cancel. Pick the first toggle
    // by matching the gray-800 background (unselected mana state).
    const allButtons = screen.getAllByRole("button");
    const toggles = allButtons.filter((b) =>
      b.className.includes("bg-gray-800"),
    );
    expect(toggles.length).toBe(3);
    // Click the first Phyrexian toggle (defaults to mana); flips to PayLife.
    fireEvent.click(toggles[0]);

    fireEvent.click(screen.getByRole("button", { name: "Pay" }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "SubmitPhyrexianChoices",
      data: {
        choices: [
          { type: "PayLife" },
          { type: "PayMana" },
          { type: "PayMana" },
        ],
      },
    });
  });
});
