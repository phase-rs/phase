import { render, screen, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { TournamentSummary, TournamentView } from "../../../adapter/types";
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
