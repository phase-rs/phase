import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { ChoiceCards, ChoiceHeaderConfirm, type ChoiceCardEntry } from "../ChoiceOverlay.tsx";

// Issue #11063: on a landscape phone (about 390 CSS px tall) the Search Library
// prompt and the effect-zone prompts ("Choose 1 card to exile", put on top or
// bottom, onto the battlefield, return to hand) kept the full layout: the
// "Game Choice" eyebrow, a title and an instruction on lines of their own (an
// effect-zone title only repeats its instruction's verb, as "Exile" over
// "Choose 1 card to exile"), and a large footer confirm. On a short screen each
// now has one header line, like the discard prompt (#11005): Search Library
// keeps its title before the instruction, because the instruction alone does
// not say where the cards come from; an effect-zone title only repeats the
// instruction's verb, so the instruction stands alone. The confirm sits at the
// end of that line, and there is no footer. It keeps its usual label, except
// that a library order counts its picks there, since the cards show the order.
// A taller screen keeps the full layout. The header confirm is one shared
// component, ChoiceHeaderConfirm, which the discard prompt uses too.
//
// The kept title joins the instruction with a bullet: "Search Library • Choose
// 1 card". On a phone (below lg, where the overlay fills the screen) both
// prompts draw their cards in a grid of larger cards that fills the width and
// scrolls down, instead of a sideways strip; a desktop keeps the strip. The
// header follows the screen's height and the grid its width, so a phone held
// upright has the grid under the full header.

const dispatchMock = vi.fn();
vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

type Viewport = { width: number; height: number };

/** A phone turned sideways: under `useIsCompactHeight`'s 500px and narrower than lg. */
const LANDSCAPE_PHONE: Viewport = { width: 844, height: 390 };
/** The same phone held upright: narrower than lg, but tall enough for the full header. */
const PORTRAIT_PHONE: Viewport = { width: 390, height: 844 };
/** A desktop window, lg and up. */
const DESKTOP: Viewport = { width: 1280, height: 768 };
/** happy-dom's default window, which the other modal tests render at. */
const DEFAULT_WINDOW: Viewport = { width: 1024, height: 768 };

// Resolved from this file's path: Vite rewrites `new URL("<relative>", import.meta.url)`
// to a root-relative URL, which readFileSync cannot open.
const STYLESHEET = readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), "../../../index.css"), "utf8");

const HAND = ["Alpha", "Bravo"].map((name, index) =>
  gameObjectFactory.creature().inHand().named(name).withId(index + 1).build(),
);
const GRAVEYARD = ["Cosmo", "Delta"].map((name, index) =>
  gameObjectFactory.creature().inGraveyard().named(name).withId(index + 3).build(),
);
const EXILE = [gameObjectFactory.creature().inExile().named("Echo").withId(5).build()];
const BATTLEFIELD = ["Foxtrot", "Golf"].map((name, index) =>
  gameObjectFactory.creature().onBattlefield().named(name).withId(index + 6).build(),
);
const EQUIPMENT = [gameObjectFactory.artifact().onBattlefield().named("Kilo").withId(11).build()];
const LIBRARY = ["Hotel", "India", "Juliet"].map((name, index) =>
  gameObjectFactory.creature().inLibrary().named(name).withId(index + 8).build(),
);
const HAND_IDS = HAND.map((card) => card.id);
const GRAVEYARD_IDS = GRAVEYARD.map((card) => card.id);
const EXILE_IDS = EXILE.map((card) => card.id);
const BATTLEFIELD_IDS = BATTLEFIELD.map((card) => card.id);
const LIBRARY_IDS = LIBRARY.map((card) => card.id);
const EQUIPMENT_IDS = EQUIPMENT.map((card) => card.id);

type EffectZoneChoiceData = Extract<WaitingFor, { type: "EffectZoneChoice" }>["data"];

/** One effect-zone mode, with the title a taller screen shows over its instruction. */
type EffectZoneCase = {
  prompt: string;
  data: Partial<EffectZoneChoiceData>;
  tallTitle: string;
  title: string;
  confirm: string;
};

