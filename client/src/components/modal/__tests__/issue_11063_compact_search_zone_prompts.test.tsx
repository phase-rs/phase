import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { ChoiceHeaderConfirm } from "../ChoiceOverlay.tsx";

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
// A taller screen keeps the full layout. The header confirm is one shared component,
// ChoiceHeaderConfirm, which the discard prompt uses too.

const dispatchMock = vi.fn();
vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

/** A landscape phone, under `useIsCompactHeight`'s 500px threshold. */
const LANDSCAPE_PHONE_HEIGHT = 390;
/** happy-dom's default window height, which the other modal tests render at. */
const DESKTOP_HEIGHT = 768;

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

// The effect-zone modes the card strip draws. The header line treats every mode
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

function setViewportHeight(height: number) {
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });
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

/** These strips wrap each card in a plain button, so find it from the card's label. */
function cardButton(name: string): HTMLElement {
  return screen.getByLabelText(new RegExp(name)).closest("button")!;
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
  setViewportHeight(DESKTOP_HEIGHT);
  useGameStore.setState(useGameStore.getInitialState(), true);
  useMultiplayerStore.setState(useMultiplayerStore.getInitialState(), true);
});

describe("Search Library on a landscape phone", () => {
  beforeEach(() => {
    setViewportHeight(LANDSCAPE_PHONE_HEIGHT);
  });

  it("puts its title before the instruction as one header line, with the confirm on that line", () => {
    showPrompt(tableState().searchChoice({ player: 0, cards: LIBRARY_IDS, count: 1 }).build());

    const confirm = expectOneHeaderLine("Search Library: Choose 1 card", "Confirm");
    expect(confirm).toBeDisabled();
    // The title and the instruction no longer sit on lines of their own.
    expect(screen.queryByText("Search Library")).toBeNull();
    expect(screen.queryByText("Choose 1 card")).toBeNull();
  });

  it("keeps its filter row below the header, and confirms the chosen card from the header", () => {
    showPrompt(tableState().searchChoice({ player: 0, cards: LIBRARY_IDS, count: 1 }).build());

    const filter = screen.getByRole("searchbox", { name: "Filter cards by name" });
    expect(overlayHeader()).not.toContainElement(filter);
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

    const confirm = expectOneHeaderLine("Search Library: Choose up to 2 cards", "Confirm");
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
      setViewportHeight(DESKTOP_HEIGHT);
      window.dispatchEvent(new Event("resize"));
    });

    // Positive guard: the taller layout is drawn now.
    expect(screen.getByText("Game Choice")).toBeInTheDocument();
    // The body was not remounted, so the filter is the same element, which is what keeps its focus.
    expect(screen.getByRole("searchbox", { name: "Filter cards by name" })).toBe(filter);
    expect(filter).toHaveValue("India");
    expect(screen.getByText("Choose")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [9] } });
  });
});

describe("effect-zone prompts on a landscape phone", () => {
  beforeEach(() => {
    setViewportHeight(LANDSCAPE_PHONE_HEIGHT);
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

  it("confirms the chosen card from the header", () => {
    showPrompt(tableState().effectZoneChoice({ player: 0, cards: GRAVEYARD_IDS, count: 1 }).build());

    clickCard("Delta");
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

describe("Search Library and the effect-zone prompts on a taller screen", () => {
  beforeEach(() => {
    setViewportHeight(DESKTOP_HEIGHT);
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
  });

  it("keep an effect-zone prompt's eyebrow, title, instruction and footer confirm", () => {
    showPrompt(tableState().effectZoneChoice({ player: 0, cards: GRAVEYARD_IDS, count: 1 }).build());

    const header = overlayHeader();
    expect(within(header).getByText("Game Choice")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Exile" })).toBeInTheDocument();
    expect(within(header).getByText("Choose 1 card to exile")).toBeInTheDocument();
    expect(within(header).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Exile (0/1)" })).toHaveLength(1);
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
