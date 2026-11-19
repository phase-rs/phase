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
// offers a one-tap reload, and the desktop preview's Alt-pinned panel offers
// the reload beside Report.

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardBackImage: vi.fn(() => ({ src: "card-back.png", isLoading: false })),
  useCardImage: vi.fn(),
}));

const SCULPTOR = "Etherium Sculptor";

/** The engine card database, by lookup name. */
const engineFaces = vi.hoisted(() => new Map<string, { name: string; oracle_text: string }>());

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: vi.fn((name: string | null) => (name == null ? null : engineFaces.get(name) ?? null)),
  // An empty parse is still a parse: the Alt-pinned panel needs one to open.
  useCardParseDetails: (name: string | null) => (name == null ? null : []),
  useCardRulings: () => [],
}));

const viewport = vi.hoisted(() => ({ isMobile: true }));

vi.mock("../../../hooks/useIsMobile.ts", () => ({
  useIsMobile: () => viewport.isMobile,
}));

const RELOAD = "Tap to reload art";
const DESKTOP_RELOAD = "Reload art";

/** The artless tile's rules text: ordinary text beside the image role that
 *  names the tile, so assistive tech reads it. */
function tileRulesText(name: string): HTMLElement {
  const rules = screen.getByRole("img", { name }).nextElementSibling;
  if (!(rules instanceof HTMLElement)) throw new Error(`the ${name} tile shows no rules text`);
  return rules;
}

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
  viewport.isMobile = true;
  engineFaces.clear();
  engineFaces.set(SCULPTOR, { name: SCULPTOR, oracle_text: "Artifact spells you cast cost {1} less to cast." });
  vi.mocked(useCardImage).mockImplementation(() => failedArt(vi.fn()));
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useGameStore.setState({ gameId: null, gameMode: null, gameState: null, spellCosts: {} });
  useUiStore.getState().dismissPreview();
  useUiStore.setState({ inspectedObjectId: null, altHeld: false });
});

