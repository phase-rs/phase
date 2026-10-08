import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { MobilePhaseChip } from "../MobilePhaseChip.tsx";

describe("MobilePhaseChip", () => {
  beforeEach(() => {
    // Factory default: phase PreCombatMain, active_player 0.
    useGameStore.setState({ gameState: buildGameState() });
    usePreferencesStore.setState({ phaseStops: [] });
  });

  afterEach(() => {
    cleanup();
  });

  it("shows the current phase name", () => {
    render(<MobilePhaseChip />);

    expect(
      screen.getByRole("button", { name: /Current phase: Main Phase 1\./ }),
    ).toHaveTextContent("Main Phase 1");
  });

  it("opens a sheet listing every turn step, including ones absent from the desktop strips", () => {
    render(<MobilePhaseChip />);
    fireEvent.click(screen.getByRole("button", { name: /Current phase/ }));

    expect(screen.getByText("Turn phases")).toBeInTheDocument();
    // Untap and Cleanup have no desktop PhaseDot — the sheet must still offer them.
    expect(screen.getByRole("button", { name: /Untap step/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Cleanup step/ })).toBeInTheDocument();
  });

  it("cycles a step's stop scope from the sheet and shows the scope as text", () => {
    render(<MobilePhaseChip />);
    fireEvent.click(screen.getByRole("button", { name: /Current phase/ }));

    const row = screen.getByRole("button", { name: /First main phase/ });
    expect(row).toHaveAttribute("aria-pressed", "false");
    expect(row).toHaveTextContent("Off");

    // off → AllTurns
    fireEvent.click(row);
    expect(usePreferencesStore.getState().phaseStops).toEqual([
      { phase: "PreCombatMain", scope: "AllTurns" },
    ]);
    expect(row).toHaveAttribute("aria-pressed", "true");
    expect(row).toHaveTextContent("All turns");

    // AllTurns → OwnTurn → OpponentsTurns → off
    fireEvent.click(row);
    expect(row).toHaveTextContent("My turns");
    fireEvent.click(row);
    expect(row).toHaveTextContent("Opponents' turns");
    fireEvent.click(row);
    expect(usePreferencesStore.getState().phaseStops).toEqual([]);
    expect(row).toHaveTextContent("Off");
  });

  it("marks the chip with the current phase's stop, colored by scope", () => {
    // Factory default phase is PreCombatMain — arm a stop on it.
    usePreferencesStore.setState({
      phaseStops: [{ phase: "PreCombatMain", scope: "OwnTurn" }],
    });
    const { container } = render(<MobilePhaseChip />);

    expect(container.querySelector(".bg-emerald-400")).not.toBeNull();
  });

  it("does not mark the chip for stops on other phases", () => {
    usePreferencesStore.setState({
      phaseStops: [{ phase: "End", scope: "AllTurns" }],
    });
    const { container } = render(<MobilePhaseChip />);

    expect(container.querySelector(".bg-amber-400")).toBeNull();
  });
});
