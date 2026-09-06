import { render, screen, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import i18n from "i18next";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  BracketShape,
  TournamentStatus,
  TournamentSummary,
  TournamentView,
} from "../../../adapter/types";
import { TournamentListItem } from "../TournamentListItem";
import { expectCatalogValuePresent, expectNoRawKeyPaths } from "./tournamentTestUtils";

// This repo's vitest config does not enable `globals`, so RTL never registers
// its own auto-cleanup. Without this, every render in this file leaks into the
// next test's DOM and row-indexed queries silently address the wrong render.
afterEach(cleanup);

/**
 * The probed hostile payload: one **active** entrant while the full detail
 * view holds two players, because one of them dropped.
 */
const summary: TournamentSummary = {
  code: "ABCD1",
  name: "Friday Night Magic",
  arity: 4,
  bracket: "Swiss",
  status: "InProgress",
  player_count: 1,
  current_round: 2,
  total_rounds: 3,
  created_at: 1_700_000_000,
};

/**
 * Every `TournamentStatus` member and the English copy it must resolve to.
 *
 * Declared as a **`Record` over the union**, not an array of tags: adding a
 * member to `TournamentStatus` is then a compile error here until this table
 * carries it, which is the closest this path gets to the compile-time gate
 * `outcomeLabelKey`'s exhaustive walk enjoys. The component interpolates the
 * wire tag straight into the key (`status.${summary.status}`), so nothing else
 * would notice a tag rename or a new member — it would degrade silently to a
 * rendered raw key path.
 */
const STATUS_LABELS: Record<TournamentStatus, string> = {
  Registration: "Registration",
  InProgress: "In Progress",
  Completed: "Completed",
  Abandoned: "Abandoned",
};

/** The same table for `BracketShape`, whose key is built the same way. */
const BRACKET_LABELS: Record<BracketShape, string> = {
  Swiss: "Swiss",
  SingleElimination: "Single Elimination",
};

const detailView: TournamentView = {
  summary,
  players: [
    { player_key: "a", display_name: "Ann", dropped: false },
    { player_key: "b", display_name: "Bob", dropped: true },
  ],
  pairings: [],
  standings: [],
};

describe("TournamentListItem", () => {
  // V18 — the entrant count is `summary.player_count` (active), never
  // `view.players.length` (the full history including drops). The prop type
  // makes the wrong field structurally unavailable; this pins the runtime half.
  it("labels the active entrant count, not the total player history", () => {
    render(<TournamentListItem summary={summary} onOpen={vi.fn()} />);

    expect(detailView.players.length).toBe(2); // the count that must NOT render
    expect(screen.getByText("1 entrant")).toBeInTheDocument();
    expect(screen.queryByText("2 entrants")).not.toBeInTheDocument();
  });

  it("renders the wire-indexed status and bracket copy and the arity label", () => {
    render(<TournamentListItem summary={summary} onOpen={vi.fn()} />);

    expect(screen.getByText("In Progress")).toBeInTheDocument();
    expect(screen.getByText("Swiss")).toBeInTheDocument();
    expect(screen.getByText("4-player pods")).toBeInTheDocument();
    expect(screen.getByText("Round 2 of 3")).toBeInTheDocument();
    expect(screen.getByText("Code ABCD1")).toBeInTheDocument();
  });

  // V34 — the class-coverage claim for the two template-interpolated key
  // groups. Both halves matter: `i18n.exists` proves the key the component
  // will build is really in the catalog, and the render proves the component
  // builds *that* key rather than a differently-shaped one.
  it.each(Object.entries(STATUS_LABELS))(
    "resolves the %s status badge to catalog copy",
    (status, label) => {
      expect(i18n.exists(`tournament:status.${status}`), status).toBe(true);

      const { container } = render(
        <TournamentListItem
          summary={{ ...summary, status: status as TournamentStatus }}
          onOpen={vi.fn()}
        />,
      );

      expect(screen.getByText(label)).toBeInTheDocument();
      expectNoRawKeyPaths(container);
    },
  );

  it.each(Object.entries(BRACKET_LABELS))(
    "resolves the %s bracket badge to catalog copy",
    (bracket, label) => {
      expect(i18n.exists(`tournament:bracket.${bracket}`), bracket).toBe(true);

      // Arity 2 for both rows: `SingleElimination` is only legal at
      // head-to-head (`tournament.rs:1514-1523`), so this keeps the fixture a
      // shape the broker could actually have produced.
      const { container } = render(
        <TournamentListItem
          summary={{ ...summary, bracket: bracket as BracketShape, arity: 2 }}
          onOpen={vi.fn()}
        />,
      );

      expect(screen.getByText(label)).toBeInTheDocument();
      expectNoRawKeyPaths(container);
    },
  );

  // The casing trap the page-state module exists to prevent, pinned on this
  // path too: these groups are keyed by the PascalCase wire tag, so the
  // lowercase spelling used by `outcome.*` must NOT resolve here.
  it("keys the status and bracket groups by the PascalCase wire tag", () => {
    expect(i18n.exists("tournament:status.inprogress")).toBe(false);
    expect(i18n.exists("tournament:bracket.swiss")).toBe(false);
  });

  // LOW 3 — `current_round` is 0 from creation until round 1 starts, which
  // otherwise reads "Round 0 of 3" next to the "Registration" badge.
  it("omits the round counter until a round has started", () => {
    const { rerender } = render(
      <TournamentListItem
        summary={{ ...summary, status: "Registration", current_round: 0 }}
        onOpen={vi.fn()}
      />,
    );

    expect(screen.queryByText(/^Round\b/)).not.toBeInTheDocument();
    // Reach-guard: the row really rendered, so the negative is not vacuous.
    expect(screen.getByText("Registration")).toBeInTheDocument();

    // Paired positive: the suppression is conditional, not a deletion.
    rerender(<TournamentListItem summary={summary} onOpen={vi.fn()} />);
    expect(screen.getByText("Round 2 of 3")).toBeInTheDocument();
  });

  it("labels a head-to-head tournament without a seat count", () => {
    render(
      <TournamentListItem summary={{ ...summary, arity: 2 }} onOpen={vi.fn()} />,
    );

    expect(screen.getByText("Head-to-head")).toBeInTheDocument();
    expect(screen.queryByText("2-player pods")).not.toBeInTheDocument();
  });

  it("reports the tournament code to the open callback", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();

    render(<TournamentListItem summary={summary} onOpen={onOpen} />);
    await user.click(screen.getByRole("button"));

    expect(onOpen).toHaveBeenCalledWith("ABCD1");
  });

  // V26 — every user-visible string routes through `t()`.
  it("routes all copy through the tournament catalog", () => {
    const { container } = render(
      <TournamentListItem summary={summary} onOpen={vi.fn()} />,
    );

    expectNoRawKeyPaths(container);
    expectCatalogValuePresent(container, "View");
  });
});
