import { cleanup, render, screen } from "@testing-library/react";
import { act } from "react";
import type { RefObject } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameEvent } from "../../../adapter/types.ts";
import type { AnimationStep } from "../../../animation/types.ts";
import { CARD_SLAM_FLIGHT_MS } from "../../../animation/types.ts";
import { normalizeEvents } from "../../../animation/eventNormalizer.ts";
import { currentSnapshot } from "../../../hooks/useGameDispatch.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { LifeTotal } from "../../controls/LifeTotal.tsx";
import { AnimationOverlay } from "../AnimationOverlay.tsx";

vi.mock("../ParticleCanvas.tsx", () => ({ ParticleCanvas: () => null }));

const containerRef = { current: null } as RefObject<HTMLDivElement | null>;

/** One attacker connecting with a player: the engine's event pair for that hit. */
function combatHit(sourceId: number, playerId: number, amount: number, newTotal: number): GameEvent[] {
  return [
    {
      type: "DamageDealt",
      data: { source_id: sourceId, target: { Player: playerId }, amount, is_combat: true },
    },
    { type: "LifeChanged", data: { player_id: playerId, amount: -amount, new_total: newTotal } },
  ];
}

function setLife(playerId: number, life: number) {
  useGameStore.setState((state) => {
    const previous = state.gameState ?? buildGameState();
    const players = previous.players.map(
      (player, index) => (index === playerId ? { ...player, life } : player),
    );
    return { gameState: { ...previous, players } };
  });
}

function playSteps(steps: AnimationStep[]) {
  act(() => {
    useAnimationStore.getState().enqueueSteps(steps);
  });
}

/**
 * Reproduces the reported combat: three 2/2s swing into a player at 20, and the
 * engine's snapshot is only committed once every step has played. The readout has
 * to tick per landed hit rather than hold at 20 and then jump to 14.
 */
describe("life totals during a damage animation", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    currentSnapshot.clear();
    useGameStore.setState({ gameState: buildGameState(), engineCommitEpoch: 1 });
    usePreferencesStore.setState({ vfxQuality: "full", animationSpeedMultiplier: 1 });
    setLife(1, 20);
  });

  afterEach(() => {
    cleanup();
    useAnimationStore.getState().clearQueue();
    useGameStore.getState().reset();
    currentSnapshot.clear();
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it("ticks down once per landed hit while the snapshot still reads 20", () => {
    const steps = normalizeEvents([
      ...combatHit(11, 1, 2, 18),
      ...combatHit(12, 1, 2, 16),
      ...combatHit(13, 1, 2, 14),
    ]);
    expect(steps).toHaveLength(3);

    render(
      <>
        <AnimationOverlay containerRef={containerRef} />
        <LifeTotal playerId={1} hideLabel />
      </>,
    );
    expect(screen.getByText("20")).toBeInTheDocument();

    playSteps(steps);
    for (const expected of ["18", "16", "14"]) {
      act(() => {
        vi.advanceTimersByTime(CARD_SLAM_FLIGHT_MS);
      });
      expect(screen.getByText(expected)).toBeInTheDocument();
      // The snapshot is still the pre-combat one — the tick came from the
      // engine's own per-event total, not from an early commit.
      expect(useGameStore.getState().gameState?.players[1].life).toBe(20);
      act(() => {
        vi.advanceTimersByTime(steps[0].duration);
      });
    }
  });

  it("holds the last landed total after the queue drains, until the snapshot commits", () => {
    const steps = normalizeEvents(combatHit(11, 1, 2, 18));

    render(
      <>
        <AnimationOverlay containerRef={containerRef} />
        <LifeTotal playerId={1} hideLabel />
      </>,
    );

    playSteps(steps);
    act(() => {
      vi.advanceTimersByTime(steps[0].duration * 2);
    });

    // `advanceStep` clears the active step on the overlay's own timer while the
    // dispatcher commits on a separate one; the readout must not flicker back to
    // 20 in whatever order those two land.
    expect(useAnimationStore.getState().activeStep).toBeNull();
    expect(screen.getByText("18")).toBeInTheDocument();

    act(() => {
      setLife(1, 18);
      useGameStore.setState((state) => ({ engineCommitEpoch: state.engineCommitEpoch + 1 }));
    });
    expect(screen.getByText("18")).toBeInTheDocument();
  });

  it("does not let a delayed hit overwrite a newer committed snapshot", () => {
    const steps = normalizeEvents(combatHit(11, 1, 2, 18));

    render(
      <>
        <AnimationOverlay containerRef={containerRef} />
        <LifeTotal playerId={1} hideLabel />
      </>,
    );

    playSteps(steps);

    // The hit has not landed yet, but a newer engine snapshot has. The delayed
    // impact still describes epoch 1 and must not be promoted into epoch 2.
    act(() => {
      setLife(1, 17);
      useGameStore.setState((state) => ({ engineCommitEpoch: state.engineCommitEpoch + 1 }));
      vi.advanceTimersByTime(CARD_SLAM_FLIGHT_MS);
    });

    expect(screen.getByText("17")).toBeInTheDocument();
    expect(screen.queryByText("18")).not.toBeInTheDocument();
    expect(useAnimationStore.getState().displayedLife?.epoch).toBe(1);
  });

  it("keeps the snapshot value for an event carrying no engine total", () => {
    const steps = normalizeEvents([
      {
        type: "DamageDealt",
        data: { source_id: 11, target: { Player: 1 }, amount: 2, is_combat: true },
      },
      // A peer older than `new_total` sends the amount alone; deriving 18 from it
      // is exactly what must not happen.
      { type: "LifeChanged", data: { player_id: 1, amount: -2 } },
    ]);

    render(
      <>
        <AnimationOverlay containerRef={containerRef} />
        <LifeTotal playerId={1} hideLabel />
      </>,
    );

    playSteps(steps);
    act(() => {
      vi.advanceTimersByTime(steps[0].duration * 2);
    });

    expect(screen.getByText("20")).toBeInTheDocument();
    expect(screen.queryByText("18")).not.toBeInTheDocument();
  });
});