// The effect-zone modes the card choice draws. The header line treats every mode
// alike, so this does not list them all: a sacrifice or an untap is usually
// chosen on the board instead, and the strip's other sacrifice-mode prompts
// (a Blight) still use sacrifice wording. The return case keeps today's
// "permanent" wording for cards from a graveyard. Both wordings are their own
// issue; these cases pin only the header layout.
const EFFECT_ZONE_CASES: EffectZoneCase[] = [
  {
    prompt: "an exile from a graveyard",
    data: { cards: GRAVEYARD_IDS, count: 1, effect_kind: "ChangeZone", zone: "Graveyard", destination: "Exile" },
    tallTitle: "Exile",
    title: "Choose 1 card to exile",
    confirm: "Exile (0/1)",
  },
  {
    prompt: "a put on top of the library",
    data: {
      cards: HAND_IDS,
      count: 2,
      effect_kind: "PutAtLibraryPosition",
      zone: "Hand",
      destination: null,
      library_position: { type: "Top" },
    },
    tallTitle: "Put on Library",
    title: "Choose 2 cards to put on top of your library",
    confirm: "Top (0/2)",
  },
  {
    prompt: "a put on the bottom of the library",
    data: {
      cards: HAND_IDS,
      count: 1,
      effect_kind: "PutAtLibraryPosition",
      zone: "Hand",
      destination: null,
      library_position: { type: "Bottom" },
    },
    tallTitle: "Put on Library",
    title: "Choose 1 card to put on the bottom of your library",
    confirm: "Bottom (0/1)",
  },
  {
    prompt: "a put onto the battlefield",
    data: { cards: HAND_IDS, count: 1, effect_kind: "ChangeZone", zone: "Hand", destination: "Battlefield" },
    tallTitle: "Put onto Battlefield",
    title: "Choose 1 card to put onto the battlefield",
    confirm: "Put (0/1)",
  },
  {
    prompt: "a return to hand",
    data: { cards: GRAVEYARD_IDS, count: 1, effect_kind: "ChangeZone", zone: "Graveyard", destination: "Hand" },
    tallTitle: "Return",
    title: "Choose 1 permanent to return to its owner's hand",
    confirm: "Return (0/1)",
  },
  {
    prompt: "a tap",
    data: { cards: BATTLEFIELD_IDS, count: 1, effect_kind: "Tap", zone: "Battlefield", destination: null },
    tallTitle: "Tap",
    title: "Choose 1 permanent to tap",
    confirm: "Confirm (0/1)",
  },
  {
    prompt: "an attach",
    data: { cards: EQUIPMENT_IDS, count: 1, effect_kind: "Attach", zone: "Battlefield", destination: null },
    tallTitle: "Attach",
    title: "Choose 1 Equipment to attach",
    confirm: "Attach (0/1)",
  },
  {
    prompt: "a cast from exile",
    data: { cards: EXILE_IDS, count: 1, effect_kind: "CastFromZone", zone: "Exile", destination: null },
    tallTitle: "Cast",
    title: "Choose 1 card to cast",
    confirm: "Cast (0/1)",
  },
];

function setViewport(viewport: Viewport) {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: viewport.width });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: viewport.height });
}

/** The declarations of the stylesheet's one rule whose selector is exactly `selector`. */
function declarationsOf(selector: string): Map<string, string> {
  const css = STYLESHEET.replace(/\/\*[\s\S]*?\*\//g, "");
  const rules = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].filter((rule) => rule[1].trim() === selector);
  expect(rules, selector).toHaveLength(1);

  const declarations = new Map<string, string>();
  for (const declaration of rules[0][2].split(";")) {
    const colon = declaration.indexOf(":");
    if (colon > 0) {
      declarations.set(declaration.slice(0, colon).trim(), declaration.slice(colon + 1).trim());
    }
  }
  return declarations;
}

function tableState() {
  return gameStateFactory
    .withPlayers({ id: 0, hand: HAND_IDS, graveyard: GRAVEYARD_IDS, library: LIBRARY_IDS }, 1)
    .withObjects(...HAND, ...GRAVEYARD, ...EXILE, ...BATTLEFIELD, ...EQUIPMENT, ...LIBRARY);
}

