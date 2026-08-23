import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObject } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayer } from "../../../test/factories/gameStateFactory.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

function makeObject(id: number, name: string): GameObject {
  return buildGameObject({
    id,
    card_id: id,
    zone: "Library",
    name,
    card_types: { supertypes: [], core_types: ["Instant"], subtypes: [] },
    mana_cost: { type: "Cost", shards: [], generic: 1 },
    timestamp: id,
  });
}

describe("SearchChoice modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("shows selectable cards first and disables ineligible cards", () => {
    const waitingFor: WaitingFor = {
      type: "SearchChoice",
      data: { player: 0, cards: [42], count: 1 },
    };
    const state = buildGameState({
      players: [buildPlayer({ id: 0, library: [42, 43] }), buildPlayer({ id: 1 })],
      objects: {
        42: makeObject(42, "Eligible Card"),
        43: makeObject(43, "Ineligible Card"),
      },
      waiting_for: waitingFor,
      active_library_searches: {
        0: {
          searcher: 0,
          searched_zone_owner: 0,
          effective_library_owner: 0,
          learned_audience: [0],
          looked_at: [
            [0, "Library", { object_id: 43, incarnation: 0 }],
            [0, "Library", { object_id: 42, incarnation: 0 }],
          ],
        },
      },
    });
    useGameStore.setState({ gameMode: "online", gameState: state, waitingFor });

    render(<CardChoiceModal />);

    const eligibleCard = screen.getByLabelText(/Eligible Card/);
    expect(eligibleCard).toBeInTheDocument();
    const ineligibleCard = screen.getByLabelText(/Ineligible Card/);
    expect(ineligibleCard.closest("button")).toBeDisabled();
    expect(
      eligibleCard.compareDocumentPosition(ineligibleCard) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("labels ordered search selections by their chosen library position", () => {
    const cardIds = [42, 43, 44, 45, 46];
    const waitingFor: WaitingFor = {
      type: "SearchChoice",
      data: {
        player: 0,
        cards: cardIds,
        count: cardIds.length,
        ordering_hint: "OrderedToLibraryTop",
      },
    };
    const state = buildGameState({
      players: [buildPlayer({ id: 0, library: cardIds }), buildPlayer({ id: 1 })],
      objects: Object.fromEntries(
        cardIds.map((id) => [id, makeObject(id, `Card ${id}`)]),
      ),
      waiting_for: waitingFor,
    });
    useGameStore.setState({ gameMode: "online", gameState: state, waitingFor });

    render(<CardChoiceModal />);

    const clickOrder = [46, 42, 45, 43, 44];
    for (const id of clickOrder) {
      fireEvent.click(screen.getByLabelText(new RegExp(`Card ${id}$`)).closest("button")!);
    }

    expect(screen.getByLabelText(/Card 46$/).closest("button")).toHaveTextContent("Top");
    expect(screen.getByText("Top")).toBeInTheDocument();
    expect(screen.getByText("2nd")).toBeInTheDocument();
    expect(screen.getByText("3rd")).toBeInTheDocument();
    expect(screen.getByText("4th")).toBeInTheDocument();
    expect(screen.getByText("5th")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCards",
      data: { cards: clickOrder },
    });
  });

  it("keeps generic badges for unordered searches", () => {
    const waitingFor: WaitingFor = {
      type: "SearchChoice",
      data: {
        player: 0,
        cards: [42],
        count: 1,
        ordering_hint: "Unordered",
      },
    };
    const state = buildGameState({
      players: [buildPlayer({ id: 0, library: [42] }), buildPlayer({ id: 1 })],
      objects: { 42: makeObject(42, "Unordered Card") },
      waiting_for: waitingFor,
    });
    useGameStore.setState({ gameMode: "online", gameState: state, waitingFor });

    render(<CardChoiceModal />);
    const cardButton = screen
      .getAllByRole("button")
      .find((button) => !button.textContent?.includes("Confirm"));
    expect(cardButton).toBeDefined();
    fireEvent.click(cardButton!);

    expect(screen.getByText("Choose")).toBeInTheDocument();
    expect(screen.queryByText("Top")).not.toBeInTheDocument();
  });
});
