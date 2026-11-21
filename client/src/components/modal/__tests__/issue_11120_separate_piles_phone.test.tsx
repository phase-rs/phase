import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

// Issue #11120: on a phone, "Separate into Two Piles" opened as a panel sized to
// its cards, so it took part of the screen, with small cards, a title, an
// eyebrow and a two-sentence subtitle over them, and a large Confirm filling
// the bottom. The pile-choosing step had the same shell, with two full-width
// buttons in the footer. On a phone both steps now fill the screen and draw
// their cards in the same larger-card grid as Search Library (#11063). On a
// short screen the partition's header is one line, "Separate into Two Piles •
// Player 1 chooses one", with Confirm at its end and the pile counts kept in
// view; the choosing step's header is its instruction with both choose buttons.
// A desktop keeps today's panel.

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

const CREATURES = ["Alpha", "Bravo", "Cosmo"].map((name, index) =>
  gameObjectFactory.creature().onBattlefield().named(name).withId(index + 1).build(),
);
const CREATURE_IDS = CREATURES.map((creature) => creature.id);

function setViewport(viewport: Viewport) {
  Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: viewport.width });
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: viewport.height });
}

function tableState() {
  return gameStateFactory.withPlayers(0, 1).withObjects(...CREATURES);
}

/** The second player separates the three creatures; the first, shown as "Player 1", will choose. */
function partitionState(remainingSubjects = 0) {
  const remaining_subjects = Array.from(
    { length: remainingSubjects },
    (_, index): [number, number[]] => [index + 2, []],
  );
  return tableState()
    .separatePilesPartition({ player: 1, eligible: CREATURE_IDS, remaining_subjects, chooser: 0 })
    .build();
}

/** The first player chooses between the second's piles: Alpha and Bravo, or Cosmo alone. */
function choiceState(pileB = [3]) {
  return tableState()
    .separatePilesChoice({ player: 0, current: { subject: 1, pile_a: [1, 2], pile_b: pileB } })
    .build();
}

function showPrompt(gameState: GameState, activePlayerId: number) {
  useMultiplayerStore.setState({ activePlayerId });
  setGameStoreForTest({ gameState, gameMode: "online" });
  render(<CardChoiceModal />);
}

/** The overlay's header, found from its heading; `getByRole` also proves there is only one. */
function overlayHeader(): HTMLElement {
  const header = screen.getByRole("heading").closest<HTMLElement>(".modal-header-compact");
  expect(header).not.toBeNull();
  return header!;
}

/** The element that sizes the overlay: its width classes decide whether it fills the screen. */
function overlayFrame(): HTMLElement {
  const frame = overlayHeader().closest<HTMLElement>(".card-scale-reset")?.parentElement;
  expect(frame).toBeInstanceOf(HTMLElement);
  return frame!;
}

/**
 * The grid cell a card sits in on a phone, found from the card's label. A
 * partition card carries two (its button's pile label and the card's own), both
 * inside the one cell.
 */
function gridCellOf(name: string): HTMLElement | null {
  return screen.getAllByLabelText(new RegExp(name))[0].closest<HTMLElement>(".card-choice-grid-cell");
}

/** A button on the header's single line, at the shared header confirm's touch height and compact size. */
function headerButton(name: string): HTMLElement {
  const button = within(overlayHeader()).getByRole("button", { name });
  expect(button).toHaveClass("min-h-11", "text-sm");
  expect(screen.getByRole("heading").parentElement).toContainElement(button);
  // A footer would draw a second one.
  expect(screen.getAllByRole("button", { name })).toHaveLength(1);
  return button;
}

beforeEach(() => {
  dispatchMock.mockClear();
});

afterEach(() => {
  cleanup();
  setViewport(DEFAULT_WINDOW);
  useGameStore.setState(useGameStore.getInitialState(), true);
  useMultiplayerStore.setState(useMultiplayerStore.getInitialState(), true);
});

describe("separating into piles on a phone turned sideways", () => {
  beforeEach(() => {
    setViewport(LANDSCAPE_PHONE);
  });

  it("fills the screen with one title line and Confirm at its end", () => {
    showPrompt(partitionState(), 1);

    expect(screen.getByRole("heading").textContent).toBe("Separate into Two Piles • Player 1 chooses one");
    expect(within(overlayHeader()).queryByText("Game Choice")).toBeNull();
    expect(screen.queryByText(/^Separate your creatures/)).toBeNull();
    headerButton("Confirm");
    // Below lg the overlay fills the width; from lg up it is the panel sized to its cards.
    expect(overlayFrame()).toHaveClass("w-full", "lg:w-fit");
  });

  it("keeps the remaining players in the title line", () => {
    showPrompt(partitionState(1), 1);

    expect(screen.getByRole("heading").textContent).toBe(
      "Separate into Two Piles • Player 1 chooses one (1 more player after you)",
    );
  });

  it("draws the cards in the phone's grid under the pile counts, which follow the toggles", () => {
    showPrompt(partitionState(), 1);

    for (const name of ["Alpha", "Bravo", "Cosmo"]) {
      expect(gridCellOf(name)).not.toBeNull();
    }
    const counts = screen.getByText("Pile A:").parentElement!;
    // Outside the scroller, so the counts stay in view while the cards scroll.
    expect(document.querySelector(".card-choice-grid")!.parentElement).not.toContainElement(counts);
    expect(counts).toHaveTextContent("Pile A: 0");
    expect(counts).toHaveTextContent("Pile B: 3");

    fireEvent.click(screen.getByLabelText(/^Bravo .+ pile B$/));
    expect(counts).toHaveTextContent("Pile A: 1");
    expect(counts).toHaveTextContent("Pile B: 2");
    expect(screen.getByLabelText(/^Bravo .+ pile A$/)).toBeInTheDocument();

    fireEvent.click(headerButton("Confirm"));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SubmitPilePartition", data: { pile_a: [2] } });
  });
});

