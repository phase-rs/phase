import { act, cleanup, render } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { MillCard } from "../MillRevealAnimation.tsx";
import { RippleRevealAnimation } from "../RippleRevealAnimation.tsx";

interface CardMotion {
  children?: ReactNode;
  transition?: { duration: number; delay: number; times: number[] };
}

/** The fan's own motion transitions, recorded instead of animated. */
const motions = vi.hoisted(() => ({ cards: [] as CardMotion[] }));

vi.mock("framer-motion", async (importOriginal) => {
  const actual = await importOriginal<typeof import("framer-motion")>();
  // The factory is hoisted above this file's imports, so React can only be
  // reached through a dynamic import here.
  const { createElement } = await import("react");
  function MockMotionDiv(props: CardMotion) {
    motions.cards.push(props);
    return createElement("div", null, props.children);
  }
  return { ...actual, motion: { div: MockMotionDiv } };
});

vi.mock("../ResolvedAnimationImage.tsx", () => ({
  ResolvedAnimationImage: () => null,
}));

const CARDS: MillCard[] = [70, 71, 72].map((objectId) => ({
  objectId,
  snapshot: { objectId, cardName: `Ripple ${objectId}`, faceIndex: 0, isToken: false },
  colors: [],
}));

function ripple(cards: MillCard[], timeScale?: number) {
  const onComplete = vi.fn();
  const { container } = render(
    <RippleRevealAnimation
      cards={cards}
      from={{ x: 0, y: 0 }}
      timeScale={timeScale}
      onComplete={onComplete}
    />,
  );
  return { onComplete, container };
}

beforeEach(() => {
  vi.useFakeTimers();
  motions.cards.length = 0;
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("RippleRevealAnimation timeScale", () => {
  it("scales the stagger, flight and hold together, keeping the same shape", () => {
    ripple(CARDS, 0.5);

    const transitions = motions.cards.map((motion) => motion.transition!);
    // Three cards: 0 / 35 / 70 ms delays at half the 70 ms stagger.
    expect(transitions.map((transition) => transition.delay)).toEqual([0, 0.035, 0.07]);
    // The flight and its 1200 ms hold compress to 760 ms from 1520 ms.
    const total = (0.32 + 1.2) * 0.5;
    expect(transitions.map((transition) => transition.duration)).toEqual([total, total, total]);
    // The picture is unchanged: the same relative keyframe times.
    expect(transitions[0].times).toEqual([0, 0.32 / (0.32 + 1.2), 0.9, 1]);
  });

  it("scales the safety timeout, so the fan cannot outlive its step", () => {
    const { onComplete } = ripple(CARDS.slice(0, 2), 0.5);

    // The safety deadline is (70 + 320 + 1200 + 500) × 0.5 = 1045 ms.
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(onComplete).not.toHaveBeenCalled();

    act(() => {
      vi.advanceTimersByTime(50);
    });
    expect(onComplete).toHaveBeenCalledTimes(1);
  });

  it("completes at once at timeScale 0 and renders nothing", () => {
    const { onComplete, container } = ripple(CARDS, 0);

    expect(onComplete).toHaveBeenCalledTimes(1);
    expect(container.querySelectorAll("div")).toHaveLength(0);
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    expect(onComplete).toHaveBeenCalledTimes(1);
  });
});
