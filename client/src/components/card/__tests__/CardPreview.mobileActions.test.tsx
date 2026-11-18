import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useCardImage } from "../../../hooks/useCardImage.ts";
import { type GameMode, useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { CardPreview } from "../CardPreview.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardBackImage: vi.fn(() => ({ src: "card-back.png", isLoading: false })),
  useCardImage: vi.fn(),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

// The mobile long-press preview is the subject: force the mobile branch.
vi.mock("../../../hooks/useIsMobile.ts", () => ({
  useIsMobile: () => true,
}));

const CARD_NAME = "Starship Enterprise";

function cardImage(isFlip: boolean) {
  return { src: "starship-enterprise.png", isLoading: false, isRotated: false, isFlip };
}

/** Inspect a battlefield card in a live game, which is what gives the preview
 *  its report context. */
function inspectInGame(gameMode: GameMode): void {
  const card = gameObjectFactory
    .onBattlefield()
    .named(CARD_NAME)
    .build({ printed_ref: { oracle_id: "oracle-starship-enterprise", face_name: CARD_NAME } });
  useGameStore.setState({
    gameId: "game-11001",
    gameMode,
    gameState: gameStateFactory.withPlayers(0, 1).withObjects(card).build(),
    spellCosts: {},
  });
  useUiStore.setState({ inspectedObjectId: card.id });
}

beforeEach(() => {
  vi.mocked(useCardImage).mockImplementation(() => cardImage(false));
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useGameStore.setState({ gameId: null, gameMode: null, gameState: null, spellCosts: {} });
  useUiStore.getState().dismissPreview();
  useUiStore.setState({ inspectedObjectId: null });
});

// Regression tests for #11001: Report and Flip were drawn on the card face,
// where they covered its rules text. They now sit in a strip beside the card.
describe("CardPreview mobile action strip", () => {
  it("puts Report outside the card face", () => {
    inspectInGame("ai");
    render(<CardPreview cardName={CARD_NAME} />);

    const cardFace = screen.getByAltText(CARD_NAME).parentElement;
    const report = screen.getByRole("button", { name: "Report a problem" });

    expect(cardFace).not.toBeNull();
    expect(cardFace).not.toContainElement(report);
    expect(report.closest("[data-card-preview-actions]")).not.toBeNull();
  });

  it("puts a flip card's Flip beside Report, outside the card face", () => {
    vi.mocked(useCardImage).mockImplementation(() => cardImage(true));
    inspectInGame("ai");
    render(<CardPreview cardName={CARD_NAME} />);

    const image = screen.getByAltText(CARD_NAME);
    const flip = screen.getByRole("button", { name: /Flip/ });
    const report = screen.getByRole("button", { name: "Report a problem" });

    expect(image.parentElement).not.toContainElement(flip);
    expect(flip.closest("[data-card-preview-actions]")).toContainElement(report);

    // Moving the button kept what it does: a tap turns the card over.
    fireEvent.click(flip);
    expect(image).toHaveClass("rotate-180");
  });

  it("keeps the preview open when an action is tapped", () => {
    const onDismiss = vi.fn();
    inspectInGame("ai");
    const { container } = render(<CardPreview cardName={CARD_NAME} onDismiss={onDismiss} />);

    const report = screen.getByRole("button", { name: "Report a problem" });
    fireEvent.pointerDown(report);
    expect(onDismiss).not.toHaveBeenCalled();

    // The strip, its padding included, sits inside the wrapper that stops the
    // event, so a tap on it does not dismiss either.
    const strip = report.closest<HTMLElement>("[data-card-preview-actions]");
    if (strip == null) throw new Error("the Report button is not in the action strip");
    fireEvent.pointerDown(strip);
    expect(onDismiss).not.toHaveBeenCalled();

    // happy-dom does no hit testing, so which areas take taps is pinned by
    // class: the wrapper is click-through (the space beside the card reads as
    // backdrop), and only the card and the strip, with 8px of padding past its
    // pills for near misses, take taps.
    const cardFrame = screen.getByAltText(CARD_NAME).parentElement;
    expect(cardFrame).toHaveClass("pointer-events-auto");
    expect(cardFrame?.parentElement).toHaveClass("pointer-events-none");
    expect(strip).toHaveClass("pointer-events-auto", "-m-2", "p-2");

    // Reach guard: a tap on the backdrop does dismiss, so the not-dismissed
    // assertions above are not passing because nothing listens.
    const backdrop = container.querySelector<HTMLElement>("[data-card-preview]");
    if (backdrop == null) throw new Error("the mobile preview rendered no backdrop");
    fireEvent.pointerDown(backdrop);
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("draws no action strip when the card has no action", () => {
    // Spectators do not report, and this card has no flip side.
    inspectInGame("spectate");
    render(<CardPreview cardName={CARD_NAME} />);

    expect(screen.getByAltText(CARD_NAME)).toBeInTheDocument();
    expect(screen.queryByRole("button")).toBeNull();
    expect(document.querySelector("[data-card-preview-actions]")).toBeNull();
  });
});
