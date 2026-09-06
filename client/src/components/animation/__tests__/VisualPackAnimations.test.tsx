import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { RefObject } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import type { AnimationStep } from "../../../animation/types.ts";
import { useCardImage } from "../../../hooks/useCardImage.ts";
import { currentSnapshot } from "../../../hooks/useGameDispatch.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";
import { AnimationOverlay } from "../AnimationOverlay.tsx";
import { CastArcAnimation } from "../CastArcAnimation.tsx";
import { MillRevealAnimation } from "../MillRevealAnimation.tsx";
import { RippleRevealAnimation } from "../RippleRevealAnimation.tsx";
import { RevealOverlay } from "../RevealOverlay.tsx";
import { visibleAnimationImageSnapshot } from "../ResolvedAnimationImage.tsx";

const imageMock = vi.hoisted(() => ({
  calls: [] as Array<[string, Record<string, unknown> | undefined]>,
  results: new Map<string, {
    src: string | null;
    isLoading: boolean;
    isRotated: boolean;
    isFlip: boolean;
    advanceFailedSource?: (src: string) => void;
  }>(),
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn((name: string, options?: Record<string, unknown>) => {
    imageMock.calls.push([name, options]);
    const key = String(options?.oracleId ?? name);
    return imageMock.results.get(key) ?? {
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    };
  }),
}));

vi.mock("../ParticleCanvas.tsx", () => ({ ParticleCanvas: () => null }));

const containerRef = { current: null } as RefObject<HTMLDivElement | null>;

function rect(x = 10, y = 20): DOMRect {
  return {
    x,
    y,
    width: 80,
    height: 112,
    top: y,
    right: x + 80,
    bottom: y + 112,
    left: x,
    toJSON: () => ({}),
  } as DOMRect;
}

function state(objects: GameObject[], players = buildPlayers([0, 1])): GameState {
  return buildGameState({ objects: buildObjectMap(...objects), players });
}

function visibleObject(overrides: Partial<GameObject>): GameObject {
  return buildGameObject({ display_visible_to_viewer: true, ...overrides });
}

function snapshot(object: GameObject) {
  const value = visibleAnimationImageSnapshot(object);
  if (!value) throw new Error("visible fixture did not produce a snapshot");
  return value;
}

function step(
  event: AnimationStep["effects"][number]["event"],
  duration = 500,
): AnimationStep {
  return { effects: [{ event, duration }], duration };
}

function seedOverlay(preState: GameState, postState: GameState, animationStep: AnimationStep) {
  act(() => {
    useGameStore.setState({ gameState: preState });
    useAnimationStore.getState().setAnimationNewState(postState);
    useAnimationStore.getState().enqueueSteps([animationStep]);
  });
}

beforeEach(() => {
  imageMock.calls.length = 0;
  imageMock.results.clear();
  currentSnapshot.clear();
  usePreferencesStore.setState({ vfxQuality: "full", animationSpeedMultiplier: 1 });
});

afterEach(() => {
  cleanup();
  useAnimationStore.getState().clearQueue();
  useGameStore.getState().reset();
  currentSnapshot.clear();
  vi.clearAllMocks();
  vi.useRealTimers();
});

