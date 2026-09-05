import { cleanup, fireEvent, render, screen } from "@testing-library/react";
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
  it("keeps a backed duplicate candidate available only for commanders-inside composition", () => {
    const name = "The Prismatic Piper";
    const sharedProps = {
      deck: [{ name, count: 2 }],
      cardDataCache: new Map([[name, makeLegendaryCreature(name)]]),
      deckSizeRule: { type: "Minimum" as const, data: 60 },
      isCommanderEligible: () => true,
      onSetCommander: vi.fn(),
      onRemoveCommander: vi.fn(),
    };

    const inside = render(
      <CommanderPanel
        {...sharedProps}
        commanders={[name]}
        deckComposition="commanders-inside"
      />,
    );
    expect(screen.getByRole("button", { name })).toBeInTheDocument();
    inside.unmount();

    const fullyDesignated = render(
      <CommanderPanel
        {...sharedProps}
        commanders={[name, name]}
        deckComposition="commanders-inside"
      />,
    );
    expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
    fullyDesignated.unmount();

    render(
      <CommanderPanel
        {...sharedProps}
        commanders={[name]}
        deckComposition="commanders-outside"
      />,
    );
    expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
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
        isCommanderEligible={() => true}
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
        isCommanderEligible={() => true}
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
        isCommanderEligible={() => false}
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
        isCommanderEligible={() => false}
        onSetCommander={vi.fn()}
        onRemoveCommander={vi.fn()}
      />,
    );
    expect(screen.getByText("99/100 cards")).toHaveClass("text-yellow-400");
  });
});
