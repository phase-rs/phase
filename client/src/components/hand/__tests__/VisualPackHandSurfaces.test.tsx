import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useCardBackImage, useCardImage } from "../../../hooks/useCardImage.ts";
import { useCardHover } from "../../../hooks/useCardHover.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPlayers } from "../../../test/factories/gameStateFactory.ts";
import { ZONE_THEME } from "../../../viewmodel/zoneAffordance.ts";
import { CompanionFanCard } from "../CompanionFanCard.tsx";
import { MobileHandDrawer } from "../MobileHandDrawer.tsx";
import { OpponentHand } from "../OpponentHand.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardBackImage: vi.fn(() => ({ src: "installed-back.png", isLoading: false })),
  useCardImage: vi.fn(),
}));
vi.mock("../../../hooks/useCardHover.ts", () => ({
  useCardHover: vi.fn(() => ({ handlers: {}, firedRef: { current: false } })),
}));

const mockUseCardImage = vi.mocked(useCardImage);
const mockUseCardBackImage = vi.mocked(useCardBackImage);
const mockUseCardHover = vi.mocked(useCardHover);

function secretOpponent(): GameObject {
  return buildGameObject({
    id: 22,
    card_id: 22,
    owner: 1,
    controller: 1,
    zone: "Hand",
    name: "SECRET FACE",
    printed_ref: { oracle_id: "secret-oracle", face_name: "SECRET FACE" },
    token_image_ref: {
      scryfall_id: "secret-printing",
      scryfall_oracle_id: "secret-token-oracle",
      face_name: "SECRET TOKEN",
      preset_id: "secret-preset",
    },
  });
}

function seed(object: GameObject): void {
  useGameStore.setState({
    gameMode: "local",
    gameState: buildGameState({
      players: buildPlayers([0, { id: 1, hand: [object.id] }]),
      objects: buildObjectMap(object),
      seat_order: [0, 1],
    }),
  });
  useUiStore.setState({ focusedOpponent: 1 });
}

beforeEach(() => seed(secretOpponent()));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useUiStore.setState({ mobileHandOpen: false, focusedOpponent: null });
});

describe("visual-pack opponent hand boundary", () => {
  it("renders a fixed back without mounting face or hover authority", () => {
    const { container } = render(<OpponentHand playerId={1} />);

    expect(mockUseCardImage).not.toHaveBeenCalled();
    expect(mockUseCardHover).not.toHaveBeenCalled();
    expect(mockUseCardBackImage).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("img", { name: "Card back" })).toHaveAttribute(
      "src",
      "installed-back.png",
    );
    expect(container.innerHTML).not.toContain("SECRET");
    expect(container.innerHTML).not.toContain("secret-oracle");
  });

  it("uses projected visible identity and never degrades a failed face into a back", () => {
    const advanceFailedSource = vi.fn();
    mockUseCardImage.mockReturnValue({
      src: "installed-face.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
      rungs: { small: "installed-small.png", normal: "installed-normal.png" },
      advanceFailedSource,
    });

    const { rerender } = render(<OpponentHand playerId={1} showCards />);
    expect(mockUseCardImage).toHaveBeenCalledWith("SECRET FACE", expect.objectContaining({
      size: "small",
      oracleId: "secret-oracle",
      faceName: "SECRET FACE",
    }));
    expect(mockUseCardHover).toHaveBeenCalledWith(22);
    expect(mockUseCardBackImage).not.toHaveBeenCalled();
    const image = screen.getByRole("img", { name: "SECRET FACE" });
    fireEvent.error(image);
    expect(advanceFailedSource).toHaveBeenCalledWith("installed-face.png");

    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    rerender(<OpponentHand playerId={1} showCards />);
    expect(screen.getByRole("img", { name: "SECRET FACE" })).toHaveTextContent("SECRET FACE");
    expect(mockUseCardBackImage).not.toHaveBeenCalled();
  });

  it("honors engine-projected visibility independently of debug showCards", () => {
    const projected = { ...secretOpponent(), display_visible_to_viewer: true };
    seed(projected);
    mockUseCardImage.mockReturnValue({
      src: "projected-face.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(<OpponentHand playerId={1} />);

    expect(mockUseCardImage).toHaveBeenCalledWith(
      "SECRET FACE",
      expect.objectContaining({ oracleId: "secret-oracle", faceName: "SECRET FACE" }),
    );
    expect(mockUseCardBackImage).not.toHaveBeenCalled();
  });
});

describe("visual-pack owned hand surfaces", () => {
  it("keeps companion normal rungs coupled and advances the exact source", () => {
    const advanceFailedSource = vi.fn();
    mockUseCardImage.mockReturnValue({
      src: "installed-companion-normal.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
      rungs: {
        small: "installed-companion-small.png",
        normal: "installed-companion-normal.png",
      },
      advanceFailedSource,
    });

    render(
      <CompanionFanCard
        companion={{ card: { card: { name: "Lurrus" }, count: 1 }, used: false }}
        canActivate={false}
        theme={ZONE_THEME.companion}
        rotation={0}
        arcOffset={0}
        restingY={0}
        hoverY={0}
        marginLeft={0}
        zIndex={1}
      />,
    );

    expect(mockUseCardImage).toHaveBeenCalledWith("Lurrus", { size: "normal" });
    const image = screen.getByRole("img", { name: "Lurrus" });
    expect(image).toHaveAttribute(
      "srcset",
      "installed-companion-small.png 146w, installed-companion-normal.png 488w",
    );
    fireEvent.error(image);
    expect(advanceFailedSource).toHaveBeenCalledWith("installed-companion-normal.png");
  });

  it("forwards the mobile hand object's current face and token provenance", () => {
    const token = buildGameObject({
      id: 31,
      owner: 0,
      controller: 0,
      zone: "Hand",
      name: "Localized Spirit",
      display_source: "Token",
      printed_ref: {
        oracle_id: "current-oracle",
        face_name: "Localized Spirit",
      },
      token_image_ref: {
        scryfall_id: "token-printing",
        scryfall_oracle_id: "token-oracle",
        face_name: "Spirit",
        preset_id: "token-preset",
      },
    });
    useGameStore.setState({
      gameMode: "local",
      gameState: buildGameState({
        players: buildPlayers([{ id: 0, hand: [token.id] }, 1]),
        objects: buildObjectMap(token),
        seat_order: [0, 1],
      }),
      legalActionsByObject: {},
      spellCosts: {},
    });
    useUiStore.setState({ mobileHandOpen: true });
    const advanceFailedSource = vi.fn();
    mockUseCardImage.mockReturnValue({
      src: "installed-token-normal.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
      rungs: { small: "installed-token-small.png", normal: "installed-token-normal.png" },
      advanceFailedSource,
    });

    render(<MobileHandDrawer />);

    expect(mockUseCardImage).toHaveBeenCalledWith(
      "Localized Spirit",
      expect.objectContaining({
        size: "normal",
        oracleId: "current-oracle",
        faceName: "Localized Spirit",
        isToken: true,
        tokenImageRef: expect.objectContaining({ scryfall_id: "token-printing" }),
      }),
    );
    const image = screen.getByRole("img", { name: "Localized Spirit" });
    expect(image).toHaveAttribute(
      "srcset",
      "installed-token-small.png 146w, installed-token-normal.png 488w",
    );
    fireEvent.error(image);
    expect(advanceFailedSource).toHaveBeenCalledWith("installed-token-normal.png");
  });
});
