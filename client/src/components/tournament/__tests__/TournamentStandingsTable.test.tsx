import { render, screen, within, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { Tiebreaks, TournamentStanding } from "../../../adapter/types";
import { TournamentStandingsTable } from "../TournamentStandingsTable";
import { expectCatalogValuePresent, expectNoRawKeyPaths } from "./tournamentTestUtils";

// This repo's vitest config does not enable `globals`, so RTL never registers
// its own auto-cleanup. Without this, every render in this file leaks into the
// next test's DOM and row-indexed queries silently address the wrong render.
afterEach(cleanup);

function headToHead(omw: number, gw: number, ogw: number): Tiebreaks {
  return {
    HeadToHead: {
      opponents_match_win_pct: omw,
      game_win_pct: gw,
      opponents_game_win_pct: ogw,
    },
  };
}

function multiplayer(mw: number, oamp: number, omw: number): Tiebreaks {
  return {
    Multiplayer: {
      match_win_pct: mw,
      opponents_avg_match_points: oamp,
      opponents_match_win_pct: omw,
    },
  };
}

/**
 * `match_points` are deliberately non-monotonic in array order (3, 9, 6). Any
 * client-side re-sort reorders these rows, and the server's own ranking is
 * what the array order already encodes.
 */
const standings: readonly TournamentStanding[] = [
  {
    player_key: "a",
    display_name: "Ann",
    dropped: false,
    match_points: 3,
    matches_played: 2,
    byes: 0,
    // A zero tiebreak: a player whose opponents have not won yet. Legitimate
    // value, not an absence.
    tiebreaks: headToHead(0, 0.5, 0.25),
  },
  {
    player_key: "b",
    display_name: "Bob",
    dropped: false,
    match_points: 9,
    matches_played: 3,
    byes: 0,
    tiebreaks: headToHead(0.6, 0.75, 0.5),
  },
  {
    player_key: "c",
    display_name: "Cid",
    dropped: true,
    match_points: 6,
    matches_played: 3,
    byes: 1,
    tiebreaks: headToHead(0.4, 0.5, 0.6),
  },
];

function bodyRows(): HTMLElement[] {
  return screen.getAllByRole("row").slice(1);
}

function tiebreakTexts(row: HTMLElement): (string | null)[] {
  return within(row)
    .getAllByRole("cell")
    .slice(5)
    .map((cell) => cell.textContent);
}

describe("TournamentStandingsTable", () => {
  // V14 — rows render in the server's order and are ranked by position, never
  // re-sorted or re-ranked. Copying `components/draft/StandingsTable.tsx`'s
  // `[...standings].sort(...)` reds this.
  it("renders rows in server order with positional ranks", () => {
    render(<TournamentStandingsTable standings={standings} />);

    const rows = bodyRows();
    expect(rows.map((row) => within(row).getAllByRole("cell")[0].textContent)).toEqual([
      "1",
      "2",
      "3",
    ]);
    expect(
      rows.map((row) => within(row).getAllByRole("cell")[1].textContent),
    ).toEqual(["Ann", "Bob", "CidDropped"]);
    expect(rows.map((row) => within(row).getAllByRole("cell")[2].textContent)).toEqual([
      "3",
      "9",
      "6",
    ]);
  });

  // V15 — a dropped entrant stays listed AND marked. Presence alone would
  // pass with the marker missing, so both halves are asserted.
  it("keeps a dropped entrant listed and marks them", () => {
    render(<TournamentStandingsTable standings={standings} />);

    expect(screen.getByText("Cid")).toBeInTheDocument();
    const droppedRow = bodyRows()[2];
    expect(within(droppedRow).getByText("Dropped")).toBeInTheDocument();
    // The other rows carry no marker — so "always marked" cannot pass.
    expect(within(bodyRows()[0]).queryByText("Dropped")).not.toBeInTheDocument();
  });

  // V6's component half — a zero is a value, not an absence.
  it("renders a zero tiebreak as a number", () => {
    render(<TournamentStandingsTable standings={standings} />);

    expect(tiebreakTexts(bodyRows()[0])).toEqual(["0.0%", "50.0%", "25.0%"]);
    // Paired sibling: a non-zero row formats identically, so the zero case is
    // visibly the discriminator.
    expect(tiebreakTexts(bodyRows()[1])).toEqual(["60.0%", "75.0%", "50.0%"]);
  });

  // V5 — cell ids are scheme-qualified, so a row carrying the other
  // `Tiebreaks` arm renders explicit placeholders rather than three
  // plausible-looking numbers under the wrong headers.
  it("renders placeholders for a row whose tiebreak arm differs from the header's", () => {
    const mixed: readonly TournamentStanding[] = [
      standings[0],
      { ...standings[1], tiebreaks: multiplayer(0.9, 5.5, 0.45) },
    ];
    render(<TournamentStandingsTable standings={mixed} />);

    // Positive control first: the header row's own arm still renders real
    // numbers, so "renders — everywhere" cannot pass.
    expect(tiebreakTexts(bodyRows()[0])).toEqual(["0.0%", "50.0%", "25.0%"]);
    expect(tiebreakTexts(bodyRows()[1])).toEqual(["—", "—", "—"]);
    // And the foreign arm's values leak nowhere.
    expect(screen.queryByText("90.0%")).not.toBeInTheDocument();
    expect(screen.queryByText("5.50")).not.toBeInTheDocument();
  });

  it("renders the multiplayer arm's own headers when it is the first row", () => {
    render(
      <TournamentStandingsTable
        standings={[{ ...standings[0], tiebreaks: multiplayer(0.9, 5.5, 0.45) }]}
      />,
    );

    expect(screen.getByText("MW%")).toBeInTheDocument();
    expect(screen.getByText("OAMP")).toBeInTheDocument();
    expect(tiebreakTexts(bodyRows()[0])).toEqual(["90.0%", "5.50", "45.0%"]);
  });

  it("renders the empty-state copy with no standings", () => {
    const { container } = render(<TournamentStandingsTable standings={[]} />);

    expect(screen.getByText("No standings yet.")).toBeInTheDocument();
    expectNoRawKeyPaths(container);
  });

  // V26 — every user-visible string routes through `t()`.
  it("routes all copy through the tournament catalog", () => {
    const { container } = render(<TournamentStandingsTable standings={standings} />);

    expectNoRawKeyPaths(container);
    expectCatalogValuePresent(container, "Player");
  });
});
