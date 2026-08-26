import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import {
  buildGameObject,
  buildObjectMap,
  gameObjectFactory,
} from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { PlayerHand } from "../../hand/PlayerHand.tsx";
import { GameCardPreview } from "../GameCardPreview.tsx";

// CardPreview renders <img alt={cardName} …>; mocking the image hook lets us
// assert the forwarded name without loading Scryfall assets. Mirrors the mocks
// in CardPreview.test.tsx.
vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({
    src: "card.png",
    isLoading: false,
    isRotated: false,
    isFlip: false,
  }),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

function battlefieldObject(overrides: Partial<GameObject> = {}): GameObject {
  return buildGameObject({
    id: 101,
    card_id: 1,
    zone: "Battlefield",
    name: "Pithing Needle",
    mana_cost: { type: "Cost", shards: [], generic: 1 },
    ...overrides,
  });
}

function gameStateWithObject(object: GameObject) {
  return buildGameState({
    objects: buildObjectMap(object),
    next_object_id: 102,
    battlefield: [object.id],
    next_timestamp: 2,
  });
}

function inspect(object: GameObject, faceIndex = 0): void {
  useGameStore.setState({ gameState: gameStateWithObject(object), spellCosts: {} });
  useUiStore.setState({ inspectedObjectId: object.id, inspectedFaceIndex: faceIndex });
}

afterEach(() => {
  cleanup();
  useGameStore.setState({ gameState: null, spellCosts: {} });
  useUiStore.setState({
    inspectedObjectId: null,
    inspectedFaceIndex: 0,
    previewPlacement: "cursor",
    isDragging: false,
    mobileHandGesture: null,
    shiftHeld: false,
    altHeld: false,
  });
  // GameCardPreview adds a third store; reset it so "shift" mode doesn't leak.
  usePreferencesStore.setState({ cardPreviewMode: "follow" });
});