function showPrompt(gameState: GameState) {
  setGameStoreForTest({ gameState, gameMode: "online" });
  render(<CardChoiceModal />);
}

/** The overlay's header, found from its heading; `getByRole` also proves there is only one. */
function overlayHeader(): HTMLElement {
  const header = screen.getByRole("heading").closest<HTMLElement>(".modal-header-compact");
  expect(header).not.toBeNull();
  return header!;
}

/** Put both hand cards on top of the library, in the order the player picks them. */
function topdeckChoiceState() {
  return tableState()
    .effectZoneChoice({
      player: 0,
      cards: HAND_IDS,
      count: 2,
      effect_kind: "PutAtLibraryPosition",
      zone: "Hand",
      destination: null,
      library_position: { type: "Top" },
    })
    .build();
}

/** These prompts wrap each card in a plain button, so find it from the card's label. */
function cardButton(name: string): HTMLElement {
  return screen.getByLabelText(new RegExp(name)).closest("button")!;
}

/** Whether the named card is drawn in the phone's grid, in a cell of its own, and not in a strip. */
function expectInGrid(name: string) {
  const cell = cardButton(name).parentElement;
  expect(cell).toHaveClass("card-choice-grid-cell");
  expect(cell!.parentElement).toHaveClass("card-choice-grid");
  // The grid scrolls down within the overlay body.
  expect(cell!.parentElement!.parentElement).toHaveClass("min-h-0", "flex-1", "overflow-y-auto", "overflow-x-hidden");
  expect(document.querySelector(".card-choice-strip")).toBeNull();
}

/** Whether the named card is drawn in the desktop's sideways strip, and no grid is drawn. */
function expectInStrip(name: string) {
  expect(cardButton(name).closest(".card-choice-strip")).not.toBeNull();
  expect(document.querySelector(".card-choice-grid")).toBeNull();
}

function clickCard(name: string) {
  fireEvent.click(cardButton(name));
}

/** The single header line: its heading, and its confirm beside it on the same line. */
function expectOneHeaderLine(title: string, confirmName: string): HTMLElement {
  const header = overlayHeader();
  expect(screen.getByRole("heading").textContent).toBe(title);
  expect(within(header).queryByText("Game Choice")).toBeNull();

  const confirm = within(header).getByRole("button", { name: confirmName });
  // The shared header confirm: a 44px touch height, at the compact size rather than the footer's full width.
  expect(confirm).toHaveClass("min-h-11", "text-sm");
  expect(confirm).not.toHaveClass("w-full");
  expect(screen.getByRole("heading").parentElement).toContainElement(confirm);
  // A footer would draw a second confirm.
  expect(screen.getAllByRole("button", { name: confirmName })).toHaveLength(1);
  return confirm;
}

beforeEach(() => {
  dispatchMock.mockClear();
  useMultiplayerStore.setState({ activePlayerId: 0 });
});

afterEach(() => {
  cleanup();
  setViewport(DEFAULT_WINDOW);
  useGameStore.setState(useGameStore.getInitialState(), true);
  useMultiplayerStore.setState(useMultiplayerStore.getInitialState(), true);
});

