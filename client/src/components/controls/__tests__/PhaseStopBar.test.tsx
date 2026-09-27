import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import {
  CombatPhaseIndicator,
  MajorPhaseStopRail,
  PhaseIndicatorLeft,
} from "../PhaseStopBar.tsx";

describe("PhaseStopBar", () => {
  beforeEach(() => {
    useGameStore.setState({ gameState: buildGameState() });
    usePreferencesStore.setState({ phaseStops: [] });
  });

  afterEach(() => {
    cleanup();
  });

  it("cycles the selected stop through its four scope states", () => {
    render(<PhaseIndicatorLeft />);

    const mainPhase = screen.getByRole("button", {
      name: /Phase stop: First main phase\./,
    });

    expect(mainPhase).not.toHaveAttribute("title");
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("Play lands and cast spells before combat."));
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("No stop set: click to pause auto-pass here."));
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("Current phase."));
    expect(mainPhase).toHaveAttribute("aria-pressed", "false");

    // off → AllTurns
    fireEvent.click(mainPhase);
    expect(usePreferencesStore.getState().phaseStops).toEqual([
      { phase: "PreCombatMain", scope: "AllTurns" },
    ]);
    expect(mainPhase).toHaveAttribute("aria-pressed", "true");
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("Stops on every turn."));

    // AllTurns → OwnTurn
    fireEvent.click(mainPhase);
    expect(usePreferencesStore.getState().phaseStops).toEqual([
      { phase: "PreCombatMain", scope: "OwnTurn" },
    ]);
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("Stops only on your turns."));

    // OwnTurn → OpponentsTurns
    fireEvent.click(mainPhase);
    expect(usePreferencesStore.getState().phaseStops).toEqual([
      { phase: "PreCombatMain", scope: "OpponentsTurns" },
    ]);
    expect(mainPhase).toHaveAttribute(
      "aria-label",
      expect.stringContaining("Stops only on opponents' turns."),
    );

    // OpponentsTurns → off
    fireEvent.click(mainPhase);
    expect(usePreferencesStore.getState().phaseStops).toEqual([]);
    expect(mainPhase).toHaveAttribute("aria-pressed", "false");
    expect(mainPhase).toHaveAttribute("aria-label", expect.stringContaining("No stop set: click to pause auto-pass here."));
  });

  it("cycles a stop in place, preserving array order", () => {
    // Order matters: `useGameplayPreferencesSync` dedupes by positional comparison, so
    // cycling must not move the touched stop to the end of the array. Seed two
    // stops and cycle the first — it must stay at index 0.
    usePreferencesStore.setState({
      phaseStops: [
        { phase: "Upkeep", scope: "AllTurns" },
        { phase: "PreCombatMain", scope: "AllTurns" },
      ],
    });
    render(<PhaseIndicatorLeft />);

    fireEvent.click(screen.getByRole("button", { name: /Phase stop: Upkeep step\./ }));

    expect(usePreferencesStore.getState().phaseStops).toEqual([
      { phase: "Upkeep", scope: "OwnTurn" },
      { phase: "PreCombatMain", scope: "AllTurns" },
    ]);
  });

  it("describes combat phase group stops", () => {
    render(<CombatPhaseIndicator />);

    expect(
      screen.getByRole("button", {
        name: /Phase stop: Declare attackers step\. The attacking player chooses attackers\./,
      }),
    ).toBeInTheDocument();
  });

  it("renders only major phases in the compact rail with a lightweight active state", () => {
    const { container } = render(<MajorPhaseStopRail />);

    const rail = container.querySelector('[data-major-phase-stop-rail]');
    expect(rail?.querySelectorAll("[data-phase-stop-dot]")).toHaveLength(5);
    expect(rail?.querySelector('[data-phase-stop-dot="Draw"]')).not.toBeInTheDocument();
    expect(rail?.querySelector('[data-phase-stop-dot="DeclareAttackers"]')).not.toBeInTheDocument();

    const activePhase = rail?.querySelector('[data-active-phase="true"]');
    expect(activePhase).toHaveAttribute("data-phase-stop-dot", "PreCombatMain");
    expect(activePhase).toHaveClass("bg-transparent", "text-cyan-200");
    expect(activePhase).not.toHaveClass("bg-cyan-950/82");
  });

  it("keeps one continuous rail with a center lane for the life badge", () => {
    const { container } = render(<MajorPhaseStopRail />);

    const rail = container.querySelector('[data-major-phase-stop-rail="all"]');
    const left = rail?.querySelector('[data-phase-stop-rail-section="left"]');
    const right = rail?.querySelector('[data-phase-stop-rail-section="right"]');

    expect(container.querySelectorAll("[data-major-phase-stop-rail]")).toHaveLength(1);
    expect(rail?.querySelector("[data-phase-stop-rail-center-gap]")).toBeInTheDocument();
    expect(
      Array.from(left?.querySelectorAll("[data-phase-stop-dot]") ?? []).map(
        (dot) => dot.getAttribute("data-phase-stop-dot"),
      ),
    ).toEqual(["Upkeep", "PreCombatMain"]);
    expect(
      Array.from(right?.querySelectorAll("[data-phase-stop-dot]") ?? []).map(
        (dot) => dot.getAttribute("data-phase-stop-dot"),
      ),
    ).toEqual(["BeginCombat", "PostCombatMain", "End"]);
  });
});
