import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useCardBackImage, useCardImage } from "../../../hooks/useCardImage.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObject } from "../../../test/factories/gameObjectFactory.ts";
import { ArtCropCard } from "../ArtCropCard.tsx";
import { CardImage } from "../CardImage.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardBackImage: vi.fn(() => ({ src: "installed-back.png", isLoading: false })),
  useCardImage: vi.fn(),
}));
vi.mock("../../../hooks/useEngineCardData.ts", () => ({ useEngineCardData: () => null }));

const mockUseCardImage = vi.mocked(useCardImage);
const mockUseCardBackImage = vi.mocked(useCardBackImage);

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useGameStore.setState({ gameState: null });
});

describe("visual-pack card surfaces", () => {
  it("renders authored normal rungs and advances the exact visible source", () => {
    const advanceFailedSource = vi.fn();
    mockUseCardImage.mockReturnValue({
      src: "installed-normal.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
      rungs: { small: "installed-small.png", normal: "installed-normal.png" },
      advanceFailedSource,
    });

    render(<CardImage cardName="Public card" />);
    const image = screen.getByRole("img", { name: "Public card" });
    expect(image).toHaveAttribute(
      "srcset",
      "installed-small.png 146w, installed-normal.png 488w",
    );
    fireEvent.error(image);
    expect(advanceFailedSource).toHaveBeenCalledWith("installed-normal.png");
  });

  it("submits only marker identity and reaches the fixed back after exhaustion", () => {
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });

    render(
      <CardImage
        cardName="Secret face"
        oracleId="secret-oracle"
        faceName="Secret face"
        faceDown
        faceDownCause="Manifest"
      />,
    );

    expect(mockUseCardImage).toHaveBeenCalledWith("", expect.objectContaining({
      oracleId: undefined,
      faceName: undefined,
      isToken: true,
      tokenImageRef: expect.objectContaining({ scryfall_oracle_id: expect.any(String) }),
    }));
    expect(mockUseCardBackImage).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("img", { name: "Card back" })).toHaveAttribute(
      "src",
      "installed-back.png",
    );
    expect(document.body.textContent).not.toContain("Secret face");
  });

  it("requests strict art crop for cards and normal full-card art for tokens", () => {
    mockUseCardImage.mockReturnValue({
      src: null,
      isLoading: false,
      isRotated: false,
      isFlip: false,
    });
    const card = buildGameObject({ id: 11, zone: "Battlefield", name: "Card" });
    useGameStore.setState({ gameState: { objects: { 11: card } } as never });
    const { unmount } = render(<ArtCropCard objectId={11} />);
    expect(mockUseCardImage).toHaveBeenLastCalledWith(
      "Card",
      expect.objectContaining({ size: "art_crop", isToken: false }),
    );
    unmount();

    const token: GameObject = buildGameObject({
      id: 12,
      zone: "Battlefield",
      name: "Public token",
      display_source: "Token",
    });
    useGameStore.setState({ gameState: { objects: { 12: token } } as never });
    render(<ArtCropCard objectId={12} />);
    expect(mockUseCardImage).toHaveBeenLastCalledWith(
      "Public token",
      expect.objectContaining({ size: "normal", isToken: true }),
    );
  });
});
