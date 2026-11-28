import { act, cleanup, render } from "@testing-library/react";
import type { RefObject } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AnimationStep } from "../../../animation/types.ts";
import { currentSnapshot } from "../../../hooks/useGameDispatch.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildObjectMap, gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { AnimationOverlay } from "../AnimationOverlay.tsx";
import type { ParticleCanvasHandle } from "../ParticleCanvas.tsx";
import { DeathShatter } from "../DeathShatter.tsx";
import { RippleRevealAnimation } from "../RippleRevealAnimation.tsx";

/** Card images whose readiness the test controls: a shatter's death is
 *  presented before its image resolves. */
const ready = vi.hoisted(() => ({ pending: [] as Array<() => void> }));

vi.mock("../ResolvedAnimationImage.tsx", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../ResolvedAnimationImage.tsx")>();
  // The factory is hoisted above this file's imports, so React can only be
  // reached through a dynamic import here.
  const { useEffect } = await import("react");
  const MockResolvedAnimationImage = ({ onReady }: { onReady?: (image: HTMLImageElement) => void }) => {
    useEffect(() => {
      if (onReady) ready.pending.push(() => onReady(document.createElement("img")));
    }, [onReady]);
    return null;
  };
  return { ...actual, ResolvedAnimationImage: MockResolvedAnimationImage };
});

vi.mock("../DeathShatter.tsx", () => ({ DeathShatter: vi.fn(() => null) }));
vi.mock("../RippleRevealAnimation.tsx", () => ({ RippleRevealAnimation: vi.fn(() => null) }));

vi.mock("../ParticleCanvas.tsx", async () => {
  // Hoisted above this file's imports, so React is dynamically imported here.
  const React = await import("react");
  const stub = new Proxy({}, { get: () => vi.fn() }) as unknown as ParticleCanvasHandle;
  return {
    ParticleCanvas: React.forwardRef<ParticleCanvasHandle>((_props, ref) => {
      React.useImperativeHandle(ref, () => stub);
      return null;
    }),
  };
});

const X = 7;
const containerRef = { current: null } as RefObject<HTMLDivElement | null>;
const elf = gameObjectFactory.withId(X).named("Llanowar Elves").creature(1, 1)
  .onBattlefield().params({ display_visible_to_viewer: true }).build();

function seed(event: AnimationStep["effects"][number]["event"], duration = 500) {
  currentSnapshot.set(X, new DOMRect(40, 600, 63, 88));
  const state = buildGameState({ objects: buildObjectMap(elf) });
  act(() => {
    useGameStore.setState({ gameState: state });
    useAnimationStore.getState().setAnimationNewState(state);
    useAnimationStore.getState().enqueueSteps([{ effects: [{ event, duration }], duration }], 1);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  ready.pending.length = 0;
  currentSnapshot.clear();
  usePreferencesStore.setState({
    animationSpeedMultiplier: 1,
    vfxQuality: "full",
    cardAnimationStyle: "classic",
  });
});

afterEach(() => {
  cleanup();
  useAnimationStore.getState().clearQueue();
  useGameStore.getState().reset();
  usePreferencesStore.setState({
    animationSpeedMultiplier: 1,
    vfxQuality: "full",
    cardAnimationStyle: "classic",
  });
  currentSnapshot.clear();
  vi.clearAllMocks();
  vi.useRealTimers();
});

describe("AnimationOverlay effect timeScale", () => {
  it("keeps the pace a death was presented at when its image resolves after a pace change", () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 0.4 });
    seed({ type: "CreatureDestroyed", data: { object_id: X, source_id: null } });
    render(<AnimationOverlay containerRef={containerRef} />);
    expect(ready.pending).toHaveLength(1);

    // The image has not resolved; the death is already recorded at the pace it
    // was presented at. A later pace must not retime the shatter it becomes.
    act(() => usePreferencesStore.setState({ animationSpeedMultiplier: 1 }));
    act(() => {
      for (const settle of ready.pending.splice(0)) settle();
    });

    expect(vi.mocked(DeathShatter).mock.calls[0]?.[0].timeScale).toBe(0.4);
  });

  it("caps a ripple reveal at timeScale 1 when the speed preference exceeds 1", () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 1.5 });
    seed({ type: "CardsRevealed", data: { player: 0, card_names: ["Opt"] } });
    render(<AnimationOverlay containerRef={containerRef} />);

    const scales = vi.mocked(RippleRevealAnimation).mock.calls.map(([props]) => props.timeScale);
    expect(scales).toEqual([1]);
  });
});
