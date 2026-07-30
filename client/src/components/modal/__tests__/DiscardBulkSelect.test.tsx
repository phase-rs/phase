import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CardChoiceModal } from "../CardChoiceModal.tsx";

const dispatchMock = vi.fn();
vi.mock("../../../hooks/useGameDispatch.ts", () => ({ useGameDispatch: () => dispatchMock }));

function makeObject(id: number, name: string): GameObject {
  return buildGameObject({
    id,
    card_id: id,
    zone: "Hand",
    name,
    timestamp: id,
  });
}

function setWaitingFor(waitingFor: WaitingFor, objects?: Record<string, GameObject>) {
  const state = buildGameState({
    objects: objects ?? {},
    waiting_for: waitingFor,
    has_pending_cast: true,
    next_object_id: 100,
  });
  useGameStore.setState({
    gameMode: "online",
    gameState: state,
    waitingFor,
  });
}

const hand = buildObjectMap(
  makeObject(1, "Alpha"),
  makeObject(2, "Bravo"),
  makeObject(3, "Cosmo"),
  makeObject(4, "Delta"),
);
const handIds = [1, 2, 3, 4];

describe("Discard bulk-select grid", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });
  afterEach(cleanup);

  it("DiscardToHandSize: select all (capped) then confirm dispatches exactly count ids", () => {
    setWaitingFor({ type: "DiscardToHandSize", data: { player: 0, count: 2, cards: handIds } }, hand);
    render(<CardChoiceModal />);
    fireEvent.click(screen.getByRole("button", { name: "Select all" }));
    fireEvent.click(screen.getByRole("button", { name: /Discard \(2\/2\)/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [1, 2] } });
  });

  it("Keep instead: keeping 2 of 4 dispatches the complementary 2 to discard", () => {
    setWaitingFor({ type: "DiscardToHandSize", data: { player: 0, count: 2, cards: handIds } }, hand);
    render(<CardChoiceModal />);
    fireEvent.click(screen.getByRole("button", { name: "Keep instead" }));
    fireEvent.click(screen.getByRole("button", { name: /Alpha/i })); // keep 1
    fireEvent.click(screen.getByRole("button", { name: /Bravo/i })); // keep 2 -> keepCap reached
    fireEvent.click(screen.getByRole("button", { name: /Discard \(/ })); // confirm (avoids "Discard instead" toggle)
    const call = dispatchMock.mock.calls.find((c) => c[0].type === "SelectCards");
    expect(call?.[0].data.cards.slice().sort((a: number, b: number) => a - b)).toEqual([3, 4]); // complement of {1,2}
    expect(call?.[0].data.cards).toHaveLength(2);
  });

  it("WardDiscardChoice (count 1): no Keep-instead toggle, confirm dispatches one id", () => {
    setWaitingFor({ type: "WardDiscardChoice", data: { player: 0, cards: handIds, pending_effect: {}, remaining: 1 } }, hand);
    render(<CardChoiceModal />);
    expect(screen.queryByRole("button", { name: "Keep instead" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /Cosmo/i }));
    fireEvent.click(screen.getByRole("button", { name: /Discard \(/ })); // confirm button (avoids card badge "Discard")
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [3] } });
  });

  it("DiscardChoice up_to: confirm with zero selected dispatches empty array", () => {
    setWaitingFor({ type: "DiscardChoice", data: { player: 0, count: 3, cards: handIds, source_id: 9, effect_kind: "x", up_to: true } }, hand);
    render(<CardChoiceModal />);
    expect(screen.queryByRole("button", { name: "Keep instead" })).toBeNull(); // up-to: no keep framing
    fireEvent.click(screen.getByRole("button", { name: /Discard \(/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [] } });
  });

  it("resets selection + keep-mode when a new prompt with a different card set arrives", () => {
    // Prompt 1: enable keep-mode and select a card.
    setWaitingFor({ type: "DiscardToHandSize", data: { player: 0, count: 2, cards: handIds } }, hand);
    render(<CardChoiceModal />);
    fireEvent.click(screen.getByRole("button", { name: "Keep instead" }));
    fireEvent.click(screen.getByRole("button", { name: /Alpha/i }));
    expect(screen.getByRole("button", { name: "Discard instead" })).toBeInTheDocument(); // keep-mode on
    expect(screen.getByRole("status")).toHaveTextContent("Keep 1 of 2");

    // A fresh prompt whose eligible set differs (the hand shrank after discarding)
    // changes the content key, so React remounts the modal with clean state.
    act(() => {
      setWaitingFor({ type: "DiscardToHandSize", data: { player: 0, count: 2, cards: [2, 3, 4] } }, hand);
    });

    // Keep-mode off (toggle back to "Keep instead") and nothing selected.
    expect(screen.getByRole("button", { name: "Keep instead" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Discard 0 of 2");
  });

  it("preserves an in-progress selection across an unrelated re-render (no per-render wipe)", () => {
    // WardDiscardChoice passes a freshly-spread `data` literal, so the modal must
    // NOT reset on every parent re-render — e.g. an engine push that replaces
    // gameState without changing the eligible set (multiplayer/opponent action).
    setWaitingFor({ type: "WardDiscardChoice", data: { player: 0, cards: handIds, pending_effect: {}, remaining: 1 } }, hand);
    render(<CardChoiceModal />);
    fireEvent.click(screen.getByRole("button", { name: /Cosmo/i })); // select 1 of 1
    expect(screen.getByRole("status")).toHaveTextContent("Discard 1 of 1");

    // Same eligible cards, new gameState reference -> parent re-renders and the
    // spread `data` literal is rebuilt, but the content key is unchanged so the
    // instance is reused and the selection survives.
    act(() => {
      setWaitingFor({ type: "WardDiscardChoice", data: { player: 0, cards: handIds, pending_effect: {}, remaining: 1 } }, hand);
    });
    expect(screen.getByRole("status")).toHaveTextContent("Discard 1 of 1");
  });
});