describe("visual-pack animation consumers", () => {
  it("keeps a cast snapshot latched while advancing its exact normal source", () => {
    const object = visibleObject({
      id: 30,
      name: "Latched Cast Face",
      transformed: true,
      printed_ref: { oracle_id: "cast-oracle", face_name: "Latched Cast Face" },
    });
    const advance = vi.fn();
    imageMock.results.set("cast-oracle", {
      src: "cast-first.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
      advanceFailedSource: advance,
    });
    const castSnapshot = snapshot(object);
    const { rerender } = render(
      <CastArcAnimation
        from={{ x: 10, y: 10 }}
        to={{ x: 100, y: 100 }}
        snapshot={castSnapshot}
        mode="cast"
        onComplete={vi.fn()}
      />,
    );

    expect(vi.mocked(useCardImage)).toHaveBeenLastCalledWith(
      "Latched Cast Face",
      expect.objectContaining({
        size: "normal",
        faceIndex: 1,
        oracleId: "cast-oracle",
        faceName: "Latched Cast Face",
      }),
    );
    const first = screen.getByAltText("Latched Cast Face");
    expect(first).not.toHaveAttribute("srcset");
    fireEvent.error(first);
    expect(advance).toHaveBeenCalledWith("cast-first.png");

    imageMock.results.set("cast-oracle", {
      src: "cast-next.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    useGameStore.setState({ gameState: state([]) });
    rerender(
      <CastArcAnimation
        from={{ x: 10, y: 10 }}
        to={{ x: 100, y: 100 }}
        snapshot={castSnapshot}
        mode="cast"
        onComplete={vi.fn()}
      />,
    );
    expect(screen.getByAltText("Latched Cast Face")).toHaveAttribute(
      "src",
      "cast-next.png",
    );
  });

  it("uses the post-action cast identity and the pre-action stack identity", () => {
    const hiddenPre = buildGameObject({
      id: 40,
      name: "Secret Hand Name",
      zone: "Hand",
      display_visible_to_viewer: false,
      printed_ref: { oracle_id: "secret-pre", face_name: "Secret Hand Face" },
    });
    const publicPost = visibleObject({
      ...hiddenPre,
      zone: "Stack",
      name: "Public Stack Face",
      display_visible_to_viewer: true,
      printed_ref: { oracle_id: "public-post", face_name: "Public Stack Face" },
    });
    imageMock.results.set("public-post", {
      src: "public-post.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    currentSnapshot.set(hiddenPre.id, rect());
    seedOverlay(
      state([hiddenPre]),
      state([publicPost]),
      step({
        type: "SpellCast",
        data: { card_id: publicPost.card_id, controller: 0, object_id: publicPost.id },
      }),
    );
    const { unmount } = render(<AnimationOverlay containerRef={containerRef} />);
    expect(screen.getByAltText("Public Stack Face")).toHaveAttribute("src", "public-post.png");
    expect(document.body.innerHTML).not.toContain("Secret Hand");
    unmount();
    useAnimationStore.getState().clearQueue();
    imageMock.calls.length = 0;

    const stackSource = visibleObject({
      id: 41,
      zone: "Stack",
      name: "Current DFC Back",
      transformed: true,
      printed_ref: { oracle_id: "stack-pre", face_name: "Current DFC Back" },
    });
    imageMock.results.set("stack-pre", {
      src: "stack-pre.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    seedOverlay(
      state([stackSource]),
      state([]),
      step({
        type: "ZoneChanged",
        data: { object_id: stackSource.id, from: "Stack", to: "Graveyard" },
      }),
    );
    render(<AnimationOverlay containerRef={containerRef} />);
    expect(screen.getByAltText("Current DFC Back")).toHaveAttribute("src", "stack-pre.png");
    expect(vi.mocked(useCardImage)).toHaveBeenLastCalledWith(
      "Current DFC Back",
      expect.objectContaining({ size: "normal", faceIndex: 1, oracleId: "stack-pre" }),
    );
  });

  it("keeps same-name mill printings and token provenance distinct in event order", () => {
    vi.useFakeTimers();
    const first = visibleObject({
      id: 50,
      zone: "Graveyard",
      name: "Shared Name",
      printed_ref: { oracle_id: "mill-first", face_name: "Shared Name" },
    });
    const second = visibleObject({
      id: 51,
      zone: "Graveyard",
      name: "Shared Name",
      display_source: "Token",
      printed_ref: { oracle_id: "mill-token", face_name: "Shared Name" },
      token_image_ref: {
        scryfall_id: "mill-token-printing",
        scryfall_oracle_id: "mill-token",
        face_name: "Shared Name",
        preset_id: "mill-token-preset",
      },
    });
    imageMock.results.set("mill-first", {
      src: "mill-first.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    imageMock.results.set("mill-token", {
      src: "mill-token.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    const complete = vi.fn();
    const direct = render(
      <MillRevealAnimation
        cards={[
          { objectId: first.id, snapshot: snapshot(first), colors: ["#fff"] },
          { objectId: second.id, snapshot: snapshot(second), colors: ["#000"] },
        ]}
        from={{ x: 0, y: 0 }}
        to={{ x: 100, y: 100 }}
        onComplete={complete}
      />,
    );

    expect(screen.getAllByAltText("Shared Name").map((image) => image.getAttribute("src"))).toEqual([
      "mill-first.png",
      "mill-token.png",
    ]);
    expect(imageMock.calls.map(([, options]) => options?.oracleId)).toEqual([
      "mill-first",
      "mill-token",
    ]);
    expect(imageMock.calls[1]?.[1]).toEqual(expect.objectContaining({
      size: "normal",
      isToken: true,
      tokenImageRef: expect.objectContaining({ scryfall_id: "mill-token-printing" }),
    }));
    act(() => vi.advanceTimersByTime(1000));
    expect(complete).toHaveBeenCalledTimes(1);

    direct.unmount();
    imageMock.calls.length = 0;
    const hiddenFirst = { ...first, zone: "Library" as const, display_visible_to_viewer: false };
    const hiddenSecond = { ...second, zone: "Library" as const, display_visible_to_viewer: false };
    seedOverlay(
      state([hiddenFirst, hiddenSecond]),
      state([first, second]),
      {
        effects: [
          {
            event: {
              type: "ZoneChanged",
              data: { object_id: first.id, from: "Library", to: "Graveyard" },
            },
            duration: 400,
          },
          {
            event: {
              type: "ZoneChanged",
              data: { object_id: second.id, from: "Library", to: "Graveyard" },
            },
            duration: 400,
          },
        ],
        duration: 400,
      },
    );
    render(<AnimationOverlay containerRef={containerRef} />);
    expect(screen.getAllByAltText("Shared Name").map((image) => image.getAttribute("src"))).toEqual([
      "mill-first.png",
      "mill-token.png",
    ]);
    expect(imageMock.calls.map(([, options]) => options?.oracleId)).toEqual([
      "mill-first",
      "mill-token",
    ]);
  });

  it("fans a Ripple reveal and completes after the hold", () => {
    vi.useFakeTimers();
    const cards = [70, 71, 72].map((id) =>
      visibleObject({
        id,
        zone: "Library",
        name: `Ripple Card ${id}`,
        printed_ref: { oracle_id: `ripple-${id}`, face_name: `Ripple Card ${id}` },
      }),
    );
    for (const card of cards) {
      imageMock.results.set(`ripple-${card.id}`, {
        src: `ripple-${card.id}.png`,
        isLoading: false,
        isRotated: false,
        isFlip: false,
      });
    }
    const complete = vi.fn();
    render(
      <RippleRevealAnimation
        cards={cards.map((card) => ({
          objectId: card.id,
          snapshot: snapshot(card),
          colors: ["#f59e0b"],
        }))}
        from={{ x: 0, y: 0 }}
        onComplete={complete}
      />,
    );

    expect(
      screen
        .getAllByAltText(/Ripple Card/)
        .map((image) => image.getAttribute("src")),
    ).toEqual(["ripple-70.png", "ripple-71.png", "ripple-72.png"]);

    act(() => vi.advanceTimersByTime(4000));
    expect(complete).toHaveBeenCalledTimes(1);
  });

  it("drives a Ripple fan from a CardsRevealed step effect", () => {
    vi.useFakeTimers();
    const cards = [80, 81].map((id) =>
      visibleObject({
        id,
        zone: "Library",
        name: `Revealed ${id}`,
        printed_ref: { oracle_id: `revealed-${id}`, face_name: `Revealed ${id}` },
      }),
    );
    for (const card of cards) {
      imageMock.results.set(`revealed-${card.id}`, {
        src: `revealed-${card.id}.png`,
        isLoading: false,
        isRotated: false,
        isFlip: false,
      });
    }
    seedOverlay(
      state(cards),
      state(cards),
      step({
        type: "CardsRevealed",
        data: {
          player: 0,
          card_ids: [80, 81],
          card_names: ["Revealed 80", "Revealed 81"],
        },
      }),
    );
    render(<AnimationOverlay containerRef={containerRef} />);
    expect(
      screen.getAllByAltText(/Revealed 8/).map((image) => image.getAttribute("src")),
    ).toEqual(["revealed-80.png", "revealed-81.png"]);
  });

  it("filters hidden reveal identity before resolving two public small snapshots", () => {
    const publicA = visibleObject({
      id: 60,
      zone: "Library",
      name: "Public Reveal A",
      printed_ref: { oracle_id: "reveal-a", face_name: "Public Reveal A" },
    });
    const hidden = buildGameObject({
      id: 61,
      zone: "Library",
      name: "Secret Reveal",
      display_visible_to_viewer: false,
      printed_ref: { oracle_id: "secret-reveal", face_name: "Secret Face" },
      token_image_ref: {
        scryfall_id: "secret-token",
        scryfall_oracle_id: "secret-token-oracle",
        face_name: "Secret Token Face",
        preset_id: "secret-preset",
      },
    });
    const publicB = visibleObject({
      id: 62,
      zone: "Library",
      name: "Public Reveal B",
      printed_ref: { oracle_id: "reveal-b", face_name: "Public Reveal B" },
    });
    imageMock.results.set("reveal-a", {
      src: "reveal-a.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    imageMock.results.set("reveal-b", {
      src: "reveal-b.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    imageMock.results.set("secret-reveal", {
      src: "secret-now-public.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    useGameStore.setState({
      gameState: state(
        [publicA, hidden, publicB],
        buildPlayers([{ id: 0, library: [60, 61, 62] }, 1]),
      ),
    });
    useGameStore.setState((current) => ({
      gameState: current.gameState
        ? { ...current.gameState, revealed_cards: [60, 61, 62] }
        : null,
    }));
    const { container, rerender } = render(<RevealOverlay />);

    expect(screen.getByAltText("Public Reveal A")).toBeInTheDocument();
    expect(screen.getByAltText("Public Reveal B")).toBeInTheDocument();
    expect(container.innerHTML).not.toContain("Secret");
    expect(imageMock.calls).toHaveLength(2);
    for (const [, options] of imageMock.calls) {
      expect(options).toEqual(expect.objectContaining({ size: "small" }));
    }

    act(() => {
      useGameStore.setState((current) => ({
        gameState: current.gameState
          ? {
              ...current.gameState,
              objects: {
                ...current.gameState.objects,
                [hidden.id]: {
                  ...current.gameState.objects[hidden.id],
                  display_visible_to_viewer: true,
                },
              },
            }
          : null,
      }));
    });
    rerender(<RevealOverlay />);
    expect(screen.getByAltText("Secret Reveal")).toHaveAttribute(
      "src",
      "secret-now-public.png",
    );
    expect(vi.mocked(useCardImage)).toHaveBeenCalledWith(
      "Secret Reveal",
      expect.objectContaining({ size: "small", oracleId: "secret-reveal" }),
    );
  });
});
