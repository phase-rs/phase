import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { StackEntry } from "../StackEntry.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import type { GameState, StackEntry as StackEntryType } from "../../../adapter/types.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildChooseXValueWaitingFor,
  buildGameState,
  buildPendingCast,
  buildStackEntry,
} from "../../../test/factories/gameStateFactory.ts";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "/test-card.png", isLoading: false }),
}));

const { dispatchActionMock } = vi.hoisted(() => ({ dispatchActionMock: vi.fn() }));
vi.mock("../../../game/dispatch.ts", () => ({ dispatchAction: dispatchActionMock }));

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return buildGameState({
    next_object_id: 100,
    next_timestamp: 1,
    ...overrides,
  });
}

describe("StackEntry", () => {
  beforeEach(() => {
    useGameStore.getState().reset();
    dispatchActionMock.mockClear();
  });

  afterEach(() => {
    cleanup();
  });

  it("renders the live pending_cast cost for an in-flight X spell instead of the printed base cost", () => {
    const entry: StackEntryType = buildStackEntry({
      id: 77,
      source_id: 42,
      controller: 0,
      kind: {
        type: "Spell",
        data: {
          card_id: 1,
          actual_mana_spent: 0,
        },
      },
    });
    const pendingCast = buildPendingCast({
      object_id: 42,
      card_id: 1,
      ability: { targets: [] },
      cost: { type: "Cost", shards: ["X", "Red", "Red"], generic: 0 },
    });

    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({
          id: 42,
          card_id: 1,
          name: "Crackle with Power",
          zone: "Stack",
          mana_cost: { type: "Cost", shards: ["X", "Red", "Red"], generic: 2 },
          card_types: { core_types: ["Sorcery"], subtypes: [], supertypes: [] },
          color: ["Red"],
          base_color: ["Red"],
        }),
      ),
      stack: [entry],
      waiting_for: buildChooseXValueWaitingFor({
        data: {
          player: 0,
          min: 0,
          max: 3,
          pending_cast: pendingCast,
        },
      }),
      has_pending_cast: true,
      pending_cast: pendingCast,
    });

    act(() => {
      useGameStore.setState({
        gameState,
        waitingFor: gameState.waiting_for,
      });
    });

    render(
      <StackEntry
        entry={entry}
        index={0}
        isTop
        isPending
        cardSize={{ width: 120, height: 168 }}
      />,
    );

    expect(screen.getByAltText("X")).toBeInTheDocument();
    expect(screen.getAllByAltText("R")).toHaveLength(2);
    expect(screen.queryByAltText("2")).not.toBeInTheDocument();
  });

  it("offers Revoke for an AllCopies yield after the source token has ceased", () => {
    // CR 400.7 + CR 704.5d: a ceased token is gone from `objects`, so the entry
    // has no live source object to read a card_id from — the menu must match the
    // standing AllCopies yield via the engine-stamped `source_card_id` instead.
    const entry: StackEntryType = buildStackEntry({
      id: 77,
      source_id: 42,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: {
          source_id: 42,
          ability: { targets: [], source_card_id: 7 },
          source_name: "Ophiomancer",
        },
      },
    });
    const gameState = createGameState({
      objects: {},
      stack: [entry],
      priority_yields: [{ player: 0, target: { AllCopies: { card_id: 7 } } }],
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(
      <StackEntry entry={entry} index={0} isTop isPending cardSize={{ width: 120, height: 168 }} />,
    );

    // The always-visible yield button opens the menu on a plain tap — no hidden
    // long-press. Its label reflects the standing yield ("Auto-passing…").
    fireEvent.click(screen.getByRole("button", { name: /auto-pass/i }));

    expect(screen.getByText("Revoke")).toBeInTheDocument();
  });

  it("shows a discoverable yield button on a triggered ability and opens the menu on tap", () => {
    const entry: StackEntryType = buildStackEntry({
      id: 88,
      source_id: 50,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: {
          source_id: 50,
          ability: { targets: [], source_card_id: 9 },
          source_name: "Bloodghast",
        },
      },
    });
    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({ id: 50, card_id: 9, name: "Bloodghast", zone: "Stack" }),
      ),
      stack: [entry],
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(<StackEntry entry={entry} index={0} isTop cardSize={{ width: 120, height: 168 }} />);

    // Discoverable: the control is present with no hidden gesture, and the menu
    // stays closed until the button is tapped.
    const button = screen.getByRole("button", { name: /auto-pass/i });
    expect(screen.queryByText("Only this one")).not.toBeInTheDocument();

    fireEvent.click(button);

    expect(screen.getByText("Only this one")).toBeInTheDocument();
    expect(screen.getByText("All copies")).toBeInTheDocument();
  });

  it("dispatches a scoped SetPriorityYield when a menu option is chosen", () => {
    const entry: StackEntryType = buildStackEntry({
      id: 88,
      source_id: 50,
      controller: 0,
      kind: {
        type: "TriggeredAbility",
        data: {
          source_id: 50,
          ability: { targets: [], source_card_id: 9 },
          source_name: "Bloodghast",
        },
      },
    });
    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({ id: 50, card_id: 9, name: "Bloodghast", zone: "Stack" }),
      ),
      stack: [entry],
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(<StackEntry entry={entry} index={0} isTop cardSize={{ width: 120, height: 168 }} />);

    fireEvent.click(screen.getByRole("button", { name: /auto-pass/i }));

    // Realistic pointer sequence: pointerdown fires PopoverMenu's window-level
    // outside-click listener BEFORE the click. If that listener wrongly treats a
    // menu-internal press as "outside" and closes the menu, the option unmounts
    // and its onClick never runs — the exact "pressing does nothing" symptom.
    const option = screen.getByText("All copies");
    fireEvent.pointerDown(option);
    expect(screen.getByText("All copies")).toBeInTheDocument(); // menu stayed open
    fireEvent.click(option);

    expect(dispatchActionMock).toHaveBeenCalledWith({
      type: "SetPriorityYield",
      data: { op: { type: "Add", data: { source_id: 50, scope: "AllCopies" } } },
    });

    // Observable behavior, not just that the handler ran: choosing an option
    // dismisses the menu. (The dispatch firing is necessary but not sufficient —
    // "the menu stays open" is a distinct, user-visible failure.)
    expect(screen.queryByText("All copies")).not.toBeInTheDocument();
  });

  it("does not render the yield button on a spell entry", () => {
    const entry: StackEntryType = buildStackEntry({
      id: 99,
      source_id: 60,
      controller: 0,
      kind: { type: "Spell", data: { card_id: 3, actual_mana_spent: 0 } },
    });
    const gameState = createGameState({
      objects: buildObjectMap(
        buildGameObject({ id: 60, card_id: 3, name: "Lightning Bolt", zone: "Stack" }),
      ),
      stack: [entry],
    });

    act(() => {
      useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
    });

    render(<StackEntry entry={entry} index={0} isTop cardSize={{ width: 120, height: 168 }} />);

    expect(screen.queryByRole("button", { name: /auto-pass/i })).not.toBeInTheDocument();
  });
});
