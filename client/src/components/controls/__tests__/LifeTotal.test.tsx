import { act } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Make framer-motion's `animate` apply instantly so the displayed value is
// deterministic in jsdom (no real animation frames to wait on).
vi.mock("framer-motion", async (importOriginal) => {
  const actual = await importOriginal<typeof import("framer-motion")>();
  return {
    ...actual,
    animate: (target: { set?: (v: number) => void }, value: number) => {
      target.set?.(value);
      return { stop: () => {} };
    },
  };
});

import type { AnimationStep } from "../../../animation/types.ts";
import type { GameState } from "../../../adapter/types.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { LifeTotal } from "../LifeTotal.tsx";

function setLife(playerId: number, life: number) {
  useGameStore.setState((s) => {
    const prev = (s.gameState ?? { players: [{ life: 20 }, { life: 20 }] }) as GameState;
    const players = prev.players.map((p, i) => (i === playerId ? { ...p, life } : p));
    return { gameState: { ...prev, players } as GameState };
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

describe("LifeTotal", () => {
  beforeEach(() => {
    useGameStore.setState({
      gameState: { players: [{ life: 20 }, { life: 20 }] } as unknown as GameState,
    });
    useAnimationStore.setState({ activeStep: null });
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllTimers();
  });

  it("renders the current life total", () => {
    render(<LifeTotal playerId={0} />);
    expect(screen.getByText("20")).toBeInTheDocument();
  });

  // Issue #1560: a combat-damage step pre-advances the internal accumulator and
  // schedules a DEFERRED (impact-synced) animation. If the animation queue is
  // interrupted before that animation runs, the displayed number must still
  // reconcile to the authoritative settled life — it must never get stuck.
  it("reconciles the display when a deferred damage animation is cancelled (#1560)", () => {
    render(<LifeTotal playerId={0} />);
    expect(screen.getByText("20")).toBeInTheDocument();

    // Combat damage arrives: schedules the deferred impact animation, but it has
    // not run yet (impact timer pending).
    act(() => {
      useAnimationStore.setState({ activeStep: combatDamageStep(0, -3) });
    });
    // Display still 20 — the deferred animation hasn't fired.
    expect(screen.getByText("20")).toBeInTheDocument();

    // Queue is interrupted (e.g. a concurrent dispatch clears it) BEFORE the
    // impact timer fires — the deferred animation is cancelled, never runs.
    act(() => {
      useAnimationStore.setState({ activeStep: null });
    });

    // The engine's real life total settles in gameStore.
    act(() => {
      setLife(0, 17);
    });

    // Pre-fix this stayed at 20 (the desync). It must now show 17.
    expect(screen.getByText("17")).toBeInTheDocument();
    expect(screen.queryByText("20")).not.toBeInTheDocument();
  });

  it("reconciles the display for life changes with no animation step (instant speed)", () => {
    render(<LifeTotal playerId={0} />);
    expect(screen.getByText("20")).toBeInTheDocument();

    act(() => {
      setLife(0, 15);
    });

    expect(screen.getByText("15")).toBeInTheDocument();
  });
});
