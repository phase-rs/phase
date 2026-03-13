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

function makeCard(name: string, typeLine: string, cmc = 0): ScryfallCard {
  return {
    id: name.toLowerCase(),
    name,
    mana_cost: "",
    cmc,
    type_line: typeLine,
    color_identity: [],
    legalities: {},
  };
}

function getRequiredParent(
  element: HTMLElement,
  label: string,
): HTMLElement {
  const parent = element.parentElement;
  if (!parent) {
    throw new Error(`Missing parent element for ${label}`);
  }
  return parent;
}

function getTileByRemoveTitle(cardName: string): HTMLElement {
  const removeButton = screen.getByTitle(`Remove one ${cardName}`);
  return getRequiredParent(
    getRequiredParent(removeButton, `remove button for ${cardName}`),
    `tile for ${cardName}`,
  );
}

function expectDocumentOrder(before: HTMLElement, after: HTMLElement): void {
  expect(
    before.compareDocumentPosition(after) & Node.DOCUMENT_POSITION_FOLLOWING,
  ).toBeTruthy();
}

describe("DeckStack", () => {
  beforeEach(() => {
    vi.stubGlobal("ResizeObserver", ResizeObserverMock);
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it("renders each card as its own stack tile with its own count badge", () => {
    render(
      <DeckStack
        deck={{
          main: [
            { name: "Banishing Light", count: 2 },
            { name: "Leyline of Hope", count: 3 },
            { name: "Forest", count: 24 },
          ],
          sideboard: [],
        }}
        commanders={[]}
        cardDataCache={
          new Map([
            ["Banishing Light", makeCard("Banishing Light", "Enchantment")],
            ["Leyline of Hope", makeCard("Leyline of Hope", "Enchantment")],
            ["Forest", makeCard("Forest", "Basic Land — Forest")],
          ])
        }
        onAddCard={vi.fn()}
        onRemoveCard={vi.fn()}
        onRemoveCommander={vi.fn()}
      />,
    );

    const forestTile = getTileByRemoveTitle("Forest");
    const leylineTile = getTileByRemoveTitle("Leyline of Hope");
    const banishingTile = getTileByRemoveTitle("Banishing Light");

    expect(screen.getByTitle("Add one Forest")).toBeInTheDocument();
    expect(forestTile.textContent).toContain("24");
    expect(forestTile.textContent).toContain("Forest");
    expect(forestTile.textContent).not.toContain("Leyline of Hope");
    expect(forestTile.textContent).not.toContain("Banishing Light");

    expect(leylineTile.textContent).toContain("3");
    expect(leylineTile.textContent).toContain("Leyline of Hope");
    expect(leylineTile.textContent).not.toContain("Forest");

    expect(banishingTile.textContent).toContain("2");
    expect(banishingTile.textContent).toContain("Banishing Light");
    expect(banishingTile.textContent).not.toContain("Forest");
  });

  it("sorts by type first, then cmc, then name, with lands after spells", () => {
    render(
      <DeckStack
        deck={{
          main: [
            { name: "Plains", count: 20 },
            { name: "Ajani's Pridemate", count: 4 },
            { name: "Leyline of Hope", count: 3 },
            { name: "Banishing Light", count: 2 },
            { name: "Healer's Hawk", count: 4 },
            { name: "Angel of Vitality", count: 1 },
          ],
          sideboard: [],
        }}
        commanders={[]}
        cardDataCache={
          new Map([
            ["Plains", makeCard("Plains", "Basic Land — Plains")],
            ["Ajani's Pridemate", makeCard("Ajani's Pridemate", "Creature", 2)],
            ["Angel of Vitality", makeCard("Angel of Vitality", "Creature", 2)],
            ["Leyline of Hope", makeCard("Leyline of Hope", "Enchantment", 4)],
            ["Banishing Light", makeCard("Banishing Light", "Enchantment", 3)],
            ["Healer's Hawk", makeCard("Healer's Hawk", "Creature", 1)],
          ])
        }
        onAddCard={vi.fn()}
        onRemoveCard={vi.fn()}
        onRemoveCommander={vi.fn()}
      />,
    );

    const ajaniTile = getTileByRemoveTitle("Ajani's Pridemate");
    const angelTile = getTileByRemoveTitle("Angel of Vitality");
    const hawkTile = getTileByRemoveTitle("Healer's Hawk");
    const leylineTile = getTileByRemoveTitle("Leyline of Hope");
    const banishingTile = getTileByRemoveTitle("Banishing Light");
    const plainsTile = getTileByRemoveTitle("Plains");

    expect(screen.getByText("Creatures")).toBeInTheDocument();
    expect(screen.getByText("Spells")).toBeInTheDocument();
    expect(screen.getByText("Lands")).toBeInTheDocument();
    expect(
      screen.getByTitle("Ajani's Pridemate is at the copy limit"),
    ).toBeDisabled();
    expect(screen.queryByText("MD")).not.toBeInTheDocument();
    expect(screen.queryByText("SB")).not.toBeInTheDocument();

    expectDocumentOrder(hawkTile, ajaniTile);
    expectDocumentOrder(ajaniTile, angelTile);
    expectDocumentOrder(angelTile, banishingTile);
    expectDocumentOrder(banishingTile, leylineTile);
    expectDocumentOrder(banishingTile, plainsTile);
  });
});
