import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CombatPhaseIndicator, PhaseIndicatorLeft } from "../PhaseStopBar.tsx";

describe("PhaseStopBar", () => {
  beforeEach(() => {
    useGameStore.setState({ gameState: buildGameState() });
    usePreferencesStore.setState({ phaseStops: [] });
  });

  afterEach(() => {
    cleanup();
  });

  it("describes HUD phase stops and toggles the selected stop", () => {
    render(<PhaseIndicatorLeft />);

    const mainPhase = screen.getByRole("button", {
      name: /Phase stop: First main phase\./,
    });

    expect(mainPhase).not.toHaveAttribute("title");
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("Play lands and cast spells before combat."));
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("No stop set: click to pause auto-pass here."));
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("Current phase."));
    expect(mainPhase).toHaveAccessibleDescription(/Play lands and cast spells before combat\./);
    expect(mainPhase).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(mainPhase);

    expect(usePreferencesStore.getState().phaseStops).toEqual(["PreCombatMain"]);
    expect(mainPhase).toHaveAttribute("aria-pressed", "true");
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("Stop set: click to remove this auto-pass stop."));
    expect(mainPhase).toHaveAccessibleDescription(/Stop set: click to remove this auto-pass stop\./);
  });

  it("describes combat phase group stops", () => {
    render(<CombatPhaseIndicator />);

    expect(
      screen.getByRole("button", {
        name: /Phase stop: Declare attackers step\. The attacking player chooses attackers\./,
      }),
    ).toBeInTheDocument();
  });
});
