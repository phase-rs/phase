import { fireEvent, render, screen, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CreateTournamentForm } from "../CreateTournamentForm";
import { expectCatalogValuePresent, expectNoRawKeyPaths } from "./tournamentTestUtils";

// This repo's vitest config does not enable `globals`, so RTL never registers
// its own auto-cleanup. Without this, every render in this file leaks into the
// next test's DOM and row-indexed queries silently address the wrong render.
afterEach(cleanup);

function submitButton() {
  return screen.getByRole("button", { name: "Create Tournament" });
}

describe("CreateTournamentForm", () => {
  // V19 — the broker refuses `SingleElimination` with an arity other than 2
  // (`crates/lobby-broker/src/tournament.rs:1514-1523`). The form must not
  // duplicate that rule. The positive reach-guard is charter-mandated: the
  // callback must fire WITH the illegal combination, so a form that never
  // submitted anything cannot satisfy this.
  it("submits an illegal bracket/arity combination without pre-rejecting it", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    fireEvent.change(screen.getByLabelText("Bracket"), {
      target: { value: "SingleElimination" },
    });
    fireEvent.change(screen.getByLabelText("Players per match"), {
      target: { value: "4" },
    });
    fireEvent.click(submitButton());

    expect(onSubmit).toHaveBeenCalledTimes(1);
    expect(onSubmit.mock.calls[0][0]).toMatchObject({
      bracket: "SingleElimination",
      arity: 4,
    });
  });

  // V20a — untouched scoring follows the arity. `default_for_arity` is
  // `2n-1`, so arity 4 must prefill 7, not stay at 3.
  it("re-prefills untouched scoring when the arity changes", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    expect(screen.getByLabelText("Win")).toHaveValue(3);

    fireEvent.change(screen.getByLabelText("Players per match"), {
      target: { value: "4" },
    });

    expect(screen.getByLabelText("Win")).toHaveValue(7);

    fireEvent.click(submitButton());
    expect(onSubmit.mock.calls[0][0].scoring).toEqual({
      win_points: 7,
      draw_points: 1,
      loss_points: 0,
    });
  });

  // V20b — the paired opposite of V20a. Without both, one implementation
  // satisfies the other vacuously.
  it("keeps an organizer's edited scoring across an arity change", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    fireEvent.change(screen.getByLabelText("Win"), { target: { value: "4" } });
    fireEvent.change(screen.getByLabelText("Players per match"), {
      target: { value: "4" },
    });

    expect(screen.getByLabelText("Win")).toHaveValue(4);

    fireEvent.click(submitButton());
    expect(onSubmit.mock.calls[0][0].scoring).toEqual({
      win_points: 4,
      draw_points: 1,
      loss_points: 0,
    });
  });

  it("prefills from the initial arity", () => {
    render(<CreateTournamentForm onSubmit={vi.fn()} initialArity={4} />);
    expect(screen.getByLabelText("Win")).toHaveValue(7);
  });

  // V21 — "Automatic" is the wire's `total_rounds: null`, the one
  // `CreateTournament` field that is `#[serde(default)]`.
  it("submits null rounds when the organizer leaves the field automatic", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    fireEvent.click(submitButton());

    expect(onSubmit.mock.calls[0][0].totalRounds).toBeNull();
  });

  it("submits an explicit round count when one is entered", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    fireEvent.change(screen.getByLabelText("Rounds"), { target: { value: "5" } });
    fireEvent.click(submitButton());

    expect(onSubmit.mock.calls[0][0].totalRounds).toBe(5);
  });

  it("submits the name exactly as typed", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    fireEvent.change(screen.getByLabelText("Tournament name"), {
      target: { value: "Friday Night Magic" },
    });
    fireEvent.click(submitButton());

    expect(onSubmit.mock.calls[0][0].name).toBe("Friday Night Magic");
  });

  it("shows the busy label while a submission is in flight", () => {
    render(<CreateTournamentForm onSubmit={vi.fn()} submitting />);
    expect(screen.getByRole("button", { name: "Creating…" })).toBeDisabled();
  });

  // V26 — every user-visible string routes through `t()`.
  it("routes all copy through the tournament catalog", () => {
    const { container } = render(<CreateTournamentForm onSubmit={vi.fn()} />);

    expectNoRawKeyPaths(container);
    expectCatalogValuePresent(container, "Create Tournament");
  });
});
