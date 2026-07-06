import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";
import { RevealOverlay } from "../RevealOverlay.tsx";

// Return a non-null src so each RevealCard renders an <img alt={name}> we can query.
vi.mock("../../../hooks/useCardImage", () => ({
  useCardImage: (cardName: string) => ({ src: `img:${cardName}`, isLoading: false }),
}));

function setStore(opts: {
  library?: number[];
  hand?: number[];
  objects: Record<number, string>;
  revealed: number[];
}) {
  const objects = Object.entries(opts.objects).map(([id, name]) =>
    buildGameObject({ id: Number(id), name, zone: "Library" }),
  );
  const gameState = buildGameState({
    players: buildPlayers([
      { id: 0, library: opts.library ?? [], hand: opts.hand ?? [] },
      { id: 1, library: [], hand: [] },
    ]),
    objects: buildObjectMap(...objects),
    revealed_cards: opts.revealed,
  });

  useGameStore.setState({ gameState });
}

afterEach(() => {
  cleanup();
});

describe("RevealOverlay", () => {
  it("renders every revealed top-of-library card when two or more are revealed", () => {
    setStore({
      library: [10, 11, 12],
      objects: { 10: "Beast Alpha", 11: "Beast Beta", 12: "Beast Gamma" },
      revealed: [10, 11],
    });

    render(<RevealOverlay />);

    expect(screen.getByAltText("Beast Alpha")).toBeInTheDocument();
    expect(screen.getByAltText("Beast Beta")).toBeInTheDocument();
    // In the library but not revealed — not surfaced.
    expect(screen.queryByAltText("Beast Gamma")).not.toBeInTheDocument();
  });

  it("renders nothing for a single revealed card (LibraryPile already shows the top)", () => {
    setStore({
      library: [10, 11],
      objects: { 10: "Solo Beast", 11: "Filler" },
      revealed: [10],
    });

    const { container } = render(<RevealOverlay />);

    expect(container).toBeEmptyDOMElement();
  });

  it("excludes hand reveals, surfacing only library reveals", () => {
    setStore({
      library: [10, 11],
      hand: [20],
      objects: { 10: "Lib A", 11: "Lib B", 20: "Hand X" },
      revealed: [10, 11, 20],
    });

    render(<RevealOverlay />);

    expect(screen.getByAltText("Lib A")).toBeInTheDocument();
    expect(screen.getByAltText("Lib B")).toBeInTheDocument();
    expect(screen.queryByAltText("Hand X")).not.toBeInTheDocument();
  });

  it("defensively skips a redacted name but still surfaces the rest", () => {
    setStore({
      library: [10, 11, 12],
      objects: { 10: "Real Beast A", 11: "Hidden Card", 12: "Real Beast B" },
      revealed: [10, 11, 12],
    });

    render(<RevealOverlay />);

    expect(screen.getByAltText("Real Beast A")).toBeInTheDocument();
    expect(screen.getByAltText("Real Beast B")).toBeInTheDocument();
    expect(screen.queryByAltText("Hidden Card")).not.toBeInTheDocument();
  });
});
