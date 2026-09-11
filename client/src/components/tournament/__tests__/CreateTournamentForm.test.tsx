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

  // V20a — scoring defaults to "Automatic": the form submits `scoring: null`
  // and the broker applies its own `default_for_arity` (lobby protocol v6).
  // The form computes no default itself — that duplicate is exactly what the
  // resolved `TournamentSummary.scoring` field exists to delete.
  it("submits null scoring when left automatic", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    fireEvent.click(submitButton());

    expect(onSubmit.mock.calls[0][0].scoring).toBeNull();
  });

  // The scoring inputs are disabled while Automatic is on — the form never
  // presents a client-computed default for the organizer to accept.
  it("disables the scoring inputs while automatic", () => {
    render(<CreateTournamentForm onSubmit={vi.fn()} />);

    expect(screen.getByLabelText("Win")).toBeDisabled();

    fireEvent.click(screen.getByLabelText("Automatic"));

    expect(screen.getByLabelText("Win")).toBeEnabled();
  });

  // V20b — the paired opposite of V20a. Turning Automatic off submits the
  // explicit override verbatim, and it is INDEPENDENT of arity: the form no
  // longer couples scoring to the arity at all.
  it("submits an explicit scoring override, unchanged by a later arity change", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    fireEvent.click(screen.getByLabelText("Automatic"));
    fireEvent.change(screen.getByLabelText("Win"), { target: { value: "5" } });
    fireEvent.change(screen.getByLabelText("Draw"), { target: { value: "2" } });
    fireEvent.change(screen.getByLabelText("Loss"), { target: { value: "1" } });
    fireEvent.change(screen.getByLabelText("Players per match"), {
      target: { value: "4" },
    });

    fireEvent.click(submitButton());
    expect(onSubmit.mock.calls[0][0].scoring).toEqual({
      win_points: 5,
      draw_points: 2,
      loss_points: 1,
    });
  });

  // Regression: the scoring inputs are string-backed and parsed at submit, like
  // the rounds field. The prior numeric `value` + `parsedOr(current)` inputs
  // reverted an emptied field to its previous value, so a draw of 1 could not
  // be cleared and retyped as 2 — the reported bug.
  it("lets a scoring field be cleared and retyped", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    fireEvent.click(screen.getByLabelText("Automatic"));
    const draw = screen.getByLabelText("Draw");

    fireEvent.change(draw, { target: { value: "1" } });
    fireEvent.change(draw, { target: { value: "" } });
    // Emptied, not snapped back to "1".
    expect(draw).toHaveValue(null);

    fireEvent.change(draw, { target: { value: "2" } });
    fireEvent.click(submitButton());
    expect(onSubmit.mock.calls[0][0].scoring.draw_points).toBe(2);
  });

  // Regression: a `type="number"` input accepts exponent notation, so `1e2` is a
  // valid entry that a real browser resolves to the integer 100 and submits.
  // `parseInt` would truncate it to `1` before the broker — the only scoring
  // authority — ever saw it. The complete value must reach `onSubmit` unchanged.
  //
  // Submitted via `fireEvent.submit` rather than clicking the button: jsdom runs
  // constraint validation on the click→submit path and (unlike a browser) blocks
  // a number control whose raw string is `1e2`, which would swallow the very
  // submission this asserts. Dispatching the submit event directly reproduces
  // the browser outcome for a value the browser considers valid.
  it("preserves an exponent-notation scoring value to the wire", () => {
    const onSubmit = vi.fn();
    const { container } = render(<CreateTournamentForm onSubmit={onSubmit} />);

    fireEvent.click(screen.getByLabelText("Automatic"));
    fireEvent.change(screen.getByLabelText("Win"), { target: { value: "1e2" } });

    const form = container.querySelector("form");
    expect(form).not.toBeNull();
    fireEvent.submit(form as HTMLFormElement);
    expect(onSubmit.mock.calls[0][0].scoring.win_points).toBe(100);
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

  // Protocol v7: a chosen format is submitted verbatim; "Unspecified" is null.
  it("submits the chosen game format, or null when unspecified", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    // Default is "Unspecified".
    fireEvent.click(submitButton());
    expect(onSubmit.mock.calls[0][0].format).toBeNull();

    fireEvent.change(screen.getByLabelText("Format"), {
      target: { value: "Commander" },
    });
    fireEvent.click(submitButton());
    expect(onSubmit.mock.calls[1][0].format).toBe("Commander");
  });

  // Protocol v8: head-to-head defaults to Bo3 and can be set to Bo1 (single-game
  // single-elim); a pod sends `null` so the broker resolves the arity default
  // (single-game per MSTR), and its control is disabled as a UI affordance.
  it("submits Bo3 by default at head-to-head, Bo1 when chosen, null for pods", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    // Default head-to-head → Bo3.
    fireEvent.click(submitButton());
    expect(onSubmit.mock.calls[0][0].matchType).toBe("Bo3");

    // Head-to-head, explicitly Bo1.
    fireEvent.change(screen.getByLabelText("Match"), { target: { value: "Bo1" } });
    fireEvent.click(submitButton());
    expect(onSubmit.mock.calls[1][0].matchType).toBe("Bo1");

    // A pod sends `null` regardless of the (now disabled) control, letting the
    // broker resolve the single-game default.
    fireEvent.change(screen.getByLabelText("Match"), { target: { value: "Bo3" } });
    fireEvent.change(screen.getByLabelText("Players per match"), {
      target: { value: "4" },
    });
    expect(screen.getByLabelText("Match")).toBeDisabled();
    fireEvent.click(submitButton());
    expect(onSubmit.mock.calls[2][0].matchType).toBeNull();
  });

  // "Automatic + N": extra rounds ride as `plusRounds` while the count is
  // automatic, and are dropped when an exact count is set (mutually exclusive).
  it("submits extra rounds as plusRounds only while the count is automatic", () => {
    const onSubmit = vi.fn();
    render(<CreateTournamentForm onSubmit={onSubmit} />);

    fireEvent.change(screen.getByLabelText("Extra rounds"), {
      target: { value: "2" },
    });
    fireEvent.click(submitButton());
    expect(onSubmit.mock.calls[0][0]).toMatchObject({
      totalRounds: null,
      plusRounds: 2,
    });

    // With an exact round count set, the addend is dropped, never sent both.
    fireEvent.change(screen.getByLabelText("Rounds"), { target: { value: "5" } });
    fireEvent.click(submitButton());
    expect(onSubmit.mock.calls[1][0]).toMatchObject({
      totalRounds: 5,
      plusRounds: null,
    });
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
