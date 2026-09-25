import { cleanup, render, screen } from "@testing-library/react";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useDisplayedLife } from "../useDisplayedLife.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";

function Readout({ playerId, snapshotLife }: { playerId: number; snapshotLife: number }) {
  return <span data-testid="life">{useDisplayedLife(playerId, snapshotLife)}</span>;
}

describe("useDisplayedLife", () => {
  beforeEach(() => {
    useAnimationStore.getState().clearQueue();
    useGameStore.setState({ engineCommitEpoch: 4 });
  });

  afterEach(() => {
    cleanup();
    useAnimationStore.getState().clearQueue();
  });

  it("renders the snapshot life when no animated total has been recorded", () => {
    render(<Readout playerId={0} snapshotLife={20} />);
    expect(screen.getByTestId("life")).toHaveTextContent("20");
  });

  it("prefers a total recorded under the current engine commit epoch", () => {
    render(<Readout playerId={0} snapshotLife={20} />);

    act(() => {
      useAnimationStore.getState().recordDisplayedLife(0, 18, 4);
    });

    expect(screen.getByTestId("life")).toHaveTextContent("18");
  });

  it("keeps each player's own recorded total", () => {
    render(<Readout playerId={1} snapshotLife={20} />);

    act(() => {
      useAnimationStore.getState().recordDisplayedLife(0, 18, 4);
    });

    expect(screen.getByTestId("life")).toHaveTextContent("20");

    act(() => {
      useAnimationStore.getState().recordDisplayedLife(1, 23, 4);
    });

    expect(screen.getByTestId("life")).toHaveTextContent("23");
  });

  it("falls back to the snapshot once a newer snapshot has been committed", () => {
    render(<Readout playerId={0} snapshotLife={20} />);

    act(() => {
      useAnimationStore.getState().recordDisplayedLife(0, 18, 4);
    });
    expect(screen.getByTestId("life")).toHaveTextContent("18");

    // The commit that supersedes those totals: the snapshot is now both newer
    // and authoritative, so the recorded total must not survive it.
    act(() => {
      useGameStore.setState({ engineCommitEpoch: 5 });
    });

    expect(screen.getByTestId("life")).toHaveTextContent("20");
  });

  it("drops totals from a superseded epoch rather than mixing them with new ones", () => {
    render(<Readout playerId={0} snapshotLife={20} />);

    act(() => {
      useAnimationStore.getState().recordDisplayedLife(0, 18, 4);
      useGameStore.setState({ engineCommitEpoch: 5 });
      useAnimationStore.getState().recordDisplayedLife(1, 19, 5);
    });

    expect(screen.getByTestId("life")).toHaveTextContent("20");
    expect(useAnimationStore.getState().displayedLife?.totals.has(0)).toBe(false);
  });
});
