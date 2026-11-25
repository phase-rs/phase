import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CommanderPanel } from "../CommanderPanel";
import type { ScryfallCard } from "../../../services/scryfall";

afterEach(cleanup);

function makeLegendaryCreature(name: string): ScryfallCard {
  return {
    name,
    mana_cost: "",
    cmc: 3,
    type_line: "Legendary Creature — Ninja",
    color_identity: ["B"],
    legalities: { commander: "legal" },
  };
}

describe("CommanderPanel", () => {
  it("renders only the supplied candidate names in order and forwards their spelling", () => {
    const onSetCommander = vi.fn();
    const props = {
      commanders: [],
      deck: [{ name: "Local Legend", count: 2 }],
      deckComposition: "commanders-inside" as const,
      cardDataCache: new Map(),
      deckSizeRule: { type: "Minimum" as const, data: 60 },
      onSetCommander,
      onRemoveCommander: vi.fn(),
    };
    const names = ["Sheoldred // The True Scriptures", "Isamaru, Hound of Konda"];
    const { rerender } = render(<CommanderPanel {...props} commanderCandidates={null} />);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    rerender(<CommanderPanel {...props} commanderCandidates={names} />);
    const buttons = within(screen.getByText("Set as commander", { exact: false }).parentElement!)
      .getAllByRole("button");
    expect(buttons.map((button) => button.textContent)).toEqual(names);
    fireEvent.click(buttons[0]);
    expect(onSetCommander).toHaveBeenCalledWith(names[0]);
    rerender(<CommanderPanel {...props} commanderCandidates={[]} />);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("renders two same-name commander slots without duplicate keys", () => {
    const name = "The Prismatic Piper";
    const onRemoveCommander = vi.fn();
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});

    render(
      <CommanderPanel
        commanders={[name, name]}
        deck={[{ name, count: 2 }]}
        deckComposition="commanders-inside"
        cardDataCache={new Map([[name, makeLegendaryCreature(name)]])}
        deckSizeRule={{ type: "Minimum", data: 60 }}
        commanderCandidates={[]}
        onSetCommander={vi.fn()}
        onRemoveCommander={onRemoveCommander}
      />,
    );

    const removeButtons = screen.getAllByRole("button", { name: "Remove" });
    expect(removeButtons).toHaveLength(2);
    fireEvent.click(removeButtons[1]);
    expect(onRemoveCommander).toHaveBeenCalledWith(name);
    expect(consoleError.mock.calls.flat().join(" ")).not.toMatch(/same key|unique.*key/i);
    consoleError.mockRestore();
  });

  it("shows all eligible commanders instead of truncating to five", () => {
    const names = [
      "Commander One",
      "Commander Two",
      "Commander Three",
      "Commander Four",
      "Commander Five",
      "Commander Six",
    ];

    render(
      <CommanderPanel
        commanders={[]}
        deck={names.map((name) => ({ name, count: 1 }))}
        deckComposition="commanders-outside"
        cardDataCache={
          new Map(names.map((name) => [name, makeLegendaryCreature(name)]))
        }
        deckSizeRule={{ type: "Exactly", data: 100 }}
        commanderCandidates={names}
        onSetCommander={vi.fn()}
        onRemoveCommander={vi.fn()}
      />,
    );

    for (const name of names) {
      expect(
        screen.getByRole("button", { name }),
      ).toBeInTheDocument();
    }
  });

  /**
   * V7 (charter G4) — CR 903.13f(1): the deck-size indicator reads the TYPED
   * rule, exhaustively. Both directions are asserted because the satisfied
   * half alone is passed by simply deleting the check (always-green), and the
   * unsatisfied half alone is passed by always-yellow.
   */
  it("treats a 61-card Minimum(60) deck as satisfied and a 99-card Exactly(100) deck as not", () => {
    // (a) CR 903.13f(1): at least 60, NO maximum. 61 is legal.
    const minimum = render(
      <CommanderPanel
        commanders={[]}
        deck={[{ name: "Filler Card", count: 61 }]}
        deckComposition="commanders-outside"
        cardDataCache={new Map()}
        deckSizeRule={{ type: "Minimum", data: 60 }}
        commanderCandidates={[]}
        onSetCommander={vi.fn()}
        onRemoveCommander={vi.fn()}
      />,
    );
    expect(screen.getByText("61/60 cards")).toHaveClass("text-green-400");
    minimum.unmount();

    // (b) CR 903.5a: exactly 100. 99 is not yet legal.
    render(
      <CommanderPanel
        commanders={[]}
        deck={[{ name: "Filler Card", count: 99 }]}
        deckComposition="commanders-outside"
        cardDataCache={new Map()}
        deckSizeRule={{ type: "Exactly", data: 100 }}
        commanderCandidates={[]}
        onSetCommander={vi.fn()}
        onRemoveCommander={vi.fn()}
      />,
    );
    expect(screen.getByText("99/100 cards")).toHaveClass("text-yellow-400");
  });

  it("does not render companion section when no companion exists in pool or selected", () => {
    render(
      <CommanderPanel
        commanders={[]}
        deck={[{ name: "Filler Card", count: 60 }]}
        deckComposition="commanders-outside"
        cardDataCache={new Map()}
        deckSizeRule={{ type: "Minimum", data: 60 }}
        commanderCandidates={[]}
        onSetCommander={vi.fn()}
        onRemoveCommander={vi.fn()}
        companionCandidates={[]}
      />,
    );
    expect(screen.queryByText(/companion/i)).not.toBeInTheDocument();
  });

  it("renders companion slot and candidates when companionCandidates is provided", () => {
    const onSetCompanion = vi.fn();
    render(
      <CommanderPanel
        commanders={[]}
        deck={[{ name: "Lurrus of the Dream-Den", count: 1 }]}
        deckComposition="commanders-outside"
        cardDataCache={new Map()}
        deckSizeRule={{ type: "Minimum", data: 60 }}
        commanderCandidates={[]}
        onSetCommander={vi.fn()}
        onRemoveCommander={vi.fn()}
        companionCandidates={["Lurrus of the Dream-Den"]}
        onSetCompanion={onSetCompanion}
      />,
    );
    expect(screen.getByText("Companion")).toBeInTheDocument();
    const candidateBtn = screen.getByRole("button", { name: "Lurrus of the Dream-Den" });
    expect(candidateBtn).toBeInTheDocument();
    fireEvent.click(candidateBtn);
    expect(onSetCompanion).toHaveBeenCalledWith("Lurrus of the Dream-Den");
  });

  it("renders selected companion with remove button and handles removal", () => {
    const onRemoveCompanion = vi.fn();
    render(
      <CommanderPanel
        commanders={[]}
        deck={[{ name: "Lurrus of the Dream-Den", count: 1 }]}
        deckComposition="commanders-outside"
        cardDataCache={new Map()}
        deckSizeRule={{ type: "Minimum", data: 60 }}
        commanderCandidates={[]}
        onSetCommander={vi.fn()}
        onRemoveCommander={vi.fn()}
        companion="Lurrus of the Dream-Den"
        companionCandidates={["Lurrus of the Dream-Den"]}
        onRemoveCompanion={onRemoveCompanion}
      />,
    );
    expect(screen.getByText("Companion")).toBeInTheDocument();
    expect(screen.getByText("Lurrus of the Dream-Den")).toBeInTheDocument();
    const removeBtn = screen.getByRole("button", { name: "Remove companion" });
    fireEvent.click(removeBtn);
    expect(onRemoveCompanion).toHaveBeenCalled();
  });
});