describe("Search Library on a landscape phone", () => {
  beforeEach(() => {
    setViewport(LANDSCAPE_PHONE);
  });

  it("puts its title before the instruction as one header line, with the confirm on that line", () => {
    showPrompt(tableState().searchChoice({ player: 0, cards: LIBRARY_IDS, count: 1 }).build());

    const confirm = expectOneHeaderLine("Search Library • Choose 1 card", "Confirm");
    expect(confirm).toBeDisabled();
    // The title and the instruction no longer sit on lines of their own.
    expect(screen.queryByText("Search Library")).toBeNull();
    expect(screen.queryByText("Choose 1 card")).toBeNull();
  });

  it("draws its cards in the phone's grid, not a sideways strip", () => {
    showPrompt(tableState().searchChoice({ player: 0, cards: LIBRARY_IDS, count: 1 }).build());

    for (const name of ["Hotel", "India", "Juliet"]) {
      expectInGrid(name);
    }

    clickCard("India");
    expect(within(cardButton("India")).getByText("Choose")).toBeInTheDocument();
  });

  it("keeps its filter row between the header and the grid, and confirms the chosen card from the header", () => {
    showPrompt(tableState().searchChoice({ player: 0, cards: LIBRARY_IDS, count: 1 }).build());

    const filter = screen.getByRole("searchbox", { name: "Filter cards by name" });
    expect(overlayHeader()).not.toContainElement(filter);
    // Outside the scroller, so the filter stays in view while the cards scroll.
    expect(document.querySelector(".card-choice-grid")!.parentElement).not.toContainElement(filter);
    // Positive guard: the card the filter hides is drawn before it.
    expect(screen.getByLabelText(/Hotel/)).toBeInTheDocument();
    fireEvent.change(filter, { target: { value: "India" } });
    expect(screen.queryByLabelText(/Hotel/)).toBeNull();

    clickCard("India");
    fireEvent.click(within(overlayHeader()).getByRole("button", { name: "Confirm" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [9] } });
  });

  it("lets a search that may find nothing confirm from the header with nothing chosen", () => {
    showPrompt(tableState().searchChoice({ player: 0, cards: LIBRARY_IDS, count: 2, up_to: true }).build());

    const confirm = expectOneHeaderLine("Search Library • Choose up to 2 cards", "Confirm");
    expect(confirm).toBeEnabled();

    fireEvent.click(confirm);
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [] } });
  });

  it("keeps the filter and the chosen card when the phone turns upright", () => {
    showPrompt(tableState().searchChoice({ player: 0, cards: LIBRARY_IDS, count: 1 }).build());
    const filter = screen.getByRole("searchbox", { name: "Filter cards by name" });
    fireEvent.change(filter, { target: { value: "India" } });
    clickCard("India");
    // Positive guard: the short-screen layout is drawn before the turn.
    expect(within(overlayHeader()).getByRole("button", { name: "Confirm" })).toBeEnabled();

    act(() => {
      setViewport(PORTRAIT_PHONE);
      window.dispatchEvent(new Event("resize"));
    });

    // Positive guard: the full header is drawn now, over the same grid.
    expect(screen.getByText("Game Choice")).toBeInTheDocument();
    expectInGrid("India");
    // The body was not remounted, so the filter is the same element, which is what keeps its focus.
    expect(screen.getByRole("searchbox", { name: "Filter cards by name" })).toBe(filter);
    expect(filter).toHaveValue("India");
    expect(within(cardButton("India")).getByText("Choose")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [9] } });
  });
});

describe("effect-zone prompts on a landscape phone", () => {
  beforeEach(() => {
    setViewport(LANDSCAPE_PHONE);
  });

  // Each case's taller-screen title must be gone.
  it.each(EFFECT_ZONE_CASES)(
    "gives $prompt, whose title only repeats the instruction, the instruction alone",
    (prompt) => {
      showPrompt(tableState().effectZoneChoice({ player: 0, ...prompt.data }).build());

      const confirm = expectOneHeaderLine(prompt.title, prompt.confirm);
      expect(confirm).toBeDisabled();
      expect(screen.queryByText(prompt.tallTitle)).toBeNull();
    },
  );

  it("confirms the chosen card from the header, with the cards in the phone's grid", () => {
    showPrompt(tableState().effectZoneChoice({ player: 0, cards: GRAVEYARD_IDS, count: 1 }).build());
    expectInGrid("Cosmo");
    expectInGrid("Delta");

    clickCard("Delta");
    expect(within(cardButton("Delta")).getByText("Exile")).toBeInTheDocument();
    fireEvent.click(within(overlayHeader()).getByRole("button", { name: "Exile (1/1)" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [4] } });
  });

  it("counts the picks in the header confirm and shows the library order on the cards", () => {
    showPrompt(topdeckChoiceState());
    expect(within(overlayHeader()).getByRole("button", { name: "Top (0/2)" })).toBeDisabled();

    clickCard("Bravo");
    expect(within(overlayHeader()).getByRole("button", { name: "Top (1/2)" })).toBeDisabled();
    clickCard("Alpha");

    // The order is on the cards, so the confirm's label keeps its length as the picks grow
    // and never pushes the instruction beside it onto a second line.
    expect(within(cardButton("Bravo")).getByText("Top")).toBeInTheDocument();
    expect(within(cardButton("Alpha")).getByText("2nd")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Put on top/ })).toBeNull();
    fireEvent.click(within(overlayHeader()).getByRole("button", { name: "Top (2/2)" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [2, 1] } });
  });

  it("offers Decline in the header for a prompt that may end with nothing chosen", () => {
    showPrompt(
      tableState()
        .effectZoneChoice({
          player: 0,
          cards: HAND_IDS,
          count: 2,
          min_count: 0,
          up_to: true,
          effect_kind: "ChangeZone",
          zone: "Hand",
          destination: "Battlefield",
        })
        .build(),
    );

    const decline = expectOneHeaderLine("Choose up to 2 cards to put onto the battlefield", "Decline");
    expect(decline).toBeEnabled();

    fireEvent.click(decline);
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [] } });
  });
});

