import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { UndoButton } from "../UndoButton.tsx";

describe("UndoButton", () => {
  afterEach(() => {
    cleanup();
    useGameStore.setState({ stateHistory: [], gameMode: null });
  });

  it("renders when single-player history exists", () => {
    useGameStore.setState({
      stateHistory: [buildGameState()],
      gameMode: "ai",
    });

    render(<UndoButton />);

    expect(screen.getByRole("button", { name: /undo/i })).toBeInTheDocument();
  });

  it("does not render without history", () => {
    useGameStore.setState({ stateHistory: [], gameMode: "ai" });

    render(<UndoButton />);

    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("never renders in multiplayer, even with history", () => {
    // Multiplayer state is authoritative and shared — a client-side rewind
    // would desync, so the affordance must stay hidden regardless of history.
    useGameStore.setState({
      stateHistory: [buildGameState()],
      gameMode: "online",
    });

    render(<UndoButton />);

    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});
