import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { FaceDownCause } from "../../../adapter/types.ts";
import { useCardBackImage, useCardImage } from "../../../hooks/useCardImage.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { getPlayerDisplayName, useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory, retargetChoiceWaitingForFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { CardImage } from "../../card/CardImage.tsx";
import { ZoneViewer } from "../../zone/ZoneViewer.tsx";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { ChoiceOverlay, ScrollableCardStrip } from "../ChoiceOverlay.tsx";
import { RetargetChoiceModal } from "../RetargetChoiceModal.tsx";

// Issue #11002: on a phone, card choices (Search Library, Dig, Scry, ...) and
// the graveyard/exile viewer showed their cards at battlefield size. The
// strips sized cards with `.card-choice-strip [role="img"]`, a rule keyed to
// the markup CardImage rendered when it was written. CardImage later drew
// loaded art as a plain <img>, which has no role attribute, so the rule
// stopped matching without any error, and the strip's wrapper never grew into
// the overlay body. These tests pin the replacement from both ends: below lg,
// where the overlay fills the screen, the strip's frame fills its parent and
// sets the --card-w/--card-h pair, and everything drawn in a strip reads that
// pair, whatever its markup. From lg up the cards keep the board size on
// purpose: desktop testing found it reads well there.

vi.mock("../../../hooks/useCardImage.ts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../hooks/useCardImage.ts")>()),
  useCardImage: vi.fn(),
  useCardBackImage: vi.fn(),
}));

vi.mock("../../../hooks/useEngineCardData.ts", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../hooks/useEngineCardData.ts")>()),
  useEngineCardData: vi.fn(() => null),
}));

const mockUseCardImage = vi.mocked(useCardImage);
const mockUseCardBackImage = vi.mocked(useCardBackImage);

const CARD_NAME = "Llanowar Elves";
const CARD_ART = "https://example.invalid/llanowar-elves.png";

beforeEach(() => {
  mockUseCardImage.mockReturnValue({ src: CARD_ART, isLoading: false, isRotated: false, isFlip: false });
  mockUseCardBackImage.mockReturnValue({ src: null, isLoading: false });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useGameStore.setState(useGameStore.getInitialState(), true);
  useMultiplayerStore.setState(useMultiplayerStore.getInitialState(), true);
});

type StyleRule = {
  /** Preludes of the blocks around the rule, outermost first, such as `@media (width < 64rem)`. */
  enclosedBy: string[];
  selectors: string[];
  declarations: Map<string, string>;
};

/**
 * Reads every style rule of a stylesheet whose strings hold no braces or
 * semicolons, which is true of index.css. It is small enough to read in full,
 * unlike happy-dom's CSSOM parser, and it keeps the at-rules around each rule.
 */
