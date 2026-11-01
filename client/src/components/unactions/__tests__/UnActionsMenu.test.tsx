import { act } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { UnActionState } from "../../../adapter/types";
import { useGameStore } from "../../../stores/gameStore";
import { gameStateFactory } from "../../../test/factories/gameStateFactory";
import { UnActionsMenu } from "../UnActionsMenu";

const { dispatch } = vi.hoisted(() => ({ dispatch: vi.fn() }));

vi.mock("../../../hooks/useGameDispatch", () => ({
  useGameDispatch: () => dispatch,
}));

function setUnActions(unActions: UnActionState) {
  act(() => {
    useGameStore.setState({
      gameMode: "ai",
      gameState: gameStateFactory.build({ un_actions: unActions }),
    });
  });
}

beforeEach(() => dispatch.mockReset());
afterEach(() => {
  cleanup();
  act(() => useGameStore.setState({ gameState: null }));
});

describe("UnActionsMenu", () => {
  it("renders nothing for a player who has not opted in", () => {
    setUnActions({ enabled_players: [], menu: [{ type: "Physical", data: "TouchFace" }] });
    const { container } = render(<UnActionsMenu />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing when no card in the game responds to a gesture", () => {
    setUnActions({ enabled_players: [0], menu: [] });
    const { container } = render(<UnActionsMenu />);
    expect(container).toBeEmptyDOMElement();
  });

  it("lists only the engine-provided gestures and performs the chosen one", () => {
    setUnActions({ enabled_players: [0], menu: [{ type: "Physical", data: "TouchFace" }] });
    render(<UnActionsMenu />);

    fireEvent.click(screen.getByRole("button", { name: /open un-actions menu/i }));
    expect(screen.getByRole("menuitem", { name: /touch your face/i })).toBeInTheDocument();
    expect(screen.queryByRole("menuitem", { name: /laugh/i })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("menuitem", { name: /touch your face/i }));
    expect(dispatch).toHaveBeenCalledWith({
      type: "UnAction",
      data: { action: { type: "PerformTell", data: { tell: { type: "Physical", data: "TouchFace" } } } },
    });
  });

  it("collects a spoken word before performing it", () => {
    setUnActions({ enabled_players: [0], menu: [{ type: "SaysWord" }] });
    render(<UnActionsMenu />);

    fireEvent.click(screen.getByRole("button", { name: /open un-actions menu/i }));
    fireEvent.click(screen.getByRole("menuitem", { name: /say a word/i }));
    fireEvent.change(screen.getByPlaceholderText("A word"), { target: { value: "creature" } });
    fireEvent.click(screen.getByRole("button", { name: /say it!/i }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "UnAction",
      data: { action: { type: "PerformTell", data: { tell: { type: "Said", data: { text: "creature" } } } } },
    });
  });
});
