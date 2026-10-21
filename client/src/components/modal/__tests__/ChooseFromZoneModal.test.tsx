import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ChooseFromZoneConstraint, GameObject } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();
vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

const budget: ChooseFromZoneConstraint = {
  type: "TotalManaValue", comparator: "LE", value: 6,
};

function artifactCreature(id: number, manaValue: number) {
  return gameObjectFactory.creature().inGraveyard().withId(id)
    .named(`Artifact creature ${id}`).withCost([], manaValue)
    .params({ card_types: { core_types: ["Artifact", "Creature"] } }).build();
}

function seed(cards: GameObject[], {
  count = cards.length, upTo = true, constraint = budget, player = 0, sourceId = 1,
}: {
  count?: number; upTo?: boolean; constraint?: ChooseFromZoneConstraint | null;
  player?: number; sourceId?: number;
} = {}) {
  const gameState = gameStateFactory.withPlayers(0, 1).withObjects(...cards)
    .chooseFromZoneChoice({
      player, source_id: sourceId, cards: cards.map((card) => card.id),
      count, up_to: upTo, constraint,
    }).build();
  setGameStoreForTest({ gameState, gameMode: "online" });
}

function select(card: GameObject) {
  const button = screen.getByLabelText(new RegExp(`${card.name}$`)).closest("button");
  expect(button).toBeInTheDocument();
  fireEvent.click(button!);
}

function confirm(cards: number[]) {
  const button = screen.getByRole("button", { name: "Confirm" });
  expect(button).toBeEnabled();
  fireEvent.click(button);
  expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards } });
}

describe("ChooseFromZone modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });
  afterEach(() => {
    cleanup();
    useGameStore.setState(useGameStore.getInitialState(), true);
    useMultiplayerStore.setState(useMultiplayerStore.getInitialState(), true);
  });

  it.each<[string, number[]]>([
    ["singleton six", [6]], ["four plus two", [4, 2]],
    ["six zero mana cards", [0, 0, 0, 0, 0, 0]],
  ])("submits %s to engine validation", (_name, values) => {
    const cards = values.map((value, index) => artifactCreature(40 + index, value));
    seed(cards);
    render(<CardChoiceModal />);
    cards.forEach(select);
    confirm(cards.map((card) => card.id));
  });

  it("submits an empty selection with visible eligible candidates", () => {
    const card = artifactCreature(42, 6);
    seed([card]);
    render(<CardChoiceModal />);
    expect(screen.getByLabelText(/Artifact creature 42$/)).toBeInTheDocument();
    confirm([]);
  });

  it("retains the bounded count cap for zero mana cards", () => {
    const cards = [42, 43, 44].map((id) => artifactCreature(id, 0));
    seed(cards, { count: 2 });
    render(<CardChoiceModal />);
    select(cards[0]);
    select(cards[1]);
    expect(screen.getByRole("button", { name: "Confirm" })).toBeEnabled();
    select(cards[2]);
    expect(screen.getByLabelText(/Artifact creature 44$/).closest("button"))
      .not.toHaveTextContent("Choose");
    confirm([42, 43]);
  });

  it("retains exact count guarding without a constraint", () => {
    const cards = [artifactCreature(42, 4), artifactCreature(43, 2)];
    seed(cards, { count: 2, upTo: false, constraint: null });
    render(<CardChoiceModal />);
    select(cards[0]);
    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    select(cards[1]);
    confirm([42, 43]);
  });

  it("retains invalid and valid distinct card type selections", () => {
    const first = gameObjectFactory.creature().inGraveyard().withId(42).named("First creature").build();
    const second = gameObjectFactory.creature().inGraveyard().withId(43).named("Second creature").build();
    const artifact = gameObjectFactory.artifact().inGraveyard().withId(44).named("Artifact").build();
    seed([first, second, artifact], { count: 2, constraint: {
      type: "DistinctCardTypes", categories: ["Creature", "Artifact"],
    } });
    render(<CardChoiceModal />);
    select(first);
    select(second);
    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    select(second);
    select(artifact);
    confirm([42, 44]);
  });

  it("hides another player's choice after an own-player positive control", () => {
    const card = artifactCreature(42, 6);
    seed([card]);
    render(<CardChoiceModal />);
    select(card);
    confirm([42]);
    seed([card], { player: 1 });
    expect(screen.queryByRole("button", { name: "Confirm" })).not.toBeInTheDocument();
  });

  it.each(["player", "source", "candidates"])("resets selection when the prompt %s changes", (identity) => {
    const first = artifactCreature(42, 6);
    const second = artifactCreature(43, 6);
    seed([first]);
    render(<CardChoiceModal />);
    select(first);
    confirm([42]);
    dispatchMock.mockClear();
    if (identity === "player") {
      // The next prompt belongs to the other seat, which now has control.
      act(() => useMultiplayerStore.setState({ activePlayerId: 1 }));
      seed([first], { player: 1 });
    } else if (identity === "source") {
      seed([first], { sourceId: 2 });
    } else {
      seed([second]);
    }
    expect(screen.queryByText("Choose")).not.toBeInTheDocument();
    confirm([]);
  });
});
