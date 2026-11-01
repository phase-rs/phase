import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { WaitingFor } from "../../../adapter/types";
import { useGameStore } from "../../../stores/gameStore";
import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory";
import { buildPendingCast, gameStateFactory } from "../../../test/factories/gameStateFactory";
import { CardChoiceModal } from "../CardChoiceModal";

const dispatchMock = vi.fn();
vi.mock("../../../hooks/useGameDispatch", () => ({ useGameDispatch: () => dispatchMock }));

// Store-seeded surface proof, not live owner-art gameplay. The nested printing sits
// in the real Rust wire carrier that the structural client ability type omits.
function setPrompt(options: {
  identity?: string;
  revision?: number;
  printing?: string;
  choices?: number[];
  count?: number;
  player?: number;
} = {}) {
  const choices = options.choices ?? [10, 11, 12];
  const player = options.player ?? 0;
  const count = options.count ?? 2;
  const ability = {
    targets: [],
    trigger_source: { lki: { power: 2 }, printing: { set_code: options.printing ?? "SAAA", collector_number: "1" } },
  };
  const waitingFor: WaitingFor = {
    type: "PayCost",
    data: {
      player,
      kind: { type: "Discard" },
      choices,
      count,
      min_count: count,
      resume: { type: "Spell", Spell: buildPendingCast({ ability }) },
    },
  };
  const objects = choices.map((id) => gameObjectFactory.withId(id).build({
    name: `Hand card ${id}`, owner: player, controller: player, zone: "Hand",
  }));
  const state = gameStateFactory.withPlayers({ id: 0, hand: player === 0 ? choices : [] }, { id: 1, hand: player === 1 ? choices : [] })
    .withObjects(...objects).build({
      waiting_for: waitingFor,
      has_pending_cast: true,
      derived: { waiting_for_identity: options.identity, state_revision: options.revision, unique_authorized_submitter: player },
    });
  useGameStore.setState({ gameMode: "online", gameState: state, waitingFor, viewerInteraction: null });
}

function select(id: number) {
  fireEvent.click(screen.getByLabelText(`Loading Hand card ${id}`));
}

beforeEach(() => {
  dispatchMock.mockReset();
  useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false });
});

afterEach(() => {
  cleanup();
  useGameStore.setState({ gameState: null, waitingFor: null, viewerInteraction: null });
});

describe("PayCost engine identity", () => {
  it("P1 retains a partial selection across a printing-only refreshed snapshot", () => {
    setPrompt({ identity: "same" });
    render(<CardChoiceModal />);
    select(10);
    expect(screen.getByRole("button", { name: "Discard (1/2)" })).toBeDisabled();
    act(() => setPrompt({ identity: "same", printing: "SBBB" }));
    expect(screen.getByRole("button", { name: "Discard (1/2)" })).toBeDisabled();
    select(11);
    fireEvent.click(screen.getByRole("button", { name: "Discard (2/2)" }));
    expect(dispatchMock).toHaveBeenCalledExactlyOnceWith({ type: "SelectCards", data: { cards: [10, 11] } });
  });

  it("P1b retains a partial selection across a revision-only refreshed snapshot", () => {
    setPrompt({ identity: "same", revision: 5 });
    render(<CardChoiceModal />);
    select(10);
    expect(screen.getByRole("button", { name: "Discard (1/2)" })).toBeDisabled();
    act(() => setPrompt({ identity: "same", revision: 6 }));
    expect(screen.getByRole("button", { name: "Discard (1/2)" })).toBeDisabled();
    select(11);
    fireEvent.click(screen.getByRole("button", { name: "Discard (2/2)" }));
    expect(dispatchMock).toHaveBeenCalledExactlyOnceWith({ type: "SelectCards", data: { cards: [10, 11] } });
  });

  it("P2 resets old selection when identity and choices change", () => {
    setPrompt({ identity: "old" });
    render(<CardChoiceModal />);
    select(10);
    act(() => setPrompt({ identity: "new", choices: [20, 21, 22] }));
    expect(screen.getByRole("button", { name: "Discard (0/2)" })).toBeDisabled();
    select(20);
    select(21);
    fireEvent.click(screen.getByRole("button", { name: "Discard (2/2)" }));
    expect(dispatchMock).toHaveBeenCalledExactlyOnceWith({ type: "SelectCards", data: { cards: [20, 21] } });
  });

  it("P3 resets selection and enforces a changed count with identical choices", () => {
    setPrompt({ identity: "old" });
    render(<CardChoiceModal />);
    select(10);
    act(() => setPrompt({ identity: "new", count: 1 }));
    expect(screen.getByRole("button", { name: "Discard (0/1)" })).toBeDisabled();
    select(11);
    fireEvent.click(screen.getByRole("button", { name: "Discard (1/1)" }));
    expect(dispatchMock).toHaveBeenCalledExactlyOnceWith({ type: "SelectCards", data: { cards: [11] } });
  });

  it("P4 preserves legacy reset on a printing-bearing resume change", () => {
    setPrompt();
    render(<CardChoiceModal />);
    select(10);
    act(() => setPrompt({ printing: "SBBB" }));
    expect(screen.getByRole("button", { name: "Discard (0/2)" })).toBeDisabled();
    select(11);
    select(12);
    fireEvent.click(screen.getByRole("button", { name: "Discard (2/2)" }));
    expect(dispatchMock).toHaveBeenCalledExactlyOnceWith({ type: "SelectCards", data: { cards: [11, 12] } });
  });

  it("P5 preserves legacy selection for identical data with new references", () => {
    setPrompt();
    render(<CardChoiceModal />);
    select(10);
    act(() => setPrompt());
    select(11);
    fireEvent.click(screen.getByRole("button", { name: "Discard (2/2)" }));
    expect(dispatchMock).toHaveBeenCalledExactlyOnceWith({ type: "SelectCards", data: { cards: [10, 11] } });
  });

  it("keeps the viewing-seat authorization gate", () => {
    setPrompt({ identity: "other-seat", player: 1 });
    render(<CardChoiceModal />);
    expect(screen.queryByLabelText("Loading Hand card 10")).not.toBeInTheDocument();
    expect(dispatchMock).not.toHaveBeenCalled();
    act(() => setPrompt({ identity: "own-seat", player: 0 }));
    select(10);
    select(11);
    fireEvent.click(screen.getByRole("button", { name: "Discard (2/2)" }));
    expect(dispatchMock).toHaveBeenCalledExactlyOnceWith({ type: "SelectCards", data: { cards: [10, 11] } });
  });
});
