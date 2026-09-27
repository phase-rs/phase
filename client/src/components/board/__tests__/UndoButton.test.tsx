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

  it("uses an accessible icon-only treatment when requested", () => {
    useGameStore.setState({
      stateHistory: [buildGameState()],
      gameMode: "ai",
    });

    render(<UndoButton iconOnly />);

    const button = screen.getByRole("button", { name: "Undo" });
    expect(button).toHaveAttribute("data-icon-only", "true");
    expect(button).toHaveClass("tabletop-liquid-glass-control", "h-11", "w-11");
    const icon = button.querySelector("svg");
    expect(icon).toHaveClass("h-5", "w-5");
    expect(icon).toHaveAttribute("fill", "none");
    expect(icon).toHaveAttribute("stroke", "currentColor");
    expect(icon).toHaveAttribute("stroke-width", "2.4");
    expect(icon?.querySelector("path")).toHaveAttribute(
      "d",
      "m9 15-6-6m0 0 6-6M3 9h12a6 6 0 0 1 0 12h-3",
    );
    expect(button.querySelector("[data-control-label]")).toBeNull();
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
