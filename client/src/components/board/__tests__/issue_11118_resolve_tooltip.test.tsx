import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { dispatchAction } from "../../../game/dispatch.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildStackEntry, gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { ActionButton } from "../ActionButton.tsx";

// Issue #11118: on a phone, the Resolve tooltip covered the bottom right of the
// board whenever the Resolve buttons were on screen. GameplayTooltip opened on
// `pointerenter` and on any focus. Nothing in the client focuses Resolve when
// it appears (FocusScope only hands focus back to it when a dialog opened over
// it closes). The likely path is the pointer: the browser sends `pointerenter`
// to a button that appears under a pointer that has not moved, a phone's
// browser keeps the mouse it emulates for touch where the last tap was, and
// Resolve appears in the same slot as the button the player taps to pass
// priority. A tap on Resolve also
// focuses it, and that focus outlives the tap. These tests drive both paths
// through the real Resolve button.

vi.mock("../../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchResolveAll: vi.fn(),
}));

const RESOLVE_TOOLTIP =
  "Pass priority so the top stack item can resolve if every player also passes. Shortcut: Space.";

// happy-dom matches `:focus-visible` for every focused element; a browser
// matches it mainly for keyboard focus, and not for the focus a tap gives.
let focusVisible = false;

function showPriority(stackSize: number) {
  const stack = Array.from({ length: stackSize }, () => buildStackEntry());
  const gameState = gameStateFactory.withPlayers(0, 1).withStack(...stack).priority(0).build();
  setGameStoreForTest({ gameState, legalActions: [] });
}

describe("issue #11118: the Resolve tooltip opens only for hover or keyboard focus", () => {
  beforeEach(() => {
    focusVisible = false;
    const matches = Element.prototype.matches;
    vi.spyOn(Element.prototype, "matches").mockImplementation(function (this: Element, selector: string) {
      return selector === ":focus-visible" ? focusVisible : matches.call(this, selector);
    });
    useGameStore.setState({ gameMode: "online" });
    useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false, actionPending: false });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    vi.restoreAllMocks();
    useGameStore.setState(useGameStore.getInitialState(), true);
    useMultiplayerStore.setState(useMultiplayerStore.getInitialState(), true);
  });

  it("stays closed when Resolve appears under a pointer that has not moved", () => {
    showPriority(0);
    render(<ActionButton />);
    expect(screen.queryByRole("button", { name: "Resolve" })).not.toBeInTheDocument();

    showPriority(1);
    const resolve = screen.getByRole("button", { name: "Resolve" });
    const tooltip = screen.getByText(RESOLVE_TOOLTIP);
    fireEvent.pointerOver(resolve, { pointerType: "mouse" });
    fireEvent.pointerEnter(resolve, { pointerType: "mouse" });

    expect(tooltip).not.toBeVisible();

    fireEvent.pointerMove(resolve, { pointerType: "mouse" });
    expect(tooltip).toBeVisible();
  });

  it("stays closed after a tap on Resolve passes priority and focuses it", () => {
    showPriority(1);
    render(<ActionButton />);
    const resolve = screen.getByRole("button", { name: "Resolve" });
    const tooltip = screen.getByText(RESOLVE_TOOLTIP);

    fireEvent.pointerEnter(resolve, { pointerType: "touch" });
    fireEvent.pointerDown(resolve, { pointerType: "touch" });
    fireEvent.pointerUp(resolve, { pointerType: "touch" });
    fireEvent.pointerLeave(resolve, { pointerType: "touch" });
    act(() => resolve.focus());
    fireEvent.click(resolve);

    expect(vi.mocked(dispatchAction)).toHaveBeenCalledWith({ type: "PassPriority" });
    expect(resolve).toHaveFocus();
    expect(tooltip).not.toBeVisible();

    act(() => resolve.blur());
    focusVisible = true;
    act(() => resolve.focus());
    expect(tooltip).toBeVisible();
  });
});
