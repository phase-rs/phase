import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { CategoryChoiceModal } from "../CategoryChoiceModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObjectWithCoreTypes } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

type CategoryChoice = Extract<WaitingFor, { type: "CategoryChoice" }>;

function makeObject(id: number, name: string, coreTypes: string[]): GameObject {
  return buildGameObjectWithCoreTypes(coreTypes, {
    id,
    card_id: 1,
    zone: "Battlefield",
    name,
    power: 1,
    toughness: 1,
    base_power: 1,
    base_toughness: 1,
    timestamp: 1,
    entered_battlefield_turn: 1,
  });
}

function setUpObjects(objects: Record<string, GameObject>) {
  useGameStore.setState({
    gameState: buildGameState({ objects }),
  });
}

function categoryData(
  overrides: Partial<CategoryChoice["data"]> = {},
): CategoryChoice["data"] {
  return {
    player: 0,
    target_player: 0,
    categories: ["Artifact", "Creature"],
    eligible_per_category: [
      [57, 56],
      [57, 99],
    ],
    source_id: 1,
    remaining_players: [],
    all_kept: [],
    scoped_players: [0],
    ...overrides,
  };
}

describe("CategoryChoiceModal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    setUpObjects({
      57: makeObject(57, "Steel Hellkite", ["Artifact", "Creature"]),
      56: makeObject(56, "Sol Ring", ["Artifact"]),
      99: makeObject(99, "Grizzly Bears", ["Creature"]),
    });
  });

  afterEach(() => {
    cleanup();
  });

  it("renders one section per category with eligible buttons", () => {
    render(<CategoryChoiceModal data={categoryData()} />);

    expect(screen.getByText("Choose Permanents to Keep")).toBeInTheDocument();
    expect(screen.getByText("Artifact")).toBeInTheDocument();
    expect(screen.getByText("Creature")).toBeInTheDocument();
    // Steel Hellkite is an artifact creature — appears in both lists.
    expect(screen.getAllByRole("button", { name: "Steel Hellkite" })).toHaveLength(2);
    expect(screen.getByRole("button", { name: "Sol Ring" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Grizzly Bears" })).toBeInTheDocument();
  });

  it("renders the disabled none line for an empty category", () => {
    render(
      <CategoryChoiceModal
        data={categoryData({
          categories: ["Artifact", "Enchantment"],
          eligible_per_category: [[56], []],
        })}
      />,
    );

    const noneButton = screen.getByRole("button", {
      name: "No Enchantment — none to keep",
    });
    expect(noneButton).toBeDisabled();
  });

  it("allows one artifact creature to be chosen in multiple category slots", () => {
    render(<CategoryChoiceModal data={categoryData()} />);

    expect(screen.getByRole("button", { name: "Confirm" })).toBeDisabled();
    const hellkiteButtons = screen.getAllByRole("button", { name: "Steel Hellkite" });
    // Both enabled before selection.
    expect(hellkiteButtons[0]).not.toBeDisabled();
    expect(hellkiteButtons[1]).not.toBeDisabled();

    // Choose Steel Hellkite in the Artifact (first) section.
    fireEvent.click(hellkiteButtons[0]);

    const afterButtons = screen.getAllByRole("button", { name: "Steel Hellkite" });
    expect(afterButtons[0]).toHaveAttribute("aria-pressed", "true");
    expect(afterButtons[1]).not.toBeDisabled();

    fireEvent.click(afterButtons[1]);
    expect(screen.getByRole("button", { name: "Confirm" })).not.toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCategoryPermanents",
      data: { choices: [57, 57] },
    });
  });

  it("dispatches SelectCategoryPermanents with all nonempty categories chosen", () => {
    render(<CategoryChoiceModal data={categoryData()} />);

    fireEvent.click(screen.getByRole("button", { name: "Sol Ring" }));
    fireEvent.click(screen.getByRole("button", { name: "Grizzly Bears" }));
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCategoryPermanents",
      data: { choices: [56, 99] },
    });
  });

  it("dispatches all-null choices when every category is empty", () => {
    render(
      <CategoryChoiceModal
        data={categoryData({
          categories: ["Artifact", "Creature"],
          eligible_per_category: [[], []],
        })}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(dispatchMock).toHaveBeenCalledWith({
      type: "SelectCategoryPermanents",
      data: { choices: [null, null] },
    });
  });
});
