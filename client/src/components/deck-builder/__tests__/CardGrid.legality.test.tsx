import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CardGrid } from "../CardGrid";
import { scryfallLegalityKey } from "../../../services/scryfall";
import type { ScryfallCard } from "../../../services/scryfall";
import { DECK_CONSTRUCTION_FORMATS, FORMAT_REGISTRY } from "../../../data/formatRegistry";

const { useCardImage } = vi.hoisted(() => ({ useCardImage: vi.fn() }));

vi.mock("../../../hooks/useCardImage", () => ({ useCardImage }));

function card(name: string, legalities: Record<string, string>): ScryfallCard {
  return {
    id: "11111111-1111-4111-8111-111111111111",
    oracle_id: "22222222-2222-4222-8222-222222222222",
    name,
    mana_cost: "{R}",
    cmc: 1,
    type_line: "Instant",
    color_identity: ["R"],
    legalities,
  };
}

// The most permissive card the engine can produce: legal under every table
// the card data records.
const recordedEverywhere = card(
  "Recorded Everywhere",
  Object.fromEntries(
    FORMAT_REGISTRY.filter((entry) => entry.legality_key !== null).map((entry) => [
      entry.legality_key as string,
      "legal",
    ]),
  ),
);

const recordedNowhere = card("Recorded Nowhere", {});

describe("CardGrid legality lens", () => {
  beforeEach(() => {
    useCardImage.mockReturnValue({ src: null, isLoading: false, advanceFailedSource: vi.fn() });
  });

  afterEach(() => {
    cleanup();
    useCardImage.mockReset();
  });

  it("Freeform and Freeform Commander mark no card unavailable", () => {
    for (const format of ["Freeform", "FreeformCommander"] as const) {
      const { unmount } = render(
        <CardGrid
          cards={[recordedNowhere, recordedEverywhere]}
          legalityFormat={format}
          onAddCard={vi.fn()}
        />,
      );
      for (const button of screen.getAllByRole("button")) {
        expect(button).not.toBeDisabled();
      }
      expect(screen.queryByText(/Not .* legal/i)).toBeNull();
      expect(screen.queryByText("Legal")).toBeNull();
      expect(screen.queryByText("Not Legal")).toBeNull();
      unmount();
    }
  });

  it("a format with a recorded table still greys a card it does not record", () => {
    render(
      <CardGrid
        cards={[recordedNowhere, recordedEverywhere]}
        legalityFormat="Legacy"
        onAddCard={vi.fn()}
      />,
    );
    const buttons = screen.getAllByRole("button");
    expect(buttons[0]).toBeDisabled();
    expect(buttons[1]).not.toBeDisabled();
    expect(screen.getByText("Not Legal")).toBeInTheDocument();
    expect(screen.getByText("Legal")).toBeInTheDocument();
  });

  it("a tile is disabled exactly when the format publishes a legality key the card lacks", () => {
    for (const entry of DECK_CONSTRUCTION_FORMATS) {
      const { getByRole, unmount } = render(
        <CardGrid cards={[recordedNowhere]} legalityFormat={entry.format} onAddCard={vi.fn()} />,
      );
      expect(getByRole("button").hasAttribute("disabled")).toBe(entry.legality_key !== null);
      unmount();
    }
  });

  it("the deck browser's key for each format is the engine-published one", () => {
    for (const entry of FORMAT_REGISTRY) {
      expect(scryfallLegalityKey(entry.format)).toBe(entry.legality_key ?? undefined);
    }
    expect(scryfallLegalityKey("Freeform")).toBeUndefined();
    expect(scryfallLegalityKey("FreeformCommander")).toBeUndefined();
  });
});
