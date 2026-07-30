import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  DecisionPoint,
  GameState,
  WaitingFor,
} from "../../../adapter/types.ts";
import {
  buildGameState,
  buildLoopShortcutWaitingFor,
  buildRespondToShortcutWaitingFor,
} from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";
import { DeclareShortcutModal, RespondToShortcutModal } from "../LoopShortcutModal.tsx";

const dispatchMock = vi.fn();

// A ConvokeTaps decision-point with two tappable creatures (informational — the
// engine auto-taps via select_convoke_taps; the modal renders it read-only).
const convokePoint: DecisionPoint = {
  slot: { source: { ThisObject: { source_id: 40, incarnation: null } }, index: 0 },
  kind: { ConvokeTaps: { tappable: [40, 41] } },
};

function seed(waitingFor: WaitingFor, overrides: Partial<GameState> = {}) {
  const gameState = buildGameState({
    objects: {},
    priority_player: 0,
    waiting_for: waitingFor,
    ...overrides,
  });
  setGameStoreForTest({ gameState, waitingFor, dispatch: dispatchMock });
}

describe("LoopShortcutModal", () => {
  beforeEach(() => {
    dispatchMock.mockReset();
    dispatchMock.mockResolvedValue(undefined);
  });

  afterEach(() => {
    cleanup();
  });

  // T1: the declare modal renders directly from the engine schema/certificate —
  // win_kind, iteration_count, and the read-only ConvokeTaps count. A wrong field
  // read renders a different/absent string and fails.
  it("renders the offer summary from certificate + schema (T1)", () => {
    seed(buildLoopShortcutWaitingFor({ schema: { points: [convokePoint] } }));
    render(<DeclareShortcutModal />);

    expect(screen.getByText("This loop deals lethal damage.")).toBeInTheDocument();
    expect(screen.getByText("Repeat until the game ends.")).toBeInTheDocument();
    expect(
      screen.getByText("Auto-taps up to 2 creatures for convoke each iteration."),
    ).toBeInTheDocument();
  });

  // T2: confirm dispatches the exact declare payload, echoing the schema's
  // iteration_count (UntilLethal) with template: null.
  it("dispatches DeclareShortcut echoing UntilLethal with template null (T2)", () => {
    seed(buildLoopShortcutWaitingFor());
    render(<DeclareShortcutModal />);

    fireEvent.click(screen.getByRole("button", { name: "Take the shortcut" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: { count: "UntilLethal", template: null },
    });
  });

  // T2 echo-guard: a Fixed(1) schema must dispatch count:{Fixed:1}, proving the
  // count is echoed from the schema, not a hardcoded "UntilLethal".
  it("echoes a Fixed iteration_count into the dispatch (T2 echo-guard)", () => {
    seed(buildLoopShortcutWaitingFor({ schema: { iteration_count: { Fixed: 1 } } }));
    render(<DeclareShortcutModal />);

    fireEvent.click(screen.getByRole("button", { name: "Take the shortcut" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: { count: { Fixed: 1 }, template: null },
    });
  });

  // T3: display-only — a ConvokeTaps point renders a read-only info line and NO
  // tappable-selection control (the confirm button is the only control), and
  // confirm still dispatches template: null.
  it("shows ConvokeTaps read-only with no selection control (T3)", () => {
    seed(buildLoopShortcutWaitingFor({ schema: { points: [convokePoint] } }));
    render(<DeclareShortcutModal />);

    expect(
      screen.getByText("Auto-taps up to 2 creatures for convoke each iteration."),
    ).toBeInTheDocument();
    // The only interactive controls are confirm + decline — no per-creature tap UI.
    const buttons = screen.getAllByRole("button");
    expect(buttons).toHaveLength(2);
    expect(buttons.map((b) => b.textContent)).toEqual([
      "Take the shortcut",
      "Decline the shortcut",
    ]);

    fireEvent.click(buttons[0]);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "DeclareShortcut",
      data: { count: "UntilLethal", template: null },
    });
  });

  // T3b (CR 732.2a): the declare modal offers a Decline control that dispatches the
  // payloadless DeclineShortcut — suggesting a shortcut is optional. Distinct from the
  // opponent-side Shorten; this is the controller declining their own auto-offer.
  it("dispatches DeclineShortcut on decline (T3b)", () => {
    seed(buildLoopShortcutWaitingFor());
    render(<DeclareShortcutModal />);

    fireEvent.click(screen.getByRole("button", { name: "Decline the shortcut" }));
    expect(dispatchMock).toHaveBeenCalledWith({ type: "DeclineShortcut" });
  });

  // T4: the respond window renders the proposal and Accept dispatches Accept.
  it("renders the proposal and dispatches Accept (T4)", () => {
    seed(buildRespondToShortcutWaitingFor());
    render(<RespondToShortcutModal />);

    expect(screen.getByText("This loop deals lethal damage.")).toBeInTheDocument();
    expect(screen.getByText("Repeat until the game ends.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Accept" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "RespondToShortcut",
      data: { response: "Accept" },
    });
  });

  // T5: "Break out" dispatches the Shorten payload shape (placeholder at_iteration).
  it("dispatches Shorten on break out (T5)", () => {
    seed(buildRespondToShortcutWaitingFor());
    render(<RespondToShortcutModal />);

    fireEvent.click(screen.getByRole("button", { name: "Break out" }));
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "RespondToShortcut",
      data: { response: { Shorten: { at_iteration: 1 } } },
    });
  });

  // T6 (non-vacuity): both modals self-gate — a non-matching waitingFor.type
  // renders nothing and never dispatches.
  it("renders nothing on a non-matching waitingFor type (T6)", () => {
    seed({ type: "Priority", data: { player: 0 } });

    const declare = render(<DeclareShortcutModal />);
    expect(declare.container.firstChild).toBeNull();
    cleanup();

    const respond = render(<RespondToShortcutModal />);
    expect(respond.container.firstChild).toBeNull();

    expect(dispatchMock).not.toHaveBeenCalled();
  });

  // T7 (non-vacuity + MP-safety + site-1 revert-guard): a LoopShortcut whose
  // proposer is the opponent (seat 1) renders nothing for the local seat (0)
  // and never dispatches. `turn_decision_controller: null` rules out the
  // delegated-turn branch, so the ONLY reason it null-renders is the seat gate.
  // (If the usePlayerId site-1 fix were reverted, even a proposer:0 offer would
  // null-render → T1/T2 would fail — so those tests non-vacuously cover site-1.)
  it("renders nothing for a non-actor seat (T7)", () => {
    seed(buildLoopShortcutWaitingFor({ proposer: 1 }), {
      turn_decision_controller: null,
      active_player: 0,
    });

    const { container } = render(<DeclareShortcutModal />);
    expect(container.firstChild).toBeNull();
    expect(dispatchMock).not.toHaveBeenCalled();
  });
});
