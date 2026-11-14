import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useCardBackImage, useCardImage } from "../../../hooks/useCardImage.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";
import { GraveyardPile } from "../GraveyardPile.tsx";
import { LibraryPile } from "../LibraryPile.tsx";

const imageMock = vi.hoisted(() => ({
  back: {
    src: "installed-back.png" as string | null,
    isLoading: false,
    advanceFailedSource: vi.fn(),
  },
  calls: [] as Array<[string, Record<string, unknown> | undefined]>,
  hoverIds: [] as number[],
  results: new Map<string, {
    src: string | null;
    isLoading: boolean;
    isRotated: boolean;
    isFlip: boolean;
    rungs?: { small: string; normal: string };
    advanceFailedSource?: (src: string) => void;
  }>(),
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardBackImage: vi.fn(() => imageMock.back),
  useCardImage: vi.fn((name: string, options?: Record<string, unknown>) => {
    imageMock.calls.push([name, options]);
    return imageMock.results.get(name) ?? {
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    };
  }),
}));

vi.mock("../../../hooks/useInspectHoverProps.ts", () => ({
  useInspectHoverProps: () => (id: number) => ({
    onMouseEnter: () => imageMock.hoverIds.push(id),
  }),
}));

vi.mock("../../../hooks/usePlayerId.ts", () => ({
  useCanActForWaitingState: () => true,
  usePlayerId: () => 0,
}));

vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => vi.fn(),
}));

function card(id: number, zone: GameObject["zone"], name: string): GameObject {
  return buildGameObject({
    id,
    card_id: id,
    zone,
    name,
    display_visible_to_viewer: true,
    printed_ref: { oracle_id: `oracle-${id}`, face_name: name },
  });
}

function seed(options: {
  graveyard?: GameObject[];
  library?: GameObject[];
  extra?: GameObject[];
}) {
  const graveyard = options.graveyard ?? [];
  const library = options.library ?? [];
  const objects = [...graveyard, ...library, ...(options.extra ?? [])];
  const gameState = buildGameState({
    objects: buildObjectMap(...objects),
    players: buildPlayers([{
      id: 0,
      graveyard: graveyard.map((object) => object.id),
      library: library.map((object) => object.id),
    }]),
    revealed_cards: [],
  });
  act(() => {
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActionsByObject: {},
    });
  });
}

beforeEach(() => {
  imageMock.back = {
    src: "installed-back.png",
    isLoading: false,
    advanceFailedSource: vi.fn(),
  };
  imageMock.calls.length = 0;
  imageMock.hoverIds.length = 0;
  imageMock.results.clear();
});

afterEach(() => {
  cleanup();
  useGameStore.getState().reset();
  vi.clearAllMocks();
});

