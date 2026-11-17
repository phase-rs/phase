import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useCardImage } from "../../../hooks/useCardImage.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { ArtCropCard } from "../ArtCropCard.tsx";
import { CardArtFallback } from "../CardArtFallback.tsx";
import { CardImage } from "../CardImage.tsx";

// Regression tests for #11008: an artless tile offers a one-tap reload where
// the tile owns its taps, and the tap never reaches the surface around it.

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardBackImage: vi.fn(() => ({ src: "card-back.png", isLoading: false })),
  useCardImage: vi.fn(),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useLocalizedCardName: (name: string | null) => name,
}));

const viewport = vi.hoisted(() => ({ isMobile: false }));

vi.mock("../../../hooks/useIsMobile.ts", () => ({
  useIsMobile: () => viewport.isMobile,
}));

const CARD_NAME = "Etherium Sculptor";
const RULES_TEXT = "Artifact spells you cast cost {1} less to cast.";
const RELOAD = "Tap to reload art";

/** The settled artless result: the art failed and the lookup is over. */
function failedArt(reload: () => void) {
  return { src: null, isLoading: false, isRotated: false, isFlip: false, reload };
}

function placeSculptorOnBattlefield(): number {
  const card = gameObjectFactory
    .creature(1, 2)
    .onBattlefield()
    .named(CARD_NAME)
    .build({ printed_ref: { oracle_id: "oracle-etherium-sculptor", face_name: CARD_NAME } });
  useGameStore.setState({
    gameState: gameStateFactory.withPlayers(0, 1).withObjects(card).build(),
  });
  return card.id;
}

beforeEach(() => {
  viewport.isMobile = false;
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useGameStore.setState({ gameState: null });
});

describe("CardArtFallback reload", () => {
  it("keeps the tap on its 44px button and off the surface around the tile", () => {
    const onReload = vi.fn();
    const onSurfacePointerDown = vi.fn();
    const onSurfaceClick = vi.fn();
    render(
      <div onPointerDown={onSurfacePointerDown} onClick={onSurfaceClick}>
        <CardArtFallback name={CARD_NAME} oracleText={RULES_TEXT} className="h-40 w-28" onReload={onReload} />
      </div>,
    );

    const reload = screen.getByRole("button", { name: RELOAD });
    expect(reload).toHaveClass("min-h-11");
    fireEvent.pointerDown(reload);
    fireEvent.click(reload);
    expect(onReload).toHaveBeenCalledTimes(1);
    expect(onSurfacePointerDown).not.toHaveBeenCalled();
    expect(onSurfaceClick).not.toHaveBeenCalled();

    // Reach guard: a tap on the tile itself does reach the surface, so the
    // assertions above are not passing because nothing listens.
    const tile = screen.getByRole("img", { name: CARD_NAME });
    fireEvent.pointerDown(tile);
    fireEvent.click(tile);
    expect(onSurfacePointerDown).toHaveBeenCalledTimes(1);
    expect(onSurfaceClick).toHaveBeenCalledTimes(1);
  });

  it("keeps the button outside the image role, so assistive tech can reach it", () => {
    render(<CardArtFallback name={CARD_NAME} oracleText={RULES_TEXT} className="h-40 w-28" onReload={vi.fn()} />);

    const tile = screen.getByRole("img", { name: CARD_NAME });
    expect(tile).toHaveTextContent("less to cast.");
    expect(tile).not.toContainElement(screen.getByRole("button", { name: RELOAD }));
  });

  it("draws no button without a reload", () => {
    render(<CardArtFallback name={CARD_NAME} className="h-40 w-28" />);

    expect(screen.getByRole("img", { name: CARD_NAME })).toHaveTextContent(CARD_NAME);
    expect(screen.queryByRole("button")).toBeNull();
  });
});

describe("ArtCropCard art reload", () => {
  it("offers the reload on a desktop tile whose art failed", () => {
    const reload = vi.fn();
    vi.mocked(useCardImage).mockReturnValue(failedArt(reload));
    render(<ArtCropCard objectId={placeSculptorOnBattlefield()} />);

    fireEvent.click(screen.getByRole("button", { name: RELOAD }));
    expect(reload).toHaveBeenCalledTimes(1);
    // The button takes the name's place in the art box; the frame header still
    // shows the name, and the tile keeps it as its label.
    expect(screen.getByRole("img", { name: CARD_NAME })).toBeInTheDocument();
  });

  it("leaves a phone's tile to the game: the long-press preview carries the reload", () => {
    viewport.isMobile = true;
    vi.mocked(useCardImage).mockReturnValue(failedArt(vi.fn()));
    render(<ArtCropCard objectId={placeSculptorOnBattlefield()} />);

    // Reach guard: the artless tile is on screen.
    expect(screen.getByRole("img", { name: CARD_NAME })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: RELOAD })).toBeNull();
  });
});

describe("CardImage art reload", () => {
  it("offers the reload only when the caller opts in", () => {
    const reload = vi.fn();
    vi.mocked(useCardImage).mockReturnValue(failedArt(reload));
    const { rerender } = render(<CardImage cardName={CARD_NAME} />);

    // Reach guard: the artless tile is on screen without the opt-in.
    expect(screen.getByRole("img", { name: CARD_NAME })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: RELOAD })).toBeNull();

    rerender(<CardImage cardName={CARD_NAME} offerArtReload />);
    fireEvent.click(screen.getByRole("button", { name: RELOAD }));
    expect(reload).toHaveBeenCalledTimes(1);
  });
});
