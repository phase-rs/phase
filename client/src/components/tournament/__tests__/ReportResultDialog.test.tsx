import { fireEvent, render, screen, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { PlayerSummary, TournamentPairingView } from "../../../adapter/types";
import { ReportResultDialog } from "../ReportResultDialog";
import { expectCatalogValuePresent, expectNoRawKeyPaths } from "./tournamentTestUtils";

// This repo's vitest config does not enable `globals`, so RTL never registers
// its own auto-cleanup. Without this, every render in this file leaks into the
// next test's DOM and row-indexed queries silently address the wrong render.
afterEach(cleanup);

const duoSeats: PlayerSummary[] = [
  { player_key: "ann", display_name: "Ann", dropped: false },
  { player_key: "bob", display_name: "Bob", dropped: false },
];

/**
 * The hostile fixture behind F1: a **two-seat** pairing in an **arity-3**
 * tournament. `MatchArity::short_pod_size` is `arity - 1`
 * (`crates/lobby-broker/src/tournament.rs:123-126`), and `partition_round` at
 * five entrants and three seats yields one 3-seat pod plus this one. The
 * broker's `validate_match_result` branches on `players.len() == 2`, so this
 * pairing REQUIRES a game-wins tally even though the tournament's arity is 3.
 * A dialog gated on the tournament arity would submit an empty map here and be
 * refused every time.
 */
const shortPodAtArityThree: TournamentPairingView = {
  id: 7,
  round: 1,
  players: duoSeats,
  outcome: null,
};

const fullPodAtArityThree: TournamentPairingView = {
  id: 8,
  round: 1,
  players: [
    { player_key: "cid", display_name: "Cid", dropped: false },
    { player_key: "dee", display_name: "Dee", dropped: false },
    { player_key: "eve", display_name: "Eve", dropped: false },
  ],
  outcome: null,
};

function renderDialog(pairing: TournamentPairingView, onSubmit = vi.fn()) {
  const result = render(
    <ReportResultDialog
      isOpen
      pairing={pairing}
      onSubmit={onSubmit}
      onCancel={vi.fn()}
    />,
  );
  return { ...result, onSubmit };
}

describe("ReportResultDialog", () => {
  // V22 — the gate is the pairing's seat count, never the tournament's arity.
  it("shows game-wins inputs for a two-seat pairing in an arity-3 tournament", () => {
    renderDialog(shortPodAtArityThree);

    expect(screen.getByLabelText("Game wins for Ann")).toBeInTheDocument();
    expect(screen.getByLabelText("Game wins for Bob")).toBeInTheDocument();
    expect(screen.getByText("Game wins")).toBeInTheDocument();
  });

  it("shows no game-wins inputs for a three-seat pod", () => {
    renderDialog(fullPodAtArityThree);

    expect(screen.queryByLabelText("Game wins for Cid")).not.toBeInTheDocument();
    expect(screen.queryByText("Game wins")).not.toBeInTheDocument();
  });

  // V23 — a pod submits an empty tally, and the winner is the seat's
  // `player_key`, never its display name.
  it("submits a pod result with an empty tally and the winner's player key", () => {
    const { onSubmit } = renderDialog(fullPodAtArityThree);

    fireEvent.click(screen.getByLabelText("Dee"));
    fireEvent.click(screen.getByRole("button", { name: "Submit Result" }));

    expect(onSubmit).toHaveBeenCalledWith({
      Decisive: { winner: "dee", game_wins: {} },
    });
  });

  it("submits a head-to-head result with the entered tally in seat order", () => {
    const { onSubmit } = renderDialog(shortPodAtArityThree);

    fireEvent.click(screen.getByLabelText("Ann"));
    fireEvent.change(screen.getByLabelText("Game wins for Ann"), {
      target: { value: "2" },
    });
    fireEvent.change(screen.getByLabelText("Game wins for Bob"), {
      target: { value: "1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Submit Result" }));

    expect(onSubmit).toHaveBeenCalledWith({
      Decisive: { winner: "ann", game_wins: { ann: 2, bob: 1 } },
    });
  });

  // V24 — the unit variant crosses the wire as the bare string.
  it("submits a draw as the bare string", () => {
    const { onSubmit } = renderDialog(shortPodAtArityThree);

    fireEvent.click(screen.getByLabelText("Draw"));
    fireEvent.click(screen.getByRole("button", { name: "Submit Result" }));

    expect(onSubmit).toHaveBeenCalledWith("Draw");
  });

  // V25 — Bo3 legality and the winner-versus-tally consistency check belong to
  // `validate_match_result` alone. An inconsistent submission must reach the
  // wire rather than being caught here.
  it("submits an inconsistent tally without pre-rejecting it", () => {
    const { onSubmit } = renderDialog(shortPodAtArityThree);

    fireEvent.click(screen.getByLabelText("Ann"));
    fireEvent.change(screen.getByLabelText("Game wins for Ann"), {
      target: { value: "0" },
    });
    fireEvent.change(screen.getByLabelText("Game wins for Bob"), {
      target: { value: "2" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Submit Result" }));

    expect(onSubmit).toHaveBeenCalledWith({
      Decisive: { winner: "ann", game_wins: { ann: 0, bob: 2 } },
    });
  });

  /**
   * Deliberate WAI-ARIA deviation from `ConcedeDialog`: this is a
   * result-entry form opened on purpose, not an urgent interruption demanding
   * acknowledgement, so it is a `dialog` and not an `alertdialog`.
   */
  it("is a dialog, not an alertdialog", () => {
    renderDialog(shortPodAtArityThree);

    expect(screen.getByRole("dialog")).toHaveAttribute("aria-modal", "true");
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  });

  it("renders nothing while closed", () => {
    render(
      <ReportResultDialog
        isOpen={false}
        pairing={shortPodAtArityThree}
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );

    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("cancels through the shared chrome action", () => {
    const onCancel = vi.fn();
    render(
      <ReportResultDialog
        isOpen
        pairing={shortPodAtArityThree}
        onSubmit={vi.fn()}
        onCancel={onCancel}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onCancel).toHaveBeenCalled();
  });

  it("shows the busy label while a submission is in flight", () => {
    render(
      <ReportResultDialog
        isOpen
        pairing={shortPodAtArityThree}
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
        submitting
      />,
    );

    expect(screen.getByRole("button", { name: "Submitting…" })).toBeDisabled();
  });

  // V26 — every user-visible string routes through `t()`.
  it("routes all copy through the tournament catalog", () => {
    const { container } = renderDialog(shortPodAtArityThree);

    expectNoRawKeyPaths(container);
    expectCatalogValuePresent(container, "Report Result");
  });
});