describe("visual-pack pile surfaces", () => {
  it("preserves the graveyard current face and advances through authored rungs", () => {
    const advanceFirst = vi.fn();
    const top = card(10, "Graveyard", "Current Back Face");
    top.transformed = true;
    top.printed_ref = { oracle_id: "dfc-oracle", face_name: "Current Back Face" };
    seed({ graveyard: [top] });
    imageMock.results.set("Current Back Face", {
      src: "grave-normal-1.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
      rungs: { small: "grave-small-1.png", normal: "grave-normal-1.png" },
      advanceFailedSource: advanceFirst,
    });

    const { rerender } = render(<GraveyardPile playerId={0} onClick={vi.fn()} />);
    const first = screen.getByAltText("Current Back Face");
    expect(vi.mocked(useCardImage)).toHaveBeenCalledWith(
      "Current Back Face",
      expect.objectContaining({
        faceIndex: 1,
        oracleId: "dfc-oracle",
        faceName: "Current Back Face",
      }),
    );
    expect(first).toHaveAttribute(
      "srcset",
      "grave-small-1.png 146w, grave-normal-1.png 488w",
    );
    fireEvent.error(first);
    expect(advanceFirst).toHaveBeenCalledWith("grave-normal-1.png");

    imageMock.results.set("Current Back Face", {
      src: "grave-normal-2.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    rerender(<GraveyardPile playerId={0} onClick={vi.fn()} />);
    expect(screen.getByAltText("Current Back Face")).toHaveAttribute(
      "src",
      "grave-normal-2.png",
    );

    imageMock.results.set("Current Back Face", {
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    rerender(<GraveyardPile playerId={0} onClick={vi.fn()} />);
    expect(screen.getByRole("img", { name: "Current Back Face" })).toHaveTextContent(
      "Current Back Face",
    );
  });

  it("keeps a hostile hidden library top out of face and hover hooks, then binds it when authorized", () => {
    const hidden = card(20, "Library", "Secret Library Face");
    hidden.display_visible_to_viewer = false;
    hidden.printed_ref = { oracle_id: "secret-oracle", face_name: "Secret Library Face" };
    hidden.back_face = {
      name: "Secret Other Face",
      power: null,
      toughness: null,
      card_types: { supertypes: [], core_types: ["Sorcery"], subtypes: [] },
      mana_cost: { type: "NoCost" },
      keywords: [],
      abilities: [],
      color: [],
      printed_ref: { oracle_id: "secret-back-oracle", face_name: "Secret Other Face" },
    };
    hidden.display_source = "Token";
    hidden.token_image_ref = {
      scryfall_id: "secret-token-printing",
      scryfall_oracle_id: "secret-token-oracle",
      face_name: "Secret Token Face",
      preset_id: "secret-preset",
    };
    const adjacent = card(21, "Library", "Secret Library Face");
    seed({ library: [hidden], extra: [adjacent] });

    const { container, rerender } = render(<LibraryPile playerId={0} />);
    const back = screen.getByRole("img", { name: "Card back" });
    expect(back).toHaveAttribute("src", "installed-back.png");
    fireEvent.mouseEnter(screen.getByRole("button"));
    expect(vi.mocked(useCardImage)).not.toHaveBeenCalled();
    expect(imageMock.hoverIds).toEqual([]);
    const hiddenDom = container.innerHTML;
    for (const secret of [
      "Secret Library Face",
      "secret-oracle",
      "Secret Other Face",
      "secret-back-oracle",
      "secret-token-printing",
      "secret-token-oracle",
      "Secret Token Face",
      "secret-preset",
    ]) {
      expect(hiddenDom).not.toContain(secret);
    }
    fireEvent.error(back);
    expect(imageMock.back.advanceFailedSource).toHaveBeenCalledWith("installed-back.png");

    imageMock.back = { src: null, isLoading: false, advanceFailedSource: vi.fn() };
    rerender(<LibraryPile playerId={0} />);
    expect(screen.getByRole("img", { name: "Card back" }).tagName).toBe("DIV");
    expect(vi.mocked(useCardImage)).not.toHaveBeenCalled();

    const visibleAdvance = vi.fn();
    const cardBackCallCount = vi.mocked(useCardBackImage).mock.calls.length;
    imageMock.results.set("Secret Library Face", {
      src: "visible-normal.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
      rungs: { small: "visible-small.png", normal: "visible-normal.png" },
      advanceFailedSource: visibleAdvance,
    });
    act(() => {
      useGameStore.setState((state) => ({
        gameState: state.gameState
          ? {
              ...state.gameState,
              objects: {
                ...state.gameState.objects,
                [hidden.id]: { ...state.gameState.objects[hidden.id], display_visible_to_viewer: true },
              },
            }
          : null,
      }));
    });
    rerender(<LibraryPile playerId={0} />);
    expect(vi.mocked(useCardImage)).toHaveBeenCalledWith(
      "Secret Library Face",
      expect.objectContaining({
        oracleId: "secret-oracle",
        faceName: "Secret Library Face",
        tokenImageRef: expect.objectContaining({ scryfall_id: "secret-token-printing" }),
      }),
    );
    const face = screen.getByAltText("Secret Library Face");
    expect(face).toHaveAttribute(
      "srcset",
      "visible-small.png 146w, visible-normal.png 488w",
    );
    fireEvent.error(face);
    expect(visibleAdvance).toHaveBeenCalledWith("visible-normal.png");

    imageMock.results.set("Secret Library Face", {
      src: "visible-next-normal.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    rerender(<LibraryPile playerId={0} />);
    expect(screen.getByAltText("Secret Library Face")).toHaveAttribute(
      "src",
      "visible-next-normal.png",
    );

    imageMock.results.set("Secret Library Face", {
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    rerender(<LibraryPile playerId={0} />);
    expect(screen.getByRole("img", { name: "Secret Library Face" })).toHaveTextContent(
      "Secret Library Face",
    );
    expect(screen.queryByRole("img", { name: "Card back" })).not.toBeInTheDocument();
    expect(vi.mocked(useCardBackImage)).toHaveBeenCalledTimes(cardBackCallCount);

    fireEvent.mouseEnter(screen.getByRole("button"));
    expect(imageMock.hoverIds).toEqual([hidden.id]);
  });
});