describe("choosing a pile on a phone turned sideways", () => {
  beforeEach(() => {
    setViewport(LANDSCAPE_PHONE);
  });

  it("shows the instruction alone as its one title, with both choose buttons at its end", () => {
    showPrompt(choiceState(), 0);

    expect(screen.getByRole("heading").textContent).toBe("Choose one of Player 2's piles.");
    expect(screen.queryByText("Choose a Pile")).toBeNull();
    expect(within(overlayHeader()).queryByText("Game Choice")).toBeNull();
    expect(overlayFrame()).toHaveClass("w-full", "lg:w-fit");

    fireEvent.click(headerButton("Choose Pile B"));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChoosePile", data: { pile: { type: "B" } } });
    fireEvent.click(headerButton("Choose Pile A"));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "ChoosePile", data: { pile: { type: "A" } } });
  });

  it("keeps the partitions still to come in the title line", () => {
    showPrompt(
      tableState()
        .separatePilesChoice({
          player: 0,
          current: { subject: 1, pile_a: [1, 2], pile_b: [3] },
          pending: [{ subject: 2, pile_a: [], pile_b: [] }],
        })
        .build(),
      0,
    );

    expect(screen.getByRole("heading").textContent).toBe(
      "Choose one of Player 2's piles. (1 more partition after this)",
    );
  });

  it("gives each pile its own grid, side by side while the phone is sideways", () => {
    showPrompt(choiceState(), 0);

    const pileA = screen.getByText("Pile A (2)").parentElement!;
    const pileB = screen.getByText("Pile B (1)").parentElement!;
    expect(within(pileA).getAllByLabelText(/Alpha|Bravo/)).toHaveLength(2);
    expect(within(pileB).getByLabelText(/Cosmo/)).toBeInTheDocument();
    expect(pileA.querySelector(".card-choice-grid")).toContainElement(gridCellOf("Alpha"));
    expect(pileB.querySelector(".card-choice-grid")).toContainElement(gridCellOf("Cosmo"));
    // Stacked when held upright, side by side when turned, so each grid keeps room for two columns.
    expect(pileA.parentElement).toHaveClass("flex-col", "landscape:flex-row");
    // A pile's cards are not buttons: tapping one inspects it, and only the header buttons choose.
    expect(screen.getByLabelText(/Cosmo/).closest("button")).toBeNull();
    fireEvent.click(screen.getByLabelText(/Cosmo/));
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  it("shows an empty pile as empty, with no grid", () => {
    showPrompt(choiceState([]), 0);

    const pileB = screen.getByText("Pile B (0)").parentElement!;
    expect(within(pileB).getByText("(empty)")).toBeInTheDocument();
    expect(pileB.querySelector(".card-choice-grid")).toBeNull();
    // Positive guard: the other pile does draw its grid.
    expect(screen.getByText("Pile A (2)").parentElement!.querySelector(".card-choice-grid")).not.toBeNull();
  });
});

describe("separating into piles and choosing one on a phone held upright", () => {
  beforeEach(() => {
    setViewport(PORTRAIT_PHONE);
  });

  it("stack the piles, each with its grid, under the full header with both choose buttons in the footer", () => {
    showPrompt(choiceState(), 0);

    const pileA = screen.getByText("Pile A (2)").parentElement!;
    expect(pileA.querySelector(".card-choice-grid")).toContainElement(gridCellOf("Alpha"));
    expect(pileA.parentElement).toHaveClass("flex-col");
    expect(screen.getByRole("heading", { name: "Choose a Pile" })).toBeInTheDocument();
    expect(within(overlayHeader()).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Choose Pile A" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Choose Pile B" })).toHaveLength(1);
  });

  it("draws the phone's grid under the full header and footer, which a tall screen keeps", () => {
    showPrompt(partitionState(), 1);

    expect(gridCellOf("Alpha")).not.toBeNull();
    expect(within(overlayHeader()).getByText("Game Choice")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Separate into Two Piles" })).toBeInTheDocument();
    expect(within(overlayHeader()).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Confirm" })).toHaveLength(1);
  });
});

describe("separating into piles and choosing one on a desktop", () => {
  beforeEach(() => {
    setViewport(DESKTOP);
  });

  it("keep the partition's panel: title over subtitle, footer Confirm, cards in a wrapping row", () => {
    showPrompt(partitionState(), 1);

    expect(within(overlayHeader()).getByText("Game Choice")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Separate into Two Piles" })).toBeInTheDocument();
    expect(
      within(overlayHeader()).getByText("Separate your creatures into two piles. Player 1 will choose one."),
    ).toBeInTheDocument();
    expect(within(overlayHeader()).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Confirm" })).toHaveLength(1);
    expect(document.querySelector(".card-choice-grid")).toBeNull();
    expect(screen.getByLabelText(/^Alpha .+ pile B$/).parentElement).toHaveClass("flex-wrap");
  });

  it("keep the choosing step's panel: both choose buttons in the footer, no grid", () => {
    showPrompt(choiceState(), 0);

    expect(screen.getByRole("heading", { name: "Choose a Pile" })).toBeInTheDocument();
    expect(within(overlayHeader()).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Choose Pile A" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Choose Pile B" })).toHaveLength(1);
    // Positive guard: the desktop piles drew their cards, in their wrapping rows rather than a grid.
    expect(within(screen.getByText("Pile A (2)").parentElement!).getAllByLabelText(/Alpha|Bravo/)).toHaveLength(2);
    expect(document.querySelector(".card-choice-grid")).toBeNull();
  });
});
