import { render, screen, within, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type {
  PairingOutcome,
  PlayerSummary,
  TournamentPairingView,
} from "../../../adapter/types";
import { PairingsList } from "../PairingsList";
import { expectCatalogValuePresent, expectNoRawKeyPaths } from "./tournamentTestUtils";

// This repo's vitest config does not enable `globals`, so RTL never registers
// its own auto-cleanup. Without this, every render in this file leaks into the
// next test's DOM and row-indexed queries silently address the wrong render.
afterEach(cleanup);

const NAMES = ["Ann", "Bob", "Cid", "Dee"] as const;

function seatsOf(count: number): PlayerSummary[] {
  return Array.from({ length: count }, (_, index) => ({
    player_key: `p${index}`,
    display_name: NAMES[index],
    dropped: false,
  }));
}

function rows(): HTMLElement[] {
  return screen.getAllByRole("listitem");
}

function reportActions(row: HTMLElement): HTMLElement[] {
  return within(row).queryAllByRole("button", { name: "Report Result" });
}

/**
 * A game-wins line is exactly `"<seat name> <count>"`
 * (`outcome.gameWins` = `"{{name}} {{wins}}"`). Anchoring on the seat names
 * keeps this from matching `"Table 3"`, which has the same shape.
 */
function gameWinsLines(row: HTMLElement, names: readonly string[]): string[] {
  const pattern = new RegExp(`^(?:${names.join("|")}) \\d+$`);
  return within(row)
    .queryAllByText(pattern)
    .map((node) => node.textContent ?? "");
}

// V16 — one code path for every arity. A bye (1 seat), head-to-head (2), a
// short pod (3) and a full Commander pod (4), each crossed with all four
// outcome shapes. Any `if (seats === 2)` branch reds one of these 16 cases.
describe("PairingsList arity polymorphism", () => {
  const outcomes: ReadonlyArray<readonly [string, PairingOutcome, string]> = [
    ["bye", "Bye", "Bye"],
    ["forfeit", { Forfeit: { winner: "p0" } }, "Forfeit — Ann"],
    ["draw", { Reported: "Draw" }, "Draw"],
    [
      "decisive",
      { Reported: { Decisive: { winner: "p0", game_wins: {} } } },
      "Ann won",
    ],
  ];
  const cases = [1, 2, 3, 4].flatMap((seats) =>
    outcomes.map(
      ([label, outcome, copy]) => [seats, label, outcome, copy] as const,
    ),
  );

  it.each(cases)("renders %i seat(s) with a %s outcome", (seats, _label, outcome, copy) => {
    const players = seatsOf(seats);
    render(
      <PairingsList pairings={[{ id: 1, round: 1, players, outcome }]} />,
    );

    const row = rows()[0];
    for (const seat of players) {
      expect(within(row).getByText(seat.display_name)).toBeInTheDocument();
    }
    expect(within(row).getByText(copy)).toBeInTheDocument();
    expect(within(row).getByText("Table 1")).toBeInTheDocument();
  });
});

describe("PairingsList", () => {
  // V17 — a pending pairing is "Pending", not a bye. The paired positive is
  // the resolved row in the same test, so "always pending" cannot pass.
  it("renders a pending pairing distinctly from a bye", () => {
    render(
      <PairingsList
        pairings={[
          { id: 1, round: 1, players: seatsOf(2), outcome: null },
          { id: 2, round: 1, players: seatsOf(1), outcome: "Bye" },
        ]}
      />,
    );

    expect(within(rows()[0]).getByText("Pending")).toBeInTheDocument();
    expect(within(rows()[0]).queryByText("Bye")).not.toBeInTheDocument();
    expect(within(rows()[1]).getByText("Bye")).toBeInTheDocument();
    expect(within(rows()[1]).queryByText("Pending")).not.toBeInTheDocument();
  });

  it("groups pairings under their round heading in wire order", () => {
    render(
      <PairingsList
        pairings={[
          { id: 1, round: 1, players: seatsOf(2), outcome: "Bye" },
          { id: 2, round: 2, players: seatsOf(2), outcome: null },
        ]}
      />,
    );

    expect(screen.getByText("Round 1")).toBeInTheDocument();
    expect(screen.getByText("Round 2")).toBeInTheDocument();
  });

  it("renders the empty-state copy with no pairings", () => {
    const { container } = render(<PairingsList pairings={[]} />);

    expect(screen.getByText("No pairings yet.")).toBeInTheDocument();
    expectNoRawKeyPaths(container);
  });

  // V26 — every user-visible string routes through `t()`.
  it("routes all copy through the tournament catalog", () => {
    const { container } = render(
      <PairingsList
        pairings={[{ id: 1, round: 1, players: seatsOf(2), outcome: null }]}
        onReport={vi.fn()}
      />,
    );

    expectNoRawKeyPaths(container);
    expectCatalogValuePresent(container, "Pending");
  });
});

// V30 — the report affordance is arm-gated, not merely prop-gated. Four
// pairings in ONE render: the two arms `report_result` refuses outright
// (`tournament.rs:1741-1753`) and the two it accepts.
describe("PairingsList report affordance", () => {
  const byePairing: TournamentPairingView = {
    id: 1,
    round: 1,
    players: [{ player_key: "ann", display_name: "Ann", dropped: false }],
    outcome: "Bye",
  };
  const forfeitPairing: TournamentPairingView = {
    id: 2,
    round: 1,
    players: [
      { player_key: "bob", display_name: "Bob", dropped: false },
      { player_key: "cid", display_name: "Cid", dropped: true },
    ],
    outcome: { Forfeit: { winner: "bob" } },
  };
  const pendingPairing: TournamentPairingView = {
    id: 3,
    round: 1,
    players: [
      { player_key: "dee", display_name: "Dee", dropped: false },
      { player_key: "eve", display_name: "Eve", dropped: false },
    ],
    outcome: null,
  };
  const reportedPairing: TournamentPairingView = {
    id: 4,
    round: 1,
    players: [
      { player_key: "fay", display_name: "Fay", dropped: false },
      { player_key: "gus", display_name: "Gus", dropped: false },
    ],
    outcome: { Reported: { Decisive: { winner: "fay", game_wins: { fay: 2, gus: 1 } } } },
  };
  const fixture = [byePairing, forfeitPairing, pendingPairing, reportedPairing];

  it("offers the action only on the arms the broker will accept", () => {
    render(<PairingsList pairings={fixture} onReport={vi.fn()} />);

    const [bye, forfeit, pending, reported] = rows();
    // Hostile half — `report_result` refuses these two before any validation.
    expect(reportActions(bye)).toHaveLength(0);
    expect(reportActions(forfeit)).toHaveLength(0);
    // Paired positive reach-guard, in the SAME render. The already-reported
    // row is the one that forecloses a resolution-based ("unresolved only")
    // guard: re-reporting is legal (`tournament.rs:1752`).
    expect(reportActions(pending)).toHaveLength(1);
    expect(reportActions(reported)).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Report Result" })).toHaveLength(2);
  });

  it("reports the exact pairing that was clicked", async () => {
    const user = userEvent.setup();
    const onReport = vi.fn();
    render(<PairingsList pairings={fixture} onReport={onReport} />);

    await user.click(reportActions(rows()[3])[0]);
    expect(onReport).toHaveBeenCalledWith(reportedPairing);

    await user.click(reportActions(rows()[2])[0]);
    expect(onReport).toHaveBeenLastCalledWith(pendingPairing);
  });

  // Third control — the prop is still required, so the arm guard alone cannot
  // be what makes the action appear.
  it("offers no action anywhere when no report callback is supplied", () => {
    render(<PairingsList pairings={fixture} />);

    expect(
      screen.queryAllByRole("button", { name: "Report Result" }),
    ).toHaveLength(0);
  });
});

// The >= 44pt touch-target rule from `.coderabbit.yaml`'s `client/src/**` path
// instructions. `min-h-[44px]` is this repo's spelling of it
// (`components/lobby/LobbyView.tsx:332,348`, `components/lobby/HostSetup.tsx:664`),
// and a class assertion is how it is already pinned elsewhere
// (`components/draft/__tests__/LimitedDeckBuilder.test.tsx:927`) — happy-dom runs
// no layout, so every box reports a height of 0 and a measured assertion would
// pass against anything.
describe("PairingsList touch targets", () => {
  it("gives every interactive control at least a 44px hit area", () => {
    render(
      <PairingsList
        pairings={[
          { id: 1, round: 1, players: seatsOf(2), outcome: null },
          { id: 2, round: 2, players: seatsOf(2), outcome: { Reported: "Draw" } },
        ]}
        onReport={vi.fn()}
      />,
    );

    // The whole render, not just the report button: a control added later is
    // caught by the same sweep. Two reportable arms, so two controls — the
    // reach-guard, since an empty list would satisfy the loop trivially.
    const controls = screen.getAllByRole("button");
    expect(controls).toHaveLength(2);
    for (const control of controls) {
      expect(control.className).toContain("min-h-[44px]");
    }
  });
});

// V32 — game-wins lines render only on the `Decisive` arm, and only through
// `decisiveGameWins`. Five pairings in ONE render, with NO arity check
// anywhere in the component.
describe("PairingsList game-wins lines", () => {
  const podSeats: PlayerSummary[] = [
    { player_key: "cid", display_name: "Cid", dropped: false },
    { player_key: "dee", display_name: "Dee", dropped: false },
    { player_key: "eve", display_name: "Eve", dropped: false },
  ];
  const duoSeats: PlayerSummary[] = [
    { player_key: "ann", display_name: "Ann", dropped: false },
    { player_key: "bob", display_name: "Bob", dropped: false },
  ];
  const fixture: TournamentPairingView[] = [
    { id: 1, round: 1, players: [duoSeats[0]], outcome: "Bye" },
    {
      id: 2,
      round: 1,
      players: podSeats,
      outcome: { Forfeit: { winner: "cid" } },
    },
    { id: 3, round: 1, players: podSeats, outcome: { Reported: "Draw" } },
    {
      id: 4,
      round: 1,
      players: podSeats,
      // A pod's decisive result: single-game per MSTR, so the tally is
      // legitimately empty. This row is `Decisive` AND renders nothing.
      outcome: { Reported: { Decisive: { winner: "cid", game_wins: {} } } },
    },
    {
      id: 5,
      round: 1,
      players: duoSeats,
      outcome: {
        Reported: { Decisive: { winner: "ann", game_wins: { ann: 2, bob: 1 } } },
      },
    },
  ];
  const allNames = ["Ann", "Bob", "Cid", "Dee", "Eve"];

  it("renders no game-wins line for any arm without a tally", () => {
    render(<PairingsList pairings={fixture} />);

    const [bye, forfeit, draw, pod] = rows();
    expect(gameWinsLines(bye, allNames)).toEqual([]);
    expect(gameWinsLines(forfeit, allNames)).toEqual([]);
    expect(gameWinsLines(draw, allNames)).toEqual([]);
    expect(gameWinsLines(pod, allNames)).toEqual([]);
  });

  it("renders one line per seat, in seat order, for a head-to-head decisive result", () => {
    render(<PairingsList pairings={fixture} />);

    // Paired positive reach-guard in the SAME render — "never renders game
    // wins" cannot pass vacuously.
    expect(gameWinsLines(rows()[4], allNames)).toEqual(["Ann 2", "Bob 1"]);
  });

  it("renders game-wins lines in seat order even for all-digit player keys", () => {
    const digitSeats: PlayerSummary[] = [
      { player_key: "12", display_name: "Twelve", dropped: false },
      { player_key: "7", display_name: "Seven", dropped: false },
    ];
    render(
      <PairingsList
        pairings={[
          {
            id: 9,
            round: 1,
            players: digitSeats,
            outcome: {
              Reported: { Decisive: { winner: "12", game_wins: { "12": 2, "7": 0 } } },
            },
          },
        ]}
      />,
    );

    expect(gameWinsLines(rows()[0], ["Twelve", "Seven"])).toEqual([
      "Twelve 2",
      "Seven 0",
    ]);
  });
});
