import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

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
});
