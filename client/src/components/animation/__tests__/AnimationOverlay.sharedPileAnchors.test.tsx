import { act, cleanup, render } from "@testing-library/react";
import type { RefObject } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import type { AnimationStep } from "../../../animation/types.ts";
import { currentSnapshot } from "../../../hooks/useGameDispatch.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";
import { AnimationOverlay } from "../AnimationOverlay.tsx";

type Point = { x: number; y: number };

const recorded = vi.hoisted(() => ({
  mill: [] as Array<{ from: Point; to: Point }>,
  ripple: [] as Array<{ from: Point }>,
}));

vi.mock("../MillRevealAnimation.tsx", () => ({
  MillRevealAnimation: (props: { from: Point; to: Point }) => {
    recorded.mill.push({ from: props.from, to: props.to });
    return null;
  },
}));

vi.mock("../RippleRevealAnimation.tsx", () => ({
  RippleRevealAnimation: (props: { from: Point }) => {
    recorded.ripple.push({ from: props.from });
    return null;
  },
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: null, isLoading: false, isRotated: false, isFlip: false }),
}));

vi.mock("../ParticleCanvas.tsx", async () => {
  const { forwardRef } = await import("react");
  return { ParticleCanvas: forwardRef(() => null) };
});

const containerRef = { current: null } as RefObject<HTMLDivElement | null>;

const SHARED = { shared_piles: { library: 0, graveyard: 0 } };

function pileNode(attr: string, seat: number, x: number, y: number): Point {
  const el = document.createElement("div");
  el.setAttribute(attr, String(seat));
  el.getBoundingClientRect = () =>
    ({ x, y, width: 80, height: 112, top: y, left: x, right: x + 80, bottom: y + 112 }) as DOMRect;
  document.body.appendChild(el);
  return { x: x + 40, y: y + 56 };
}

function stateWith(derived: GameState["derived"], ownerOfCard: number): GameState {
  const card = buildGameObject({ id: 50, owner: ownerOfCard, zone: "Graveyard" });
  return buildGameState({
    objects: buildObjectMap(card),
    players: buildPlayers([0, 1]),
    derived,
  });
}

function seed(pre: GameState, post: GameState, effectEvent: AnimationStep["effects"][number]["event"]) {
  act(() => {
    useGameStore.setState({ gameState: pre });
    useAnimationStore.getState().setAnimationNewState(post);
    useAnimationStore
      .getState()
      .enqueueSteps([{ effects: [{ event: effectEvent, duration: 500 }], duration: 500 }], 1);
  });
}

const mill = (): AnimationStep["effects"][number]["event"] => ({
  type: "ZoneChanged",
  data: { object_id: 50, from: "Library", to: "Graveyard" },
});

beforeEach(() => {
  recorded.mill.length = 0;
  recorded.ripple.length = 0;
  currentSnapshot.clear();
  usePreferencesStore.setState({ vfxQuality: "full", animationSpeedMultiplier: 1 });
});

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  useAnimationStore.getState().clearQueue();
  useGameStore.getState().reset();
  vi.clearAllMocks();
});

describe("AnimationOverlay shared-pile anchors", () => {
  it.each([0, 1])("flies a card owned by seat %i from the shared library to the shared graveyard", (owner) => {
    const from = pileNode("data-library-pile", 0, 10, 20);
    const to = pileNode("data-graveyard-pile", 0, 300, 20);
    const state = stateWith(SHARED, owner);
    seed(state, state, mill());

    render(<AnimationOverlay containerRef={containerRef} />);

    expect(recorded.mill).toEqual([{ from, to }]);
  });

  it("anchors a revealed top card on the shared library for the non-holder seat", () => {
    const from = pileNode("data-library-pile", 0, 10, 20);
    const state = stateWith(SHARED, 1);
    seed(state, state, {
      type: "CardsRevealed",
      data: { player: 1, card_ids: [50], card_names: ["Card"] },
    } as AnimationStep["effects"][number]["event"]);

    render(<AnimationOverlay containerRef={containerRef} />);

    expect(recorded.ripple).toEqual([{ from }]);
  });

  it("keeps resolving each seat's own piles when nothing is shared", () => {
    pileNode("data-library-pile", 0, 10, 20);
    const from = pileNode("data-library-pile", 1, 10, 400);
    const to = pileNode("data-graveyard-pile", 1, 300, 400);
    const state = stateWith(undefined, 1);
    seed(state, state, mill());

    render(<AnimationOverlay containerRef={containerRef} />);

    expect(recorded.mill).toEqual([{ from, to }]);
  });
});
