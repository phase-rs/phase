import { cleanup, render, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CompactStrip } from "../CompactStrip.tsx";

describe("CompactStrip", () => {
  afterEach(() => {
    cleanup();
  });

  it("renders the engine's unbounded life as a green ∞ and keeps a finite seat numeric", () => {
    // Only `life_magnitude` is seeded, so no `UnboundedBadge` can supply the title.
    useGameStore.setState({
      gameState: buildGameState({ derived: { life_magnitude: { 1: "Unbounded" } } }),
    });

    const { getByTestId } = render(
      <>
        <CompactStrip playerId={0} />
        <CompactStrip playerId={1} />
      </>,
    );

    const readout = within(getByTestId("compact-strip-1")).getByText("∞");
    expect(readout).toHaveAttribute("title", "Unbounded life (∞)");
    expect(readout).toHaveAttribute("aria-label", "Unbounded life (∞)");
    expect(readout).toHaveClass("text-green-400");
    const finite = within(getByTestId("compact-strip-0")).getByText("20");
    expect(finite).not.toHaveAttribute("title");
  });
});
