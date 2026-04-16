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
        cardDataCache={
          new Map(names.map((name) => [name, makeLegendaryCreature(name)]))
        }
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
});
