import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject } from "../../../adapter/types.ts";
import { useCardImage } from "../../../hooks/useCardImage.ts";
import { useEngineCardData } from "../../../hooks/useEngineCardData.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { CardPreview } from "../CardPreview.tsx";

// Regression tests for #11008: the long-press preview of a card whose art
// failed was a blank panel with only the name. It now shows the rules text and
// offers a one-tap reload.

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardBackImage: vi.fn(() => ({ src: "card-back.png", isLoading: false })),
  useCardImage: vi.fn(),
}));

const SCULPTOR = "Etherium Sculptor";

/** The engine card database, by lookup name. */
const engineFaces = vi.hoisted(() => new Map<string, { name: string; oracle_text: string }>());

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: vi.fn((name: string | null) => (name == null ? null : engineFaces.get(name) ?? null)),
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

// The mobile long-press preview is the subject: force the mobile branch.
vi.mock("../../../hooks/useIsMobile.ts", () => ({
  useIsMobile: () => true,
}));

const RELOAD = "Tap to reload art";

function failedArt(reload: () => void) {
  return { src: null, isLoading: false, isRotated: false, isFlip: false, reload };
}

function inspect(card: GameObject): void {
  useGameStore.setState({
    gameId: "game-11008",
    gameMode: "ai",
    gameState: gameStateFactory.withPlayers(0, 1).withObjects(card).build(),
    spellCosts: {},
  });
  useUiStore.setState({ inspectedObjectId: card.id });
}

function sculptor(): GameObject {
  return gameObjectFactory
    .creature(1, 2)
    .onBattlefield()
    .named(SCULPTOR)
    .build({ printed_ref: { oracle_id: "oracle-etherium-sculptor", face_name: SCULPTOR } });
}

beforeEach(() => {
  engineFaces.clear();
  engineFaces.set(SCULPTOR, { name: SCULPTOR, oracle_text: "Artifact spells you cast cost {1} less to cast." });
  vi.mocked(useCardImage).mockImplementation(() => failedArt(vi.fn()));
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useGameStore.setState({ gameId: null, gameMode: null, gameState: null, spellCosts: {} });
  useUiStore.getState().dismissPreview();
  useUiStore.setState({ inspectedObjectId: null });
});

describe("CardPreview artless mobile tile", () => {
  it("shows the card's rules text and a reload that keeps the preview open", () => {
    const reload = vi.fn();
    vi.mocked(useCardImage).mockImplementation(() => failedArt(reload));
    const onDismiss = vi.fn();
    inspect(sculptor());
    const { container } = render(<CardPreview cardName={SCULPTOR} onDismiss={onDismiss} />);

    const tile = screen.getByRole("img", { name: SCULPTOR });
    expect(tile).toHaveTextContent("Artifact spells you cast cost");
    expect(tile).toHaveTextContent("less to cast.");

    const button = screen.getByRole("button", { name: RELOAD });
    fireEvent.pointerDown(button);
    fireEvent.click(button);
    expect(reload).toHaveBeenCalledTimes(1);
    expect(onDismiss).not.toHaveBeenCalled();

    // Reach guard: a tap on the backdrop does dismiss, so the assertion above
    // is not passing because nothing listens.
    const backdrop = container.querySelector<HTMLElement>("[data-card-preview]");
    if (backdrop == null) throw new Error("the preview has no backdrop");
    fireEvent.pointerDown(backdrop);
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("looks the rules text up only once the artless tile shows", () => {
    vi.mocked(useCardImage).mockImplementation(() => ({
      src: "etherium-sculptor.png", isLoading: false, isRotated: false, isFlip: false,
    }));
    inspect(sculptor());
    const { rerender } = render(<CardPreview cardName={SCULPTOR} />);

    expect(screen.getByAltText(SCULPTOR)).toBeInTheDocument();
    expect(useEngineCardData).not.toHaveBeenCalledWith(SCULPTOR);

    vi.mocked(useCardImage).mockImplementation(() => failedArt(vi.fn()));
    rerender(<CardPreview cardName={SCULPTOR} />);
    expect(useEngineCardData).toHaveBeenCalledWith(SCULPTOR);
    expect(screen.getByRole("img", { name: SCULPTOR })).toHaveTextContent("less to cast.");
  });

  it("names the tile in the language of its rules text", () => {
    engineFaces.set(SCULPTOR, {
      name: "エーテリウムの彫刻家",
      oracle_text: "あなたが唱えるアーティファクト・呪文は{1}少ないコストで唱えられる。",
    });
    inspect(sculptor());
    render(<CardPreview cardName={SCULPTOR} />);

    expect(screen.getByRole("img", { name: "エーテリウムの彫刻家" })).toHaveTextContent("少ないコストで唱えられる。");
  });

  it("never looks up or offers a reload for a face-down card the viewer may not see", () => {
    const hidden = gameObjectFactory
      .creature(2, 2)
      .onBattlefield()
      .named("")
      .build({
        face_down: true,
        face_down_cause: "Morph",
        printed_ref: { oracle_id: "secret-oracle-id", face_name: "Hooded Hydra" },
      });
    inspect(hidden);
    const { container } = render(<CardPreview cardName="Morph" />);

    // Reach guard: the marker's art failed, so the preview drew the card back.
    const srcs = [...container.querySelectorAll("img")].map((img) => img.getAttribute("src"));
    expect(srcs).toContain("card-back.png");
    expect(useEngineCardData).toHaveBeenCalled();
    for (const [name] of vi.mocked(useEngineCardData).mock.calls) expect(name).toBeNull();
    expect(screen.queryByRole("button", { name: RELOAD })).toBeNull();
  });

  it("shows a token's own rules text without a card lookup", () => {
    const token = gameObjectFactory
      .artifact()
      .onBattlefield()
      .named("Banana")
      .build({
        display_source: "Token",
        token_rules_text: "Sacrifice this token: Add one mana. You gain 2 life.",
      });
    inspect(token);
    render(<CardPreview cardName="Banana" />);

    expect(screen.getByRole("img", { name: "Banana" })).toHaveTextContent("You gain 2 life.");
    expect(useEngineCardData).not.toHaveBeenCalledWith("Banana");
  });

  it("keeps the rules text but no reload on the compact peek, which dismisses on any tap", () => {
    inspect(sculptor());
    render(<CardPreview cardName={SCULPTOR} mobileLayout="compact" />);

    // Reach guard: the artless tile, with its text, is what rendered.
    expect(screen.getByRole("img", { name: SCULPTOR })).toHaveTextContent("less to cast.");
    expect(screen.queryByRole("button", { name: RELOAD })).toBeNull();
  });
});