describe("Search Library and the effect-zone prompts on a phone held upright", () => {
  beforeEach(() => {
    setViewport(PORTRAIT_PHONE);
  });

  it("draw the phone's grid under the full header, which a tall screen keeps", () => {
    showPrompt(tableState().searchChoice({ player: 0, cards: LIBRARY_IDS, count: 1 }).build());

    expectInGrid("Hotel");
    expect(within(overlayHeader()).getByText("Game Choice")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Search Library" })).toBeInTheDocument();
    expect(within(overlayHeader()).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Confirm" })).toHaveLength(1);
  });

  it("draw an effect-zone prompt's cards in the phone's grid under the full header", () => {
    showPrompt(tableState().effectZoneChoice({ player: 0, cards: GRAVEYARD_IDS, count: 1 }).build());

    expectInGrid("Cosmo");
    expect(within(overlayHeader()).getByText("Game Choice")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Exile" })).toBeInTheDocument();
    expect(within(overlayHeader()).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Exile (0/1)" })).toHaveLength(1);
  });
});

describe("a short desktop window", () => {
  beforeEach(() => {
    setViewport({ width: 1280, height: 390 });
  });

  it("draws the one header line over the desktop strip", () => {
    showPrompt(tableState().searchChoice({ player: 0, cards: LIBRARY_IDS, count: 1 }).build());

    expectOneHeaderLine("Search Library • Choose 1 card", "Confirm");
    expectInStrip("Hotel");
  });
});

describe("the phone's card grid", () => {
  beforeEach(() => {
    setViewport(LANDSCAPE_PHONE);
  });

  it("sizes each card from its own cell, in columns of at least 10rem but never fewer than two", () => {
    expect(declarationsOf(".card-choice-grid").get("grid-template-columns")).toBe(
      "repeat(auto-fill, minmax(min(10rem, calc(50% - 0.25rem)), 1fr))",
    );

    const cell = declarationsOf(".card-choice-grid-cell");
    expect(cell.get("container-type")).toBe("inline-size");
    expect(cell.get("--card-w")).toBe("100cqw");
    expect(cell.get("--card-h")).toBe("calc(var(--card-w) * 7 / 5)");
  });

  it("fades its bottom edge while more cards lie below, and its top edge once scrolled", () => {
    showPrompt(tableState().searchChoice({ player: 0, cards: LIBRARY_IDS, count: 1 }).build());
    const scroller = document.querySelector(".card-choice-grid")!.parentElement!;
    // The fades follow the scroller, inside the wrapper it fills.
    const [topFade, bottomFade] = Array.from(scroller.parentElement!.children).slice(1);
    // Positive guard: happy-dom lays nothing out, so nothing overflows yet and both fades are hidden.
    expect(topFade).toHaveClass("opacity-0");
    expect(bottomFade).toHaveClass("opacity-0");

    // happy-dom has no layout, so give the scroller a tall content and a short window onto it.
    Object.defineProperty(scroller, "scrollHeight", { configurable: true, value: 900 });
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 300 });
    fireEvent.scroll(scroller);
    expect(topFade).toHaveClass("opacity-0");
    expect(bottomFade).toHaveClass("opacity-100");

    Object.defineProperty(scroller, "scrollTop", { configurable: true, value: 600 });
    fireEvent.scroll(scroller);
    expect(topFade).toHaveClass("opacity-100");
    expect(bottomFade).toHaveClass("opacity-0");
  });
});

describe("ChoiceCards", () => {
  /** Draws each card as a button, except card 2, which has nothing to draw. */
  function renderTestCard(id: number, _entry: ChoiceCardEntry) {
    return id === 2 ? null : <button type="button">{`Card ${id}`}</button>;
  }

  /** The entry the last render handed card `id`, from the render callback's calls. */
  function lastEntryFor(calls: Array<[number, ChoiceCardEntry]>, id: number): ChoiceCardEntry {
    const callsForCard = calls.filter(([calledId]) => calledId === id);
    expect(callsForCard.length, `card ${id} was drawn`).toBeGreaterThan(0);
    return callsForCard[callsForCard.length - 1][1];
  }

  function turnTo(viewport: Viewport) {
    act(() => {
      setViewport(viewport);
      window.dispatchEvent(new Event("resize"));
    });
  }

  it("staggers the strip's entrance on a desktop, and skips a card with nothing to draw", () => {
    setViewport(DESKTOP);
    const renderCard = vi.fn(renderTestCard);
    render(<ChoiceCards cards={[1, 2, 3]} renderCard={renderCard} />);

    expect(document.querySelector(".card-choice-strip")).not.toBeNull();
    expect(screen.getAllByRole("button", { name: /^Card / })).toHaveLength(2);
    const third = lastEntryFor(renderCard.mock.calls, 3);
    expect(third.initial).toEqual({ opacity: 0, y: 60, scale: 0.85 });
    expect(third.transition).toEqual({ delay: expect.closeTo(0.26), duration: 0.35 });
  });

  it("gives the phone's grid a quick entrance with no stagger, and no cell to a card with nothing to draw", () => {
    setViewport(LANDSCAPE_PHONE);
    const renderCard = vi.fn(renderTestCard);
    render(<ChoiceCards cards={[1, 2, 3]} renderCard={renderCard} />);

    expect(document.querySelectorAll(".card-choice-grid-cell")).toHaveLength(2);
    const third = lastEntryFor(renderCard.mock.calls, 3);
    expect(third.initial).toEqual({ opacity: 0, y: 24, scale: 0.9 });
    expect(third.transition).toEqual({ duration: 0.18 });
  });

  it("never hands the strip its staggered entrance once the window has crossed lg", () => {
    setViewport(LANDSCAPE_PHONE);
    const renderCard = vi.fn(renderTestCard);
    render(<ChoiceCards cards={[1, 2, 3]} renderCard={renderCard} />);
    // Positive guard: the prompt opened in the phone's grid.
    expect(document.querySelector(".card-choice-grid")).not.toBeNull();
    const callsBeforeTurn = renderCard.mock.calls.length;

    turnTo(DESKTOP);

    // The strip is drawn now, and every entry its cards were handed since the
    // turn, the first render's included, skips the entrance and its delays.
    // A switch noticed one render late would hand the first render the stagger.
    expect(document.querySelector(".card-choice-strip")).not.toBeNull();
    const entriesSinceTurn = renderCard.mock.calls
      .slice(callsBeforeTurn)
      .filter(([id]) => id === 3)
      .map(([, entry]) => entry);
    expect(entriesSinceTurn.length).toBeGreaterThan(0);
    for (const entry of entriesSinceTurn) {
      expect(entry.initial).toBe(false);
      expect(entry.transition).toEqual({ duration: 0.35 });
    }
  });

  it("keeps the strip's skipped entrance after crossing lg and back", () => {
    setViewport(DESKTOP);
    const renderCard = vi.fn(renderTestCard);
    render(<ChoiceCards cards={[1, 2, 3]} renderCard={renderCard} />);
    // Positive guard: the strip opened with its staggered entrance.
    expect(lastEntryFor(renderCard.mock.calls, 3).initial).not.toBe(false);

    turnTo(LANDSCAPE_PHONE);
    turnTo(DESKTOP);

    expect(document.querySelector(".card-choice-strip")).not.toBeNull();
    expect(lastEntryFor(renderCard.mock.calls, 3).initial).toBe(false);
  });
});

describe("Search Library and the effect-zone prompts on a desktop", () => {
  beforeEach(() => {
    setViewport(DESKTOP);
  });

  // The positive side of each case's short-screen check that its title is gone.
  it.each(EFFECT_ZONE_CASES)("keep $prompt's own title over its instruction", (prompt) => {
    showPrompt(tableState().effectZoneChoice({ player: 0, ...prompt.data }).build());

    expect(screen.getByRole("heading").textContent).toBe(prompt.tallTitle);
    expect(within(overlayHeader()).getByText(prompt.title)).toBeInTheDocument();
    expect(within(overlayHeader()).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: prompt.confirm })).toHaveLength(1);
  });

  it("keep Search Library's eyebrow, title, instruction and footer confirm", () => {
    showPrompt(tableState().searchChoice({ player: 0, cards: LIBRARY_IDS, count: 1 }).build());

    const header = overlayHeader();
    expect(within(header).getByText("Game Choice")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Search Library" })).toBeInTheDocument();
    expect(within(header).getByText("Choose 1 card")).toBeInTheDocument();
    expect(within(header).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Confirm" })).toHaveLength(1);
    expect(screen.getByRole("searchbox", { name: "Filter cards by name" })).toBeInTheDocument();
    expectInStrip("Hotel");
  });

  it("keep an effect-zone prompt's eyebrow, title, instruction and footer confirm", () => {
    showPrompt(tableState().effectZoneChoice({ player: 0, cards: GRAVEYARD_IDS, count: 1 }).build());

    const header = overlayHeader();
    expect(within(header).getByText("Game Choice")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Exile" })).toBeInTheDocument();
    expect(within(header).getByText("Choose 1 card to exile")).toBeInTheDocument();
    expect(within(header).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Exile (0/1)" })).toHaveLength(1);
    expectInStrip("Cosmo");
  });

  it("keep the library order in an effect-zone prompt's footer confirm", () => {
    showPrompt(topdeckChoiceState());

    clickCard("Bravo");
    clickCard("Alpha");
    const confirm = screen.getByRole("button", { name: "Put on top (Top -> 2nd)" });
    expect(overlayHeader()).not.toContainElement(confirm);
    fireEvent.click(confirm);
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [2, 1] } });
  });
});

describe("ChoiceHeaderConfirm", () => {
  it("says Confirm unless given a label, at a 44px touch height", () => {
    const { rerender } = render(<ChoiceHeaderConfirm onClick={vi.fn()} />);

    const confirm = screen.getByRole("button", { name: "Confirm" });
    expect(confirm).toHaveAttribute("type", "button");
    expect(confirm).toHaveClass("min-h-11", "text-sm");
    expect(confirm).not.toHaveClass("w-full");

    rerender(<ChoiceHeaderConfirm onClick={vi.fn()} label="Exile (1/1)" />);
    expect(screen.getByRole("button", { name: "Exile (1/1)" })).toHaveClass("min-h-11");
    expect(screen.queryByRole("button", { name: "Confirm" })).toBeNull();
  });

  it("confirms on a click only while enabled", () => {
    const onClick = vi.fn();
    const { rerender } = render(<ChoiceHeaderConfirm onClick={onClick} disabled />);

    const confirm = screen.getByRole("button", { name: "Confirm" });
    expect(confirm).toBeDisabled();
    // It looks disabled too, not only refuses the click.
    expect(confirm).toHaveClass("cursor-not-allowed");
    fireEvent.click(confirm);
    expect(onClick).not.toHaveBeenCalled();

    rerender(<ChoiceHeaderConfirm onClick={onClick} />);
    expect(confirm).toBeEnabled();
    expect(confirm).not.toHaveClass("cursor-not-allowed");
    fireEvent.click(confirm);
    expect(onClick).toHaveBeenCalledTimes(1);
  });
});
