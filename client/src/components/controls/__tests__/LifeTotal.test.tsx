import { act } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  GROUPED_DAMAGE_FLURRY_IMPACT_DELAY_MS,
  type AnimationStep,
} from "../../../animation/types.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { LifeTotal } from "../LifeTotal.tsx";

function setLife(playerId: number, life: number) {
  useGameStore.setState((s) => {
    const prev = s.gameState ?? buildGameState();
    const players = prev.players.map((p, i) => (i === playerId ? { ...p, life } : p));
    return { gameState: { ...prev, players } };
  });
}

// A combat step that damages `playerId` for `amount` (LifeChanged + DamageDealt).
function combatDamageStep(playerId: number, amount: number): AnimationStep {
  return {
    duration: 900,
    effects: [
      {
        event: { type: "LifeChanged", data: { player_id: playerId, amount } },
        duration: 300,
      } as AnimationStep["effects"][number],
      {
        event: {
          type: "DamageDealt",
          data: {
            source_id: 1,
            target: { Player: playerId },
            amount: -amount,
            is_combat: true,
          },
        },
        duration: 900,
      } as AnimationStep["effects"][number],
    ],
  };
}

function groupedDamageStep(playerId: number, lifeAmount?: number, lifePlayerId = playerId): AnimationStep {
  return {
    duration: 900,
    effects: [
      {
        event: {
          type: "GroupedDamageFlurry",
          data: { player_id: playerId, source_ids: [1, 2, 3], total_damage: 3, hit_count: 3 },
        },
        duration: 900,
      },
      ...(lifeAmount == null
        ? []
        : [{
            event: { type: "LifeChanged" as const, data: { player_id: lifePlayerId, amount: lifeAmount } },
            duration: 300,
            displayOnly: true as const,
          }]),
    ],
  };
}

describe("LifeTotal", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useGameStore.setState({
      gameState: buildGameState(),
    });
    useAnimationStore.setState({ activeStep: null });
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it("renders the current life total", () => {
    render(<LifeTotal playerId={0} />);
    expect(screen.getByText("20")).toBeInTheDocument();
  });

  it("always renders the snapshot life while a stale damage animation is active", () => {
    render(<LifeTotal playerId={0} />);
    act(() => {
      setLife(0, 38);
    });
    expect(screen.getByText("38")).toBeInTheDocument();

    // This models a late LifeChanged(-2) event arriving after the full snapshot
    // has already established the authoritative value of 38.
    act(() => {
      useAnimationStore.setState({ activeStep: combatDamageStep(0, -2) });
      vi.advanceTimersByTime(900);
    });

    expect(screen.getByText("38")).toBeInTheDocument();
    expect(screen.queryByText("36")).not.toBeInTheDocument();
  });

  it("updates when the snapshot life changes without an animation step", () => {
    render(<LifeTotal playerId={0} />);
    expect(screen.getByText("20")).toBeInTheDocument();

    act(() => {
      setLife(0, 15);
    });

    expect(screen.getByText("15")).toBeInTheDocument();
  });

  it("does not derive a grouped flurry life total from its display event", () => {
    render(<LifeTotal playerId={0} />);
    expect(screen.getByText("20")).toBeInTheDocument();

    act(() => {
      useAnimationStore.setState({ activeStep: groupedDamageStep(0, -3) });
    });

    act(() => {
      vi.advanceTimersByTime(GROUPED_DAMAGE_FLURRY_IMPACT_DELAY_MS);
    });
    expect(screen.getByText("20")).toBeInTheDocument();
  });

  it("does not move life for grouped flurry without a consumed LifeChanged effect", () => {
    render(<LifeTotal playerId={0} />);

    act(() => {
      useAnimationStore.setState({ activeStep: groupedDamageStep(0) });
      vi.advanceTimersByTime(GROUPED_DAMAGE_FLURRY_IMPACT_DELAY_MS);
    });

    expect(screen.getByText("20")).toBeInTheDocument();
  });

  it("does not derive a lifelink gain from a grouped flurry display event", () => {
    render(<LifeTotal playerId={1} />);
    expect(screen.getByText("20")).toBeInTheDocument();

    act(() => {
      useAnimationStore.setState({ activeStep: groupedDamageStep(0, 3, 1) });
    });

    act(() => {
      vi.advanceTimersByTime(GROUPED_DAMAGE_FLURRY_IMPACT_DELAY_MS);
    });
    expect(screen.getByText("20")).toBeInTheDocument();
  });
});
