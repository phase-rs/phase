import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";
import { ChoiceOverlay, ConfirmButton } from "../ChoiceOverlay.tsx";

// Issue #11005: on a landscape phone (about 390 CSS px tall) the discard prompt
// spent most of its height on chrome that said "discard" again and again: the
// "Game Choice" eyebrow, the "Discard" title, the instruction, the
// "Discard 0 of 3" counter and a full-width footer confirm. The cards were left
// about 120px. On a short screen the prompt now has one header line, the
// instruction (after the reason, where the title names one: Ward, Connive, a
// cost) with a compact confirm that shows the count, and no footer; the
// counter is a screen-reader-only live region while the confirm shows the same
// count, and "Keep instead" sits in the toolbar row, whose controls take a 44px
// touch height. The discard prompt on a taller screen, and every other
// ChoiceOverlay prompt, keep their layout.

const dispatchMock = vi.fn();
vi.mock("../../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => dispatchMock,
}));

/** A landscape phone, under `useIsCompactHeight`'s 500px threshold. */
const LANDSCAPE_PHONE_HEIGHT = 390;
/** happy-dom's default window height, which the other modal tests render at. */
const DESKTOP_HEIGHT = 768;

const HAND = ["Alpha", "Bravo", "Cosmo", "Delta", "Echo"].map((name, index) =>
  gameObjectFactory.creature().inHand().named(name).withId(index + 1).build(),
);
const HAND_IDS = HAND.map((card) => card.id);

function setViewportHeight(height: number) {
  Object.defineProperty(window, "innerHeight", { configurable: true, writable: true, value: height });
}