describe("GameCardPreview", () => {
  it("forwards the inspected object's name to the preview", () => {
    inspect(battlefieldObject());

    render(<GameCardPreview />);

    expect(screen.getAllByAltText("Pithing Needle").length).toBeGreaterThan(0);
  });

  it("docks a preview opened from a modal even when cursor-follow is preferred", () => {
    inspect(battlefieldObject());
    useUiStore.setState({ previewPlacement: "side" });

    const { container } = render(<GameCardPreview />);

    expect(container.querySelector<HTMLElement>("[data-card-preview]")).toHaveStyle({
      right: "calc(env(safe-area-inset-right) + 1rem + var(--game-right-rail-offset, 0px))",
    });
  });

  it("anchors the preview to the hand card hovered through PlayerHand", async () => {
    const firstCard = gameObjectFactory
      .withId(201)
      .inHand()
      .named("First Card")
      .build();
    const hoveredCard = gameObjectFactory
      .withId(202)
      .inHand()
      .named("Hovered Card")
      .build();
    const gameState = gameStateFactory
      .withPlayers({ id: 0, hand: [firstCard.id, hoveredCard.id] }, 1)
      .withObjects(firstCard, hoveredCard)
      .build();
    useGameStore.setState({ gameState, spellCosts: {} });

    const { container } = render(
      <>
        <PlayerHand />
        <GameCardPreview />
      </>,
    );
    const firstSource = container.querySelector<HTMLElement>(
      `[data-hand-card][data-object-id="${firstCard.id}"]`,
    );
    const hoveredSource = container.querySelector<HTMLElement>(
      `[data-hand-card][data-object-id="${hoveredCard.id}"]`,
    );
    expect(firstSource).not.toBeNull();
    expect(hoveredSource).not.toBeNull();
    if (!firstSource || !hoveredSource) return;

    vi.spyOn(firstSource, "matches").mockReturnValue(false);
    vi.spyOn(hoveredSource, "matches").mockImplementation(
      (selector) => selector === ":hover",
    );
    vi.spyOn(hoveredSource, "getBoundingClientRect").mockReturnValue({
      bottom: 700,
      height: 140,
      left: 400,
      right: 500,
      top: 560,
      width: 100,
      x: 400,
      y: 560,
      toJSON: () => ({}),
    });
    Object.defineProperty(hoveredSource, "offsetWidth", {
      configurable: true,
      value: 100,
    });

    fireEvent.mouseEnter(hoveredSource);

    await waitFor(() => {
      const preview = container.querySelector<HTMLElement>("[data-card-preview]");
      expect(preview).not.toBeNull();
      expect(preview).toHaveStyle({ bottom: "0px" });
      expect(within(preview!).getByAltText("Hovered Card")).toBeInTheDocument();
    });
  });

  it("keeps the held card in the stable fan until direct dragging begins", () => {
    const card = gameObjectFactory
      .withId(203)
      .inHand()
      .named("Held Card")
      .build();
    const gameState = gameStateFactory
      .withPlayers({ id: 0, hand: [card.id] }, 1)
      .withObjects(card)
      .build();
    useGameStore.setState({ gameState, spellCosts: {} });

    const { container } = render(<PlayerHand />);
    const source = container.querySelector<HTMLElement>(
      `[data-hand-card][data-object-id="${card.id}"]`,
    );
    expect(source).not.toBeNull();

    const sourceOrigin = {
      bottom: 700,
      centerX: 450,
      height: 140,
      rotation: 0,
      top: 560,
      width: 100,
    };
    act(() => {
      useUiStore.getState().setMobileHandGesture({
        objectId: card.id,
        phase: "preview",
        sourceOrigin,
        offsetX: 0,
        offsetY: 0,
        playable: true,
        castReady: false,
      });
    });

    expect(source).not.toHaveAttribute("data-hand-held-source");
    expect(source).not.toHaveClass("w-0", "opacity-0");

    act(() => {
      useUiStore.getState().setMobileHandGesture({
        objectId: card.id,
        phase: "drag",
        sourceOrigin,
        offsetX: 0,
        offsetY: -24,
        playable: true,
        castReady: false,
      });
    });

    expect(source).toHaveAttribute("data-hand-held-source", "true");
    expect(source).toHaveClass("w-0", "opacity-0");
  });

  it("renders no preview while a card is being dragged", () => {
    inspect(battlefieldObject());
    useUiStore.setState({ isDragging: true });

    const { container } = render(<GameCardPreview />);

    expect(container.firstChild).toBeNull();
    expect(screen.queryByAltText("Pithing Needle")).toBeNull();
  });

  it("suppresses the preview in shift mode when Shift is not held", () => {
    inspect(battlefieldObject());
    usePreferencesStore.setState({ cardPreviewMode: "shift" });
    useUiStore.setState({ shiftHeld: false });

    const { container } = render(<GameCardPreview />);

    expect(container.firstChild).toBeNull();

    // Holding Shift reveals it.
    cleanup();
    useUiStore.setState({ shiftHeld: true });
    render(<GameCardPreview />);
    expect(screen.getAllByAltText("Pithing Needle").length).toBeGreaterThan(0);
  });

  it("shows the back-face name when inspecting face index 1", () => {
    const dfc = battlefieldObject({
      name: "Delver of Secrets",
      back_face: {
        name: "Insectile Aberration",
        power: 3,
        toughness: 2,
        card_types: { supertypes: [], core_types: ["Creature"], subtypes: ["Human", "Insect"] },
        mana_cost: { type: "Cost", shards: [], generic: 0 },
        keywords: ["Flying"],
        abilities: [],
        color: ["Blue"],
      },
    });
    inspect(dfc, 1);

    render(<GameCardPreview />);

    expect(screen.getAllByAltText("Insectile Aberration").length).toBeGreaterThan(0);
  });

  it("previews a markerless face-down permanent as the generic back — identity stays hidden (#7551 review)", () => {
    // No recorded cause (older saves): there is no marker printing, but the
    // hover must still answer — with the generic card back, which reveals
    // nothing (CR 708.2a: the public face is a blank 2/2).
    inspect(battlefieldObject({ face_down: true }));

    render(<GameCardPreview />);

    expect(screen.getAllByAltText("Face-down card").length).toBeGreaterThan(0);
    expect(screen.queryByAltText("Pithing Needle")).toBeNull();
  });

  it("previews the Ixidron class (TurnedFaceDown) as the generic back (#7551 review)", () => {
    // `TurnedFaceDown` (an effect turned it face down, CR 708.2a) has no
    // printed marker token — same generic-back path as the unknown cause.
    inspect(
      battlefieldObject({
        face_down: true,
        face_down_cause: "TurnedFaceDown" as never,
        name: "",
      }),
    );

    render(<GameCardPreview />);

    expect(screen.getAllByAltText("Face-down card").length).toBeGreaterThan(0);
    expect(screen.queryByAltText("Pithing Needle")).toBeNull();
  });

  it("previews a face-down permanent when the engine projects its identity", () => {
    inspect(battlefieldObject({ face_down: true, display_visible_to_viewer: true }));

    render(<GameCardPreview />);

    expect(screen.getAllByAltText("Pithing Needle").length).toBeGreaterThan(0);
  });

  it("peeks the STORED face of the viewer's own face-down permanent (#7547)", () => {
    // The live face is blanked per CR 708.2a; the preview is the CR 708.5
    // peek, so it resolves the stored real face — on any hovered face index.
    inspect(
      battlefieldObject({
        face_down: true,
        display_visible_to_viewer: true,
        name: "",
        back_face: { name: "Hooded Hydra", layout_kind: null } as never,
      }),
    );

    render(<GameCardPreview />);

    expect(screen.getAllByAltText("Hooded Hydra").length).toBeGreaterThan(0);
  });

  it("previews an OPPONENT's face-down permanent as its cause marker (#7547)", () => {
    // The identity stays hidden; the marker carries the mechanic's reminder
    // text, which is exactly what an opponent may know.
    inspect(
      battlefieldObject({
        face_down: true,
        face_down_cause: "Morph" as never,
        name: "",
      }),
    );

    render(<GameCardPreview />);

    expect(screen.getAllByAltText("Morph").length).toBeGreaterThan(0);
    expect(screen.queryByAltText("Pithing Needle")).toBeNull();
  });
});
