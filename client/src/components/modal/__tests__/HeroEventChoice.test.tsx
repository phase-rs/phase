import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, WaitingFor } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildHeroEventChoiceWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
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

const cards = buildObjectMap(
  makeObject(1, "Alpha"),
  makeObject(2, "Bravo"),
  makeObject(3, "Cosmo"),
  makeObject(4, "Delta"),
);
const cardIds = [1, 2, 3, 4];

describe("HeroEventChoice modal", () => {
  beforeEach(() => {
    dispatchMock.mockClear();
    useMultiplayerStore.setState({ activePlayerId: 0 });
  });
  afterEach(cleanup);

  it("ChromeOverload exact-1: confirm disabled until one selected, dispatches the id", () => {
    setWaitingFor(
      buildHeroEventChoiceWaitingFor({
        data: {
          player: 0,
          event: { type: "ChromeOverload" },
          event_id: 9,
          cards: cardIds,
          count: 1,
          copies_pending: 0,
        },
      }),
      cards,
    );
    render(<CardChoiceModal />);
    expect(screen.getByText("Chrome Dome's Overload")).toBeInTheDocument();
    // Confirm button (avoids card badge "Sacrifice").
    expect(screen.getByRole("button", { name: /Sacrifice \(/ })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: /Bravo/i }));
    fireEvent.click(screen.getByRole("button", { name: /Sacrifice \(1\/1\)/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [2] } });
  });

  it("KrangStratagem up_to: team-owed subtitle renders, empty confirm dispatches []", () => {
    setWaitingFor(
      buildHeroEventChoiceWaitingFor({
        data: {
          player: 0,
          event: { type: "KrangStratagem", data: { remaining: 2 } },
          event_id: 9,
          cards: cardIds,
          count: 2,
          up_to: true,
          copies_pending: 0,
        },
      }),
      cards,
    );
    render(<CardChoiceModal />);
    expect(screen.getByText("Krang's Stratagem")).toBeInTheDocument();
    expect(screen.getByText(/team owes 2 total/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Discard \(/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [] } });
  });

  it("KrangStratagem min floor: confirm disabled below min, range subtitle renders", () => {
    setWaitingFor(
      buildHeroEventChoiceWaitingFor({
        data: {
          player: 0,
          event: { type: "KrangStratagem", data: { remaining: 2 } },
          event_id: 9,
          cards: cardIds,
          count: 2,
          up_to: true,
          min: 1,
          copies_pending: 0,
        },
      }),
      cards,
    );
    render(<CardChoiceModal />);
    expect(screen.getByText(/Choose 1 to 2 cards/)).toBeInTheDocument();
    // Below the floor the confirm stays disabled.
    expect(screen.getByRole("button", { name: /Discard \(/ })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: /Bravo/i }));
    fireEvent.click(screen.getByRole("button", { name: /Discard \(1\/2\)/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [2] } });
  });

  it("SavantiCurse exact-2: plural subtitle, two picks dispatch both", () => {
    setWaitingFor(
      buildHeroEventChoiceWaitingFor({
        data: {
          player: 0,
          event: { type: "SavantiCurse" },
          event_id: 9,
          cards: cardIds,
          count: 2,
          copies_pending: 0,
        },
      }),
      cards,
    );
    render(<CardChoiceModal />);
    expect(
      screen.getByText("Choose 2 creatures you control to return to their owners' hands"),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Alpha/i }));
    fireEvent.click(screen.getByRole("button", { name: /Bravo/i }));
    fireEvent.click(screen.getByRole("button", { name: /Confirm \(2\/2\)/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [1, 2] } });
  });

  it("VillainMayhem: team-spare text renders, three picks dispatch", () => {
    setWaitingFor(
      buildHeroEventChoiceWaitingFor({
        data: {
          player: 0,
          event: { type: "VillainMayhem" },
          event_id: 9,
          cards: cardIds,
          count: 3,
          copies_pending: 0,
        },
      }),
      cards,
    );
    render(<CardChoiceModal />);
    expect(screen.getByText("Villain Mayhem!")).toBeInTheDocument();
    expect(screen.getByText(/choose 3 creatures to spare/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Alpha/i }));
    fireEvent.click(screen.getByRole("button", { name: /Bravo/i }));
    fireEvent.click(screen.getByRole("button", { name: /Cosmo/i }));
    fireEvent.click(screen.getByRole("button", { name: /Confirm \(3\/3\)/ }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "SelectCards", data: { cards: [1, 2, 3] } });
  });

  it("player gate: another hero's prompt renders nothing for this client", () => {
    setWaitingFor(
      buildHeroEventChoiceWaitingFor({
        data: {
          player: 1,
          event: { type: "ChromeOverload" },
          event_id: 9,
          cards: cardIds,
          count: 1,
          copies_pending: 0,
        },
      }),
      cards,
    );
    render(<CardChoiceModal />);
    expect(screen.queryByText("Chrome Dome's Overload")).toBeNull();
  });
});