function handState() {
  return gameStateFactory.withPlayers({ id: 0, hand: HAND_IDS }, 1).withObjects(...HAND);
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

function clickCard(name: string) {
  fireEvent.click(screen.getByRole("button", { name: new RegExp(name) }));
}

/** The grid's toolbar row, found from its Select all button. */
function toolbarRow(): HTMLElement {
  return screen.getByRole("button", { name: "Select all" }).parentElement!;
}

/** The row gives every control in it, whoever draws it, a 44px touch height. */
const TOUCH_ROW_CLASSES = ["[&_button]:min-h-11", "[&_select]:min-h-11"];

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

describe("the discard prompt on a landscape phone", () => {
  beforeEach(() => {
    setViewportHeight(LANDSCAPE_PHONE_HEIGHT);
  });

  it("shows the instruction as its one title, with the confirm on that line and no footer", () => {
    showPrompt(handState().discardToHandSize({ player: 0, count: 3, cards: HAND_IDS }).build());

    const header = overlayHeader();
    expect(screen.getByRole("heading").textContent).toBe("Choose 3 cards to discard");
    expect(screen.queryByText("Game Choice")).toBeNull();
    expect(screen.queryByText("Discard")).toBeNull();

    const confirm = within(header).getByRole("button", { name: "Discard (0/3)" });
    expect(confirm).toBeDisabled();
    expect(confirm).toHaveClass("min-h-11");
    // The line holding the title and the confirm keeps clear of a phone's rounded corners.
    expect(screen.getByRole("heading").parentElement).toHaveClass(
      "pl-[env(safe-area-inset-left)]",
      "pr-[env(safe-area-inset-right)]",
    );
    expect(screen.getByRole("heading").parentElement).toContainElement(confirm);
    // A footer would draw a second confirm.
    expect(screen.getAllByRole("button", { name: /^Discard \(/ })).toHaveLength(1);
  });

  it("keeps the counter as a live region for screen readers, while the confirm counts the selection", () => {
    showPrompt(handState().discardToHandSize({ player: 0, count: 3, cards: HAND_IDS }).build());

    const counter = screen.getByRole("status");
    expect(counter).toHaveClass("sr-only");
    expect(counter).toHaveAttribute("aria-live", "polite");
    expect(counter).toHaveTextContent("Discard 0 of 3");

    clickCard("Alpha");
    expect(screen.getByRole("status")).toHaveTextContent("Discard 1 of 3");
    expect(within(overlayHeader()).getByRole("button", { name: "Discard (1/3)" })).toBeDisabled();

    clickCard("Bravo");
    clickCard("Cosmo");
    fireEvent.click(within(overlayHeader()).getByRole("button", { name: "Discard (3/3)" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [1, 2, 3] } });
  });

  it("puts Keep instead in the toolbar row, whose controls all take a touch height", () => {
    showPrompt(handState().discardToHandSize({ player: 0, count: 3, cards: HAND_IDS }).build());

    const row = toolbarRow();
    expect(row).toHaveClass(...TOUCH_ROW_CLASSES);
    for (const name of ["Keep instead", "Select all", "Invert", "Clear"]) {
      expect(screen.getByRole("button", { name }).parentElement).toBe(row);
    }
    // The sort and group selects are drawn by the organizer toolbar inside the row.
    expect(row).toContainElement(screen.getByRole("combobox", { name: /^Sort/ }));
    expect(row).toContainElement(screen.getByRole("combobox", { name: /^Group/ }));
  });

  it("shows the keep count in keep mode, where the confirm still counts discards, and discards the rest", () => {
    showPrompt(handState().discardToHandSize({ player: 0, count: 3, cards: HAND_IDS }).build());

    fireEvent.click(screen.getByRole("button", { name: "Keep instead" }));
    expect(screen.getByRole("heading")).toHaveTextContent(/^Choose 2 cards to keep/);
    expect(screen.getByRole("button", { name: "Discard instead" }).parentElement).toBe(toolbarRow());
    expect(within(overlayHeader()).getByRole("button", { name: "Discard (5/3)" })).toBeDisabled();
    const counter = screen.getByRole("status");
    expect(counter).not.toHaveClass("sr-only");
    expect(counter).toHaveTextContent("Keep 0 of 2");

    clickCard("Alpha");
    clickCard("Bravo");
    expect(screen.getByRole("status")).toHaveTextContent("Keep 2 of 2");
    fireEvent.click(within(overlayHeader()).getByRole("button", { name: "Discard (3/3)" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [3, 4, 5] } });

    // Back in discard mode the confirm shows the count again, so the counter is hidden again.
    fireEvent.click(screen.getByRole("button", { name: "Discard instead" }));
    expect(screen.getByRole("status")).toHaveClass("sr-only");
  });

  // Each case names the taller screen's title by a pattern, which must be gone.
  it.each([
    {
      prompt: "an exact discard",
      gameState: () => handState().discardChoice({ player: 0, count: 3, cards: HAND_IDS }).build(),
      tallTitle: /^Discard 3 cards$/,
      title: "Choose 3 cards to discard",
    },
    {
      prompt: "an up-to discard",
      gameState: () => handState().discardChoice({ player: 0, count: 3, cards: HAND_IDS, up_to: true }).build(),
      tallTitle: /^Discard up to 3 cards$/,
      title: "Choose up to 3 cards to discard",
    },
  ])("gives $prompt, whose title only repeats the instruction, the instruction alone", (prompt) => {
    showPrompt(prompt.gameState());

    const header = overlayHeader();
    expect(screen.getByRole("heading").textContent).toBe(prompt.title);
    expect(screen.queryByText(prompt.tallTitle)).toBeNull();
    expect(screen.queryByText("Game Choice")).toBeNull();
    expect(within(header).getByRole("button", { name: /^Discard \(/ })).toHaveClass("min-h-11");
    expect(screen.getAllByRole("button", { name: /^Discard \(/ })).toHaveLength(1);
  });

  it.each([
    {
      prompt: "a Ward discard",
      gameState: () => handState().wardDiscardChoice({ player: 0, cards: HAND_IDS }).build(),
      tallTitle: /Discard a card$/,
      title: "Ward: Choose 1 card to discard",
    },
    {
      prompt: "a Connive discard",
      gameState: () => handState().conniveDiscard({ player: 0, cards: HAND_IDS, count: 2 }).build(),
      tallTitle: /Discard 2 cards$/,
      title: "Connive: Choose 2 cards to discard",
    },
    {
      prompt: "a spell's discard cost",
      gameState: () => handState().payCost({ player: 0, choices: HAND_IDS, count: 1, min_count: 1 }).build(),
      tallTitle: /^Discard as additional cost$/,
      title: "Additional cost: Choose 1 card to discard",
    },
    {
      prompt: "a mana ability's discard cost",
      gameState: () =>
        handState()
          .payCost({
            player: 0,
            choices: HAND_IDS,
            count: 1,
            min_count: 1,
            resume: { type: "ManaAbility", ManaAbility: {} },
          })
          .build(),
      tallTitle: /^Discard for mana ability$/,
      title: "Mana ability: Choose 1 card to discard",
    },
  ])("keeps the reason $prompt's title names, before the instruction", (prompt) => {
    showPrompt(prompt.gameState());

    const header = overlayHeader();
    expect(screen.getByRole("heading").textContent).toBe(prompt.title);
    expect(screen.queryByText(prompt.tallTitle)).toBeNull();
    expect(screen.queryByText("Game Choice")).toBeNull();
    expect(within(header).getByRole("button", { name: /^Discard \(/ })).toHaveClass("min-h-11");
    expect(screen.getAllByRole("button", { name: /^Discard \(/ })).toHaveLength(1);
  });

  it("keeps the reason in keep mode, before the keep instruction", () => {
    showPrompt(handState().conniveDiscard({ player: 0, cards: HAND_IDS, count: 2 }).build());

    fireEvent.click(screen.getByRole("button", { name: "Keep instead" }));

    expect(screen.getByRole("heading").textContent).toMatch(/^Connive: Choose 3 cards to keep/);
  });

  it("moves a discard cost's Cancel into the header line", () => {
    showPrompt(handState().payCost({ player: 0, choices: HAND_IDS, count: 1, min_count: 1 }).build());

    const cancel = within(overlayHeader()).getByRole("button", { name: "Cancel" });
    expect(cancel).toHaveClass("min-h-11");
    expect(screen.getAllByRole("button", { name: "Cancel" })).toHaveLength(1);

    fireEvent.click(cancel);
    expect(dispatchMock).toHaveBeenCalledWith({ type: "CancelCast" });
  });

  it("offers no Cancel for a mana ability's discard cost, as on a taller screen", () => {
    showPrompt(
      handState()
        .payCost({ player: 0, choices: HAND_IDS, count: 1, resume: { type: "ManaAbility", ManaAbility: {} } })
        .build(),
    );

    // Positive guard: the single header line is drawn, with its confirm.
    expect(within(overlayHeader()).getByRole("button", { name: "Discard (0/1)" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull();
  });

  it("keeps the selection and the sort when the phone turns upright", () => {
    showPrompt(handState().discardToHandSize({ player: 0, count: 3, cards: HAND_IDS }).build());
    fireEvent.change(screen.getByRole("combobox", { name: /^Sort/ }), { target: { value: "name" } });
    clickCard("Alpha");
    // Positive guard: the short-screen layout is drawn before the turn.
    expect(screen.queryByText("Game Choice")).toBeNull();
    expect(within(overlayHeader()).getByRole("button", { name: "Discard (1/3)" })).toBeInTheDocument();

    act(() => {
      setViewportHeight(DESKTOP_HEIGHT);
      window.dispatchEvent(new Event("resize"));
    });

    // Positive guard: the taller layout is drawn now.
    expect(screen.getByText("Game Choice")).toBeInTheDocument();
    // The grid was not remounted, so its own sort state survived the switch.
    expect(screen.getByRole("combobox", { name: /^Sort/ })).toHaveValue("name");
    expect(screen.getByRole("status")).toHaveTextContent("Discard 1 of 3");
  });
});

describe("the discard prompt on a taller screen", () => {
  beforeEach(() => {
    setViewportHeight(DESKTOP_HEIGHT);
  });

  it("keeps the eyebrow, the title over the instruction, the visible counter and the footer confirm", () => {
    showPrompt(handState().discardToHandSize({ player: 0, count: 3, cards: HAND_IDS }).build());

    const header = overlayHeader();
    expect(within(header).getByText("Game Choice")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Discard" })).toBeInTheDocument();
    expect(within(header).getByText("Choose 3 cards to discard")).toBeInTheDocument();
    expect(within(header).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: "Discard (0/3)" })).toHaveLength(1);

    expect(screen.getByRole("status")).not.toHaveClass("sr-only");
    const row = toolbarRow();
    for (const className of TOUCH_ROW_CLASSES) {
      expect(row).not.toHaveClass(className);
    }
    expect(screen.getByRole("button", { name: "Keep instead" }).parentElement).not.toBe(row);
  });

  it("keeps a reason in its own title, over the plain instruction", () => {
    showPrompt(handState().wardDiscardChoice({ player: 0, cards: HAND_IDS }).build());

    expect(screen.getByRole("heading").textContent).toMatch(/^Ward .*Discard a card$/);
    expect(within(overlayHeader()).getByText("Choose 1 card to discard")).toBeInTheDocument();
    expect(screen.queryByText(/^Ward: /)).toBeNull();
  });
});

describe("other choice prompts on a landscape phone", () => {
  beforeEach(() => {
    setViewportHeight(LANDSCAPE_PHONE_HEIGHT);
  });

  it("keep the full header and the footer in a prompt that shares the discard grid", () => {
    const creatures = ["Foxtrot", "Golf"].map((name, index) =>
      gameObjectFactory.creature().onBattlefield().tapped().named(name).withId(index + 10).build(),
    );
    const creatureIds = creatures.map((creature) => creature.id);
    showPrompt(
      gameStateFactory
        .withPlayers(0, 1)
        .withObjects(...creatures)
        .heroEventChoice({ player: 0, event: { type: "ChromeOverload" }, cards: creatureIds, count: 1 })
        .build(),
    );

    const header = overlayHeader();
    expect(within(header).getByText("Game Choice")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Chrome Dome's Overload" })).toBeInTheDocument();
    expect(within(header).getByText("Choose a tapped creature you control to sacrifice")).toBeInTheDocument();
    expect(within(header).queryByRole("button")).toBeNull();
    expect(screen.getByRole("button", { name: "Sacrifice (0/1)" })).toBeInTheDocument();

    const counter = screen.getByRole("status");
    expect(counter).toHaveTextContent("Choose 0 of 1");
    expect(counter).not.toHaveClass("sr-only");
    for (const className of TOUCH_ROW_CLASSES) {
      expect(toolbarRow()).not.toHaveClass(className);
    }
  });

  it("keep the full header and the footer when a prompt does not opt in", () => {
    render(
      <ChoiceOverlay title="Choose a mode" subtitle="Choose one mode" footer={<ConfirmButton onClick={vi.fn()} />}>
        <span />
      </ChoiceOverlay>,
    );

    const header = overlayHeader();
    expect(within(header).getByText("Game Choice")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Choose a mode" })).toBeInTheDocument();
    expect(within(header).getByText("Choose one mode")).toBeInTheDocument();
    expect(within(header).queryByRole("button")).toBeNull();
    expect(screen.getByRole("button", { name: "Confirm" })).toBeInTheDocument();
  });
});
