import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { CardPreview } from "../CardPreview.tsx";

/**
 * Content i18n on the card PREVIEW.
 *
 * Scope note, because it is easy to overclaim here: with art present, the card's
 * name is part of the IMAGE, so nothing in the preview renders it as text. The
 * preview's text path is its PLACEHOLDER — the branch that stands in for missing
 * or not-yet-loaded art — and that placeholder is what these tests pin. Before
 * the change it printed the canonical English name in every language, so a
 * Chinese player whose art had not loaded saw an English title over a Chinese UI.
 *
 * The overlay that supplies localized card text already reached `ArtCropCard`,
 * `CommandDock` and `CommanderCardZone`; these tests are the preview's half.
 *
 * `useLocalizedCardName`'s real contract is "localized name, else the name you
 * passed in" — never null for a non-null input — so the stub mirrors that.
 * A stub returning null would model a different function and would let a broken
 * call site pass.
 */
const { localizedNames } = vi.hoisted(() => ({ localizedNames: new Map<string, string>() }));

/**
 * Mirrors `useLocalizedCardName`'s real contract: a passthrough in `en` (English
 * IS the engine's text), otherwise the localized name when the content sidecar
 * has one, otherwise the name passed in. A stub that ignored the language would
 * model a different function and would let a broken call site pass.
 */
vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
  useLocalizedCardName: (name: string | null) =>
    name == null || usePreferencesStore.getState().language === "en"
      ? name
      : (localizedNames.get(name) ?? name),
}));

// `src: null` selects the placeholder branch — the one that renders a name.
const cardImageMock = vi.fn((_cardName: string) => ({
  src: null as string | null,
  isLoading: false,
  isRotated: false,
  isFlip: false,
}));

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: (cardName: string) => cardImageMock(cardName),
}));

afterEach(() => {
  cleanup();
  localizedNames.clear();
  vi.clearAllMocks();
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1280 });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: 768 });
  useGameStore.setState({ gameState: null, spellCosts: {}, legalActionsByObject: {} });
  usePreferencesStore.setState({ language: "en", chineseCardText: true, animationSpeedMultiplier: 0 });
  useUiStore.getState().dismissPreview();
});

describe("CardPreview localized card name", () => {
  it("renders the localized name in the placeholder when the overlay supplies one", () => {
    localizedNames.set("Sol Ring", "阳光戒");
    usePreferencesStore.setState({ language: "zh" });

    render(<CardPreview cardName="Sol Ring" objectId={null} />);

    expect(screen.getByText("阳光戒")).toBeTruthy();
    expect(screen.queryByText("Sol Ring")).toBeNull();
  });

  it("falls back to the engine name for a card with no localized entry", () => {
    usePreferencesStore.setState({ language: "zh" });

    render(<CardPreview cardName="Untranslated Card" objectId={null} />);

    expect(screen.getByText("Untranslated Card")).toBeTruthy();
  });

  it("leaves the name alone in English", () => {
    // The hook is a passthrough in `en` (English IS the engine's text), and the
    // preview must not invent a localized string for it.
    localizedNames.set("Sol Ring", "阳光戒");
    usePreferencesStore.setState({ language: "en" });

    const { container } = render(<CardPreview cardName="Sol Ring" objectId={null} />);

    expect(container.textContent).toContain("Sol Ring");
  });

  it("looks the name up per FACE, never the combined 'A // B' string", () => {
    // A DFC's faces have separate sidecar entries. No sidecar keys on the
    // combined name, so looking that up would silently never localize a DFC.
    localizedNames.set("Delver of Secrets", "秘密的代尔弗");
    usePreferencesStore.setState({ language: "zh" });

    render(
      <CardPreview
        cardName="Delver of Secrets // Insectile Aberration"
        backFaceName="Insectile Aberration"
        objectId={null}
      />,
    );

    expect(screen.getByText("秘密的代尔弗")).toBeTruthy();
  });

  it("keeps the canonical English name for image lookup and alt text", () => {
    localizedNames.set("Sol Ring", "阳光戒");
    usePreferencesStore.setState({ language: "zh" });

    const { container } = render(<CardPreview cardName="Sol Ring" objectId={null} />);

    // Image resolution is keyed by the engine's identity. A localized key would
    // resolve no art at all, silently breaking every card with missing art.
    expect(cardImageMock).toHaveBeenCalledWith("Sol Ring");
    // `alt` stays canonical so the DOM contract other code and tests read still
    // names the card the way the engine does.
    expect(container.querySelector('[aria-label="阳光戒"]')).toBeTruthy();
  });
});
