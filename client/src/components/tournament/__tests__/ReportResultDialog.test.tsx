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

/**
 * The hostile fixture behind V33b: a **later round's** pairing that shares
 * exactly **one** seat with `shortPodAtArityThree` (Ann).
 *
 * One shared seat is all it takes, and it is not an exotic arrangement — it is
 * what every multi-round tournament produces for every player. That single
 * overlap is what makes stale entry state *silently accepted* rather than
 * merely refused: Ann's carried-over `2` plus Cid's defaulted `0` is the tally
 * `(2, 0)`, which `validate_match_result` accepts as a legal completed Bo3
 * (`crates/lobby-broker/src/tournament.rs:1000-1009` — `(2,0)` is a listed
 * legal tally and `winner == expected` because `wa > wb`). The broker records
 * a 2-0 win for Ann that the organizer never entered for this pairing.
 */
const annVersusCidNextRound: TournamentPairingView = {
  id: 12,
  round: 2,
  players: [
    { player_key: "ann", display_name: "Ann", dropped: false },
    { player_key: "cid", display_name: "Cid", dropped: false },
  ],
  outcome: null,
};

function dialogFor(pairing: TournamentPairingView, onSubmit: () => void) {
  return (
    <ReportResultDialog
      isOpen
      pairing={pairing}
      onSubmit={onSubmit}
      onCancel={vi.fn()}
    />
  );
}

function renderDialog(pairing: TournamentPairingView, onSubmit = vi.fn()) {
  const result = render(dialogFor(pairing, onSubmit));
  return { ...result, onSubmit };
}

function submitButton() {
  return screen.getByRole("button", { name: "Submit Result" });
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

  /**
   * V33 — entry state belongs to **one** pairing, enforced by the component.
   *
   * The dialog is a long-lived mount in the surface that owns it: one dialog
   * is shown for whichever pairing the organizer picked. Entry state that
   * outlives a `pairing` change is therefore reachable in production, not
   * hypothetical, and both halves below are real submissions the organizer
   * never made. Enforcing it here rather than documenting `key={pairing.id}`
   * at the mount site is deliberate: the mount site lives in a later phase and
   * an unenforced contract is exactly what this class of bug is made of.
   */
  describe("resets entry state when the pairing changes", () => {
    // V33a — swapping to a pod strands a winner who is not one of its seats.
    it("clears a head-to-head result when the pairing becomes a three-seat pod", () => {
      const onSubmit = vi.fn();
      const { rerender } = render(dialogFor(shortPodAtArityThree, onSubmit));

      fireEvent.click(screen.getByLabelText("Ann"));
      fireEvent.change(screen.getByLabelText("Game wins for Ann"), {
        target: { value: "2" },
      });
      fireEvent.change(screen.getByLabelText("Game wins for Bob"), {
        target: { value: "1" },
      });
      // Reach-guard: the entry really landed. Without this, every assertion
      // after the swap is satisfiable by a dialog that never accepted input.
      expect(submitButton()).toBeEnabled();

      rerender(dialogFor(fullPodAtArityThree, onSubmit));

      // Ann is not a seat of this pod, so nothing shows as chosen — and the
      // submit affordance must agree with what the organizer can see.
      for (const radio of screen.getAllByRole("radio")) {
        expect(radio).not.toBeChecked();
      }
      expect(submitButton()).toBeDisabled();
      fireEvent.click(submitButton());
      // Pre-fix this emitted `{Decisive:{winner:"ann", game_wins:{}}}`, which
      // the broker refuses ("Winner must be one of the pod's players",
      // `tournament.rs:976-977`).
      expect(onSubmit).not.toHaveBeenCalled();

      // Paired positive: the pod is still fully usable afterwards, so
      // "resets and then stays inert" cannot pass.
      fireEvent.click(screen.getByLabelText("Dee"));
      fireEvent.click(submitButton());
      expect(onSubmit).toHaveBeenCalledWith({
        Decisive: { winner: "dee", game_wins: {} },
      });
    });

    // V33b — the dangerous half: the carried-over result is *legal* for the
    // new pairing, so nothing downstream rejects it.
    it("carries no winner or tally into a different head-to-head pairing", () => {
      const onSubmit = vi.fn();
      const { rerender } = render(dialogFor(shortPodAtArityThree, onSubmit));

      fireEvent.click(screen.getByLabelText("Ann"));
      fireEvent.change(screen.getByLabelText("Game wins for Ann"), {
        target: { value: "2" },
      });
      fireEvent.change(screen.getByLabelText("Game wins for Bob"), {
        target: { value: "1" },
      });
      // Reach-guard, as above: a real 2-1 for Ann is on screen.
      expect(screen.getByLabelText("Game wins for Ann")).toHaveValue(2);
      expect(submitButton()).toBeEnabled();

      rerender(dialogFor(annVersusCidNextRound, onSubmit));

      // Ann is a seat here too — which is precisely why a stale selection
      // would look legitimate rather than obviously wrong.
      expect(screen.getByLabelText("Ann")).not.toBeChecked();
      expect(screen.getByLabelText("Cid")).not.toBeChecked();
      expect(screen.getByLabelText("Game wins for Ann")).toHaveValue(0);
      expect(screen.getByLabelText("Game wins for Cid")).toHaveValue(0);
      expect(submitButton()).toBeDisabled();
      fireEvent.click(submitButton());
      // Pre-fix this emitted `{Decisive:{winner:"ann", game_wins:{ann:2,
      // cid:0}}}` — a legal completed Bo3 the broker ACCEPTS, recording a
      // result for round 2 that the organizer entered for round 1.
      expect(onSubmit).not.toHaveBeenCalled();

      // Paired positive: the organizer's own entry for THIS pairing submits
      // exactly as entered, with no residue of the previous one.
      fireEvent.click(screen.getByLabelText("Cid"));
      fireEvent.change(screen.getByLabelText("Game wins for Cid"), {
        target: { value: "2" },
      });
      fireEvent.click(submitButton());
      expect(onSubmit).toHaveBeenCalledWith({
        Decisive: { winner: "cid", game_wins: { ann: 0, cid: 2 } },
      });
    });
  });
});
