import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import { DeckStack } from "../DeckStack";
import type { ScryfallCard } from "../../../services/scryfall";

vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: () => ({ src: null, isLoading: false }),
}));

class ResizeObserverMock {
  observe(): void {}
  disconnect(): void {}
  unobserve(): void {}
}

function makeCard(name: string, typeLine: string): ScryfallCard {
  return {
    id: name.toLowerCase(),
    name,
    mana_cost: "",
    cmc: 0,
    type_line: typeLine,
    color_identity: [],
    legalities: {},
  };
}

describe("DeckStack", () => {
  beforeEach(() => {
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("surfaces high-copy entries ahead of 4x cards within the same lane", () => {
    render(
      <DeckStack
        deck={{
          main: [
            { name: "Island", count: 4 },
            { name: "Forest", count: 24 },
          ],
          sideboard: [],
        }}
        commanders={[]}
        cardDataCache={
          new Map([
            ["Island", makeCard("Island", "Basic Land — Island")],
            ["Forest", makeCard("Forest", "Basic Land — Forest")],
          ])
        }
        onRemoveCard={vi.fn()}
        onRemoveCommander={vi.fn()}
      />,
    );

    const [forestBadge] = screen.getAllByText("24x");
    const [islandBadge] = screen.getAllByText("4x");

    expect(
      forestBadge.compareDocumentPosition(islandBadge) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });
});
