import { cleanup, render, screen } from "@testing-library/react";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { DerivedViews } from "../../adapter/types.ts";
import { formatLifeReadout, lifeReadout, useDisplayedLife } from "../useDisplayedLife.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { buildGameState } from "../../test/factories/gameStateFactory.ts";

function Readout({ playerId }: { playerId: number }) {
  return <span data-testid={`life-${playerId}`}>{formatLifeReadout(useDisplayedLife(playerId))}</span>;
}

/** Commits a snapshot whose players sit at the factory's 20 life. */
function commit(derived: DerivedViews | undefined, epoch: number) {
  useGameStore.setState({ gameState: buildGameState({ derived }), engineCommitEpoch: epoch });
}

describe("useDisplayedLife", () => {
  beforeEach(() => {
    useAnimationStore.getState().clearQueue();
    commit(undefined, 4);
  });

  afterEach(() => {
    cleanup();
    useAnimationStore.getState().clearQueue();
  });

  it("renders the snapshot life when no animated total has been recorded", () => {
    render(<Readout playerId={0} />);
    expect(screen.getByTestId("life-0")).toHaveTextContent("20");
  });

  it("prefers a total recorded under the current engine commit epoch", () => {
    render(<Readout playerId={0} />);

    act(() => {
      useAnimationStore.getState().recordDisplayedLife(0, 18, 4);
    });

    expect(screen.getByTestId("life-0")).toHaveTextContent("18");
  });

  it("keeps each player's own recorded total", () => {
    render(<Readout playerId={1} />);

    act(() => {
      useAnimationStore.getState().recordDisplayedLife(0, 18, 4);
    });

    expect(screen.getByTestId("life-1")).toHaveTextContent("20");

    act(() => {
      useAnimationStore.getState().recordDisplayedLife(1, 23, 4);
    });

    expect(screen.getByTestId("life-1")).toHaveTextContent("23");
  });

  it("falls back to the snapshot once a newer snapshot has been committed", () => {
    render(<Readout playerId={0} />);

    act(() => {
      useAnimationStore.getState().recordDisplayedLife(0, 18, 4);
    });
    expect(screen.getByTestId("life-0")).toHaveTextContent("18");

    act(() => {
      useGameStore.setState({ engineCommitEpoch: 5 });
    });

    expect(screen.getByTestId("life-0")).toHaveTextContent("20");
  });

  it("drops totals from a superseded epoch rather than mixing them with new ones", () => {
    render(<Readout playerId={0} />);

    act(() => {
      useAnimationStore.getState().recordDisplayedLife(0, 18, 4);
      useGameStore.setState({ engineCommitEpoch: 5 });
      useAnimationStore.getState().recordDisplayedLife(1, 19, 5);
    });

    expect(screen.getByTestId("life-0")).toHaveTextContent("20");
    expect(useAnimationStore.getState().displayedLife?.totals.has(0)).toBe(false);
  });

  it("keeps an unbounded readout over a landed total until a snapshot without the mark commits", () => {
    commit({ life_magnitude: { 0: "Unbounded" } }, 4);
    render(
      <>
        <Readout playerId={0} />
        <Readout playerId={1} />
      </>,
    );

    act(() => {
      useAnimationStore.getState().recordDisplayedLife(0, 18, 4);
      useAnimationStore.getState().recordDisplayedLife(1, 17, 4);
    });

    expect(screen.getByTestId("life-0")).toHaveTextContent("∞");
    expect(screen.getByTestId("life-1")).toHaveTextContent("17");

    act(() => commit(undefined, 5));

    expect(screen.getByTestId("life-0")).toHaveTextContent("20");
  });
});

describe("lifeReadout", () => {
  it("reads Unbounded only for the seat the engine marked", () => {
    const state = buildGameState({ derived: { life_magnitude: { 1: "Unbounded" } } });
    expect(lifeReadout(state, 1)).toEqual({ magnitude: "Unbounded" });
    expect(lifeReadout(state, 0)).toEqual({ magnitude: "Finite", life: 20 });
  });

  it("reads Finite when the snapshot carries no derived views", () => {
    expect(lifeReadout(buildGameState(), 1)).toEqual({ magnitude: "Finite", life: 20 });
  });
});