function readStyleRules(source: string): StyleRule[] {
  const css = source.replace(/\/\*[\s\S]*?\*\//g, "");
  const rules: StyleRule[] = [];
  const openBlocks: Array<{ prelude: string; declarations: Map<string, string> }> = [];
  let segmentStart = 0;

  for (let index = 0; index < css.length; index += 1) {
    const character = css[index];
    if (character !== "{" && character !== "}" && character !== ";") {
      continue;
    }

    const segment = css.slice(segmentStart, index).trim();
    segmentStart = index + 1;
    if (character === "{") {
      openBlocks.push({ prelude: segment, declarations: new Map() });
      continue;
    }

    // Inside a block, a semicolon or the closing brace ends a declaration.
    const block = openBlocks[openBlocks.length - 1];
    const colon = segment.indexOf(":");
    if (block && colon > 0) {
      block.declarations.set(segment.slice(0, colon).trim(), segment.slice(colon + 1).trim());
    }
    if (character !== "}" || !block) {
      continue;
    }

    openBlocks.pop();
    if (!block.prelude.startsWith("@")) {
      rules.push({
        enclosedBy: openBlocks.map((openBlock) => openBlock.prelude),
        selectors: block.prelude.split(",").map((selector) => selector.trim()),
        declarations: block.declarations,
      });
    }
  }

  return rules;
}

// Resolved from this file's path: Vite rewrites `new URL("<relative>", import.meta.url)`
// to a root-relative URL, which readFileSync cannot open.
const STYLESHEET_PATH = resolve(dirname(fileURLToPath(import.meta.url)), "../../../index.css");
const STYLE_RULES = readStyleRules(readFileSync(STYLESHEET_PATH, "utf8"));
const BELOW_LG = "@media (width < 64rem)";
const STRIP_CLASS = /\.(card-strip-frame|card-choice-strip|zone-viewer-strip)(?![\w-])/;
/** A strip class in a selector's last compound: the rule styles the frame or a strip itself. */
const STYLES_A_STRIP = /\.(card-strip-frame|card-choice-strip|zone-viewer-strip)(?![\w-])[^\s>+~]*$/;
const SIZING_PROPERTY =
  /^(min-|max-)?(width|height|inline-size|block-size)$|^(aspect-ratio|flex(-basis)?|container(-type)?)$|^--card-/;

/**
 * The declarations index.css gives `element` through rules that select one of
 * its classes on its own, inside exactly the `enclosedBy` at-rules, later rules
 * winning. Every card-strip rule addresses its element this way.
 */
function declarationsFor(element: Element, enclosedBy: string[]): Map<string, string> {
  const classSelectors = Array.from(element.classList, (className) => `.${className}`);
  const declarations = new Map<string, string>();
  for (const rule of STYLE_RULES) {
    if (rule.enclosedBy.join("\n") !== enclosedBy.join("\n")) {
      continue;
    }
    if (!rule.selectors.some((selector) => classSelectors.includes(selector))) {
      continue;
    }
    for (const [property, value] of rule.declarations) {
      declarations.set(property, value);
    }
  }
  return declarations;
}

/** The frame ScrollableCardStrip renders around its strip. */
function stripFrame(): Element {
  const frame = document.querySelector(".card-strip-frame");
  if (!frame) {
    throw new Error("No ScrollableCardStrip frame was rendered");
  }
  return frame;
}

type CardState = {
  state: string;
  cardSrc: string | null;
  isLoading: boolean;
  cardBackSrc: string | null;
  faceDownCause?: FaceDownCause;
  faceDown: boolean;
  findCard: () => HTMLElement;
};

/** Every state CardImage draws a card in, and how to find the element that draws it. */
const CARD_STATES: CardState[] = [
  {
    state: "loaded art",
    cardSrc: CARD_ART,
    isLoading: false,
    cardBackSrc: null,
    faceDown: false,
    findCard: () => screen.getByAltText(CARD_NAME),
  },
  {
    state: "loading pulse",
    cardSrc: null,
    isLoading: true,
    cardBackSrc: null,
    faceDown: false,
    findCard: () => screen.getByLabelText(`Loading ${CARD_NAME}`),
  },
  {
    state: "text fallback",
    cardSrc: null,
    isLoading: false,
    cardBackSrc: null,
    faceDown: false,
    // The tile's image role sits on its name line, which keeps its rules text
    // readable to assistive tech (#11008), so the tile is found by its test id.
    findCard: () => screen.getByTestId("card-art-fallback"),
  },
  {
    state: "drawn card back",
    cardSrc: null,
    isLoading: false,
    cardBackSrc: null,
    faceDown: true,
    findCard: () => screen.getByRole("img", { name: "Card back" }),
  },
  {
    state: "card back art",
    cardSrc: null,
    isLoading: false,
    cardBackSrc: "https://example.invalid/card-back.png",
    faceDown: true,
    findCard: () => screen.getByAltText("Card back"),
  },
  {
    state: "face-down marker art",
    cardSrc: "https://example.invalid/morph.png",
    isLoading: false,
    cardBackSrc: null,
    faceDownCause: "Morph",
    faceDown: true,
    findCard: () => screen.getByAltText("Morph"),
  },
];

describe("CardImage", () => {
  it.each(CARD_STATES)("draws the $state at the --card-w/--card-h size", (cardState) => {
    mockUseCardImage.mockReturnValue({
      src: cardState.cardSrc,
      isLoading: cardState.isLoading,
      isRotated: false,
      isFlip: false,
    });
    mockUseCardBackImage.mockReturnValue({ src: cardState.cardBackSrc, isLoading: false });

    render(
      <CardImage
        cardName={CARD_NAME}
        faceDown={cardState.faceDown}
        faceDownCause={cardState.faceDownCause}
      />,
    );

    expect(cardState.findCard()).toHaveClass("w-[var(--card-w)]", "h-[var(--card-h)]");
  });
});

describe("the strip's frame", () => {
  beforeEach(() => {
    render(
      <ScrollableCardStrip>
        <span />
      </ScrollableCardStrip>,
    );
  });

  it("grows into its parent and sizes the cards from its own height below the lg breakpoint", () => {
    const frameStyle = declarationsFor(stripFrame(), [BELOW_LG]);

    expect(frameStyle.get("flex")).toMatch(/^1(\s|$)/);
    expect(frameStyle.get("container-type")).toBe("size");
    // The whole declarations, so a value that keeps the tokens but yields an
    // unusable card size fails.
    expect(frameStyle.get("--card-strip-chrome-h")).toBe("2.75rem");
    expect(frameStyle.get("--card-h")).toBe(
      "min(100cqh - var(--card-strip-chrome-h), (100cqw - 2rem) * 7 / 5, 560px)",
    );
    expect(frameStyle.get("--card-w")).toBe("calc(var(--card-h) * 5 / 7)");
  });

  it("leaves the cards at the board size from the lg breakpoint", () => {
    // Desktop's board-size cards (from .card-scale-reset) read well in testing,
    // so the frame and the strips size nothing outside the below-lg block, and
    // no strip rule takes anything from the viewport height.
    // A pseudo-element, such as the scrollbar, sizes only itself, not the cards.
    const sizingRules = STYLE_RULES.filter((rule) =>
      rule.selectors.some((selector) => STYLES_A_STRIP.test(selector) && !selector.includes("::"))
      && Array.from(rule.declarations.keys()).some((property) => SIZING_PROPERTY.test(property)),
    );
    // Reach guard: the scan must find the below-lg sizing rule, or the loop below proves nothing.
    expect(sizingRules.length).toBeGreaterThan(0);
    for (const rule of sizingRules) {
      expect(rule.enclosedBy, rule.selectors.join(", ")).toEqual([BELOW_LG]);
    }

    const stripRules = STYLE_RULES.filter((rule) =>
      rule.selectors.some((selector) => STRIP_CLASS.test(selector))
      || rule.enclosedBy.some((prelude) => STRIP_CLASS.test(prelude)),
    );
    for (const rule of stripRules) {
      for (const value of rule.declarations.values()) {
        expect(value, rule.selectors.join(", ")).not.toMatch(/\d(vh|svh|dvh|lvh)\b/);
      }
    }
  });
});

describe("the parent of a strip's frame", () => {
  it("is a flex column the frame can fill in a choice overlay", () => {
    render(
      <ChoiceOverlay title="Choose a card" subtitle="Choose 1 card">
        <ScrollableCardStrip>
          <CardImage cardName={CARD_NAME} />
        </ScrollableCardStrip>
      </ChoiceOverlay>,
    );

    expect(stripFrame().parentElement).toHaveClass("flex", "flex-col", "flex-1", "min-h-0");
  });

  it("is a flex column the frame can fill under Search Library's filter", () => {
    const card = gameObjectFactory.creature().inLibrary().named(CARD_NAME).withId(42).build();
    const gameState = gameStateFactory
      .withPlayers({ id: 0, library: [card.id] }, 1)
      .withObjects(card)
      .searchChoice({ player: 0, cards: [card.id], count: 1 })
      .build();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    setGameStoreForTest({ gameState, gameMode: "online" });

    render(<CardChoiceModal />);

    // Positive guard: the search drew the card inside the strip.
    expect(screen.getByAltText(CARD_NAME).closest(".card-choice-strip")).not.toBeNull();
    expect(stripFrame().parentElement).toHaveClass("flex", "flex-col", "flex-1", "min-h-0");
  });

  it("is a flex column the frame can fill in the graveyard viewer", () => {
    const card = gameObjectFactory.creature().inGraveyard().named(CARD_NAME).withId(7).build();
    const gameState = gameStateFactory
      .withPlayers({ id: 0, graveyard: [card.id] }, 1)
      .withObjects(card)
      .priority(0)
      .build();
    setGameStoreForTest({ gameState });

    render(<ZoneViewer zone="graveyard" playerId={0} onClose={vi.fn()} />);

    // Positive guard: the viewer drew the card inside the strip.
    expect(screen.getByAltText(CARD_NAME).closest(".zone-viewer-strip")).not.toBeNull();
    expect(stripFrame().parentElement).toHaveClass("flex", "flex-col", "flex-1", "min-h-0");
  });
});

describe("what a strip draws besides a card", () => {
  it("draws Learn's selection over the card, where the strip cannot cut it off", () => {
    const card = gameObjectFactory.creature().inHand().named(CARD_NAME).withId(42).build();
    const gameState = gameStateFactory
      .withPlayers({ id: 0, hand: [card.id] }, 1)
      .withObjects(card)
      .learnChoice({ player: 0, hand_cards: [card.id] })
      .build();
    useMultiplayerStore.setState({ activePlayerId: 0 });
    setGameStoreForTest({ gameState, gameMode: "online" });

    render(<CardChoiceModal />);
    fireEvent.click(screen.getByRole("button", { name: CARD_NAME }));

    // Positive guard: the badge belongs to the card's strip item.
    const badge = screen.getByText("Selected");
    expect(badge.closest(".card-choice-strip")).not.toBeNull();
    expect(badge.parentElement).toHaveClass("absolute", "inset-0");
  });

  it("draws a player target's tile at the cards' size below lg and at its desktop size from lg", () => {
    const { data } = retargetChoiceWaitingForFactory
      .withData({
        scope: { type: "All" },
        current_targets: [{ Player: 1 }],
        legal_new_targets: [{ Player: 0 }],
      })
      .build();

    render(<RetargetChoiceModal data={data} />);

    expect(screen.getByText(getPlayerDisplayName(0))).toHaveClass(
      "w-[var(--card-w)]",
      "h-[var(--card-h)]",
      "lg:w-32",
      "lg:h-44",
    );
  });
});

it("never sizes a strip's cards by reaching into their markup", () => {
  // A strip class, any pseudo-classes or compound classes on it, then a combinator.
  const reachesIntoStrip = /\.(card-strip-frame|card-choice-strip|zone-viewer-strip)[^\s>+~,]*[\s>+~]/;
  const descendantRules = STYLE_RULES.filter((rule) =>
    rule.selectors.some((selector) => reachesIntoStrip.test(selector))
    || rule.enclosedBy.some((prelude) => STRIP_CLASS.test(prelude)),
  );
  // Reach guard: the scan must see the strips' descendant rules, or the loop below proves nothing.
  expect(descendantRules.length).toBeGreaterThan(0);

  for (const rule of descendantRules) {
    for (const property of rule.declarations.keys()) {
      expect(property, rule.selectors.join(", ")).not.toMatch(SIZING_PROPERTY);
    }
  }
});
