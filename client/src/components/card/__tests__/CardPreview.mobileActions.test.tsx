import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useCardImage } from "../../../hooks/useCardImage.ts";
import { type GameMode, useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
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
  const object = buildGameObject({
    id: 101,
    card_id: 1,
    zone: "Battlefield",
    name: CARD_NAME,
    printed_ref: { oracle_id: "oracle-starship-enterprise", face_name: CARD_NAME },
  });
  useGameStore.setState({
    gameId: "game-11001",
    gameMode,
    gameState: buildGameState({
      objects: buildObjectMap(object),
      next_object_id: 102,
      battlefield: [object.id],
      next_timestamp: 2,
    }),
    spellCosts: {},
  });
  useUiStore.setState({ inspectedObjectId: object.id });
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

    fireEvent.pointerDown(screen.getByRole("button", { name: "Report a problem" }));
    expect(onDismiss).not.toHaveBeenCalled();

    // Reach guard: a tap on the backdrop does dismiss, so the line above is
    // not passing because nothing listens.
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
