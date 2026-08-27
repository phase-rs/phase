import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { CommanderPanel } from "../CommanderPanel";
import type { ScryfallCard } from "../../../services/scryfall";

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