describe("CardPreview artless mobile tile", () => {
  // The button's own propagation guard is pinned in CardArtFallback.reload;
  // here the preview wires the reload in and stays open around it.
  it("shows the card's rules text and a reload that keeps the preview open", () => {
    const reload = vi.fn();
    vi.mocked(useCardImage).mockImplementation(() => failedArt(reload));
    const onDismiss = vi.fn();
    inspect(sculptor());
    const { container } = render(<CardPreview cardName={SCULPTOR} onDismiss={onDismiss} />);

    const rules = tileRulesText(SCULPTOR);
    expect(rules).toHaveTextContent("Artifact spells you cast cost");
    expect(rules).toHaveTextContent("less to cast.");

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
    expect(tileRulesText(SCULPTOR)).toHaveTextContent("less to cast.");
  });

  it("names the tile in the language of its rules text", () => {
    // A localized name and a placeholder for its printed text, as in
    // CardImage.test's localized tile.
    engineFaces.set(SCULPTOR, { name: "エーテリウムの彫刻家", oracle_text: "日本語の印刷本文" });
    inspect(sculptor());
    render(<CardPreview cardName={SCULPTOR} />);

    expect(tileRulesText("エーテリウムの彫刻家")).toHaveTextContent("日本語の印刷本文");
  });

  // A hidden face's art is the card back, so the artless tile, and with it
  // the lookup, is never reached; the lookup's own guard is defence in depth.
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

  it("never offers another surface's reload for a face-down card the viewer may not see", () => {
    const hidden = gameObjectFactory
      .creature(2, 2)
      .onBattlefield()
      .named("")
      .build({
        face_down: true,
        face_down_cause: "Morph",
        printed_ref: { oracle_id: "secret-oracle-id", face_name: "Hooded Hydra" },
      });
    // Like the real hook, only a lookup by oracle id can carry that card's
    // failure and reload; the Morph marker's art loads.
    vi.mocked(useCardImage).mockImplementation((_name, options) => ({
      src: "morph-marker.png",
      isLoading: false,
      isRotated: false,
      isFlip: false,
      ...(options?.oracleId ? { reload: vi.fn(), cardArtFailed: true } : {}),
    }));
    inspect(hidden);
    render(<CardPreview cardName="Morph" />);

    // Reach guard: the marker's art is what the preview shows.
    expect(screen.getByAltText("Morph")).toHaveAttribute("src", "morph-marker.png");
    for (const [, options] of vi.mocked(useCardImage).mock.calls) {
      expect(options?.oracleId).not.toBe("secret-oracle-id");
    }
    expect(screen.queryByRole("button", { name: DESKTOP_RELOAD })).toBeNull();
  });

  it("shows a token's own rules text without a card lookup", () => {
    const token = gameObjectFactory
      .artifact()
      .onBattlefield()
      .named("Food")
      .build({
        display_source: "Token",
        token_rules_text: "{2}, {T}, Sacrifice this token: You gain 3 life.",
      });
    inspect(token);
    render(<CardPreview cardName="Food" />);

    expect(tileRulesText("Food")).toHaveTextContent("You gain 3 life.");
    expect(useEngineCardData).not.toHaveBeenCalledWith("Food");
  });

  // A battlefield tile asks for a smaller image, so its failure never reaches
  // the preview's own request: the image result says the card failed elsewhere.
  it("offers a pill to reload another surface whose art failed while this one loaded", () => {
    const reload = vi.fn();
    const onDismiss = vi.fn();
    const loadedArt = { src: "etherium-sculptor.png", isLoading: false, isRotated: false, isFlip: false, reload };
    vi.mocked(useCardImage).mockImplementation(() => ({ ...loadedArt, cardArtFailed: false }));
    inspect(sculptor());
    const { rerender } = render(<CardPreview cardName={SCULPTOR} onDismiss={onDismiss} />);

    // Reach guard: the preview's own art and its strip are on screen, with no
    // pill while no surface of the card failed.
    expect(screen.getByAltText(SCULPTOR)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Report a problem" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: DESKTOP_RELOAD })).toBeNull();

    vi.mocked(useCardImage).mockImplementation(() => ({ ...loadedArt, cardArtFailed: true }));
    rerender(<CardPreview cardName={SCULPTOR} onDismiss={onDismiss} />);
    const pill = screen.getByRole("button", { name: DESKTOP_RELOAD });
    expect(pill).toHaveClass("min-h-11");
    expect(pill.closest("[data-card-preview-actions]")).not.toBeNull();
    fireEvent.pointerDown(pill);
    fireEvent.click(pill);
    expect(reload).toHaveBeenCalledTimes(1);
    expect(onDismiss).not.toHaveBeenCalled();
  });

  // A hand card opened from the hand drawer has no held fan card behind it (the
  // fanned hand stays mounted under the drawer, its card at rest), so it gets
  // the full modal preview, with the rules text and the reload.
  it("gives a hand card inspected from the hand drawer the modal preview with the reload", () => {
    const reload = vi.fn();
    vi.mocked(useCardImage).mockImplementation(() => failedArt(reload));
    const card = gameObjectFactory
      .creature(1, 2)
      .inHand()
      .named(SCULPTOR)
      .build({ printed_ref: { oracle_id: "oracle-etherium-sculptor", face_name: SCULPTOR } });
    const restingFanCard = document.createElement("div");
    restingFanCard.setAttribute("data-hand-card", "");
    restingFanCard.setAttribute("data-object-id", String(card.id));
    document.body.appendChild(restingFanCard);
    try {
      inspect(card);
      render(<CardPreview cardName={SCULPTOR} objectId={card.id} handSourceObjectId={card.id} />);

      expect(tileRulesText(SCULPTOR)).toHaveTextContent("less to cast.");
      fireEvent.click(screen.getByRole("button", { name: RELOAD }));
      expect(reload).toHaveBeenCalledTimes(1);
    } finally {
      restingFanCard.remove();
    }
  });

  // In the default follow and Shift preview modes, the preview of a fan card a
  // finger holds follows the finger: it takes no taps (it is
  // `pointer-events-none`, and the hand keeps the held finger's pointer
  // captured) and closes when the finger lifts, so a button there could never
  // be tapped. It keeps the name-only placeholder.
  it("offers no reload on the preview that follows a finger held on the hand", () => {
    const card = gameObjectFactory
      .creature(1, 2)
      .inHand()
      .named(SCULPTOR)
      .build({ printed_ref: { oracle_id: "oracle-etherium-sculptor", face_name: SCULPTOR } });
    const heldFanCard = document.createElement("div");
    heldFanCard.setAttribute("data-hand-card", "");
    heldFanCard.setAttribute("data-object-id", String(card.id));
    heldFanCard.setAttribute("data-hand-touch-active", "true");
    document.body.appendChild(heldFanCard);
    try {
      inspect(card);
      const { container } = render(
        <CardPreview cardName={SCULPTOR} objectId={card.id} handSourceObjectId={card.id} />,
      );

      // Reach guard: the hand preview (not the modal) rendered the artless card.
      expect(screen.getByRole("img", { name: SCULPTOR })).toBeInTheDocument();
      expect(container.querySelector("[data-card-preview]")).toHaveClass("pointer-events-none");
      expect(screen.queryByRole("button", { name: RELOAD })).toBeNull();
      expect(screen.queryByRole("button", { name: DESKTOP_RELOAD })).toBeNull();
    } finally {
      heldFanCard.remove();
    }
  });

  it("keeps the rules text but no reload on the compact peek, which dismisses on any tap", () => {
    inspect(sculptor());
    render(<CardPreview cardName={SCULPTOR} mobileLayout="compact" />);

    // Reach guard: the artless tile, with its text, is what rendered.
    const rules = tileRulesText(SCULPTOR);
    expect(rules).toHaveTextContent("less to cast.");
    expect(screen.queryByRole("button", { name: RELOAD })).toBeNull();
    // A scroll closes the peek, so its rules text clips rather than scrolls.
    expect(rules).toHaveClass("overflow-hidden");
    expect(rules).not.toHaveClass("overflow-y-auto");
  });
});

describe("CardPreview Alt-pinned panel (desktop)", () => {
  beforeEach(() => {
    viewport.isMobile = false;
    useUiStore.setState({ altHeld: true });
  });

  it("offers the reload beside Report when the card's art failed", () => {
    const reload = vi.fn();
    vi.mocked(useCardImage).mockImplementation(() => failedArt(reload));
    inspect(sculptor());
    render(<CardPreview cardName={SCULPTOR} />);

    // Reach guard: the pinned panel, with its Report button, is what rendered.
    expect(screen.getByRole("button", { name: "Report a problem" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: DESKTOP_RELOAD }));
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("offers no reload while the card's art is on screen", () => {
    vi.mocked(useCardImage).mockImplementation(() => ({
      src: "etherium-sculptor.png", isLoading: false, isRotated: false, isFlip: false, reload: vi.fn(),
    }));
    inspect(sculptor());
    render(<CardPreview cardName={SCULPTOR} />);

    // Reach guard: the pinned panel, with its Report button, is what rendered.
    expect(screen.getByRole("button", { name: "Report a problem" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: DESKTOP_RELOAD })).toBeNull();
  });

  it("offers the reload when another surface of the card failed while this one loaded", () => {
    const reload = vi.fn();
    vi.mocked(useCardImage).mockImplementation(() => ({
      src: "etherium-sculptor.png", isLoading: false, isRotated: false, isFlip: false, reload, cardArtFailed: true,
    }));
    inspect(sculptor());
    render(<CardPreview cardName={SCULPTOR} />);

    // Reach guard: the pinned panel, with its Report button, is what rendered.
    expect(screen.getByRole("button", { name: "Report a problem" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: DESKTOP_RELOAD }));
    expect(reload).toHaveBeenCalledTimes(1);
  });
});
