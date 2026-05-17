import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { CategoryChoiceModal } from "../CategoryChoiceModal.tsx";
import { useGameStore } from "../../../stores/gameStore.ts";

const dispatchMock = vi.fn();

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

type CategoryChoice = Extract<WaitingFor, { type: "CategoryChoice" }>;

function makeObject(id: number, name: string, coreTypes: string[]): GameObject {
  return {
    id,
    card_id: 1,
    owner: 0,
    controller: 0,
    zone: "Battlefield",
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
    card_types: { supertypes: [], core_types: coreTypes, subtypes: [] },
    mana_cost: { type: "NoCost" },
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
  } as unknown as GameObject;
}

function setUpObjects(objects: Record<string, GameObject>) {
  useGameStore.setState({
    gameState: { objects } as unknown as ReturnType<
      typeof useGameStore.getState
    >["gameState"],
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

  it("disables an artifact creature in the Creature category once chosen as Artifact", () => {
    render(<CategoryChoiceModal data={categoryData()} />);

    const hellkiteButtons = screen.getAllByRole("button", { name: "Steel Hellkite" });
    // Both enabled before selection.
    expect(hellkiteButtons[0]).not.toBeDisabled();
    expect(hellkiteButtons[1]).not.toBeDisabled();

    // Choose Steel Hellkite in the Artifact (first) section.
    fireEvent.click(hellkiteButtons[0]);

    const afterButtons = screen.getAllByRole("button", { name: "Steel Hellkite" });
    expect(afterButtons[0]).toHaveAttribute("aria-pressed", "true");
    // The Creature-section copy is now disabled (engine duplicate-object rule).
    expect(afterButtons[1]).toBeDisabled();
  });

  it("dispatches SelectCategoryPermanents with the chosen choices including null", () => {
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
