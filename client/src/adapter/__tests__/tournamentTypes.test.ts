import { describe, expect, it } from "vitest";

import type { PairingView as DraftPairingView } from "../draft-adapter";
import type {
  MatchArity,
  PairingId,
  PairingOutcome,
  ScoringPolicy,
  Tiebreaks,
  TournamentPairingView,
  TournamentView,
} from "../types";

/**
 * One fixture carrying every tournament wire shape at once, written the way the
 * broker actually serializes it: bare-number `arity`, flat `scoring`,
 * externally-tagged outcomes with `Reported` double-nested, an explicit
 * `"outcome": null` for a pending pairing, and both `Tiebreaks` arms.
 *
 * Typing it as `TournamentView` is what makes a mirror-shape error a *compile*
 * error here rather than a silent runtime mismatch later.
 */
const PROBED_VIEW: TournamentView = {
  summary: {
    code: "TOUR01",
    name: "Friday Night",
    arity: 4,
    bracket: "Swiss",
    status: "InProgress",
    // Active entrants, not `players.length` — this fixture holds 4 registered
    // players of whom 1 has dropped.
    player_count: 3,
    current_round: 2,
    total_rounds: 3,
    created_at: 1_700_000_000,
  },
  players: [
    { player_key: "key-a", display_name: "Alice", dropped: false },
    { player_key: "key-b", display_name: "Bob", dropped: false },
    { player_key: "key-c", display_name: "Carol", dropped: false },
    { player_key: "key-d", display_name: "Dave", dropped: true },
  ],
  pairings: [
    {
      id: 0,
      round: 1,
      players: [{ player_key: "key-a", display_name: "Alice", dropped: false }],
      outcome: "Bye",
    },
    {
      id: 1,
      round: 1,
      players: [
        { player_key: "key-b", display_name: "Bob", dropped: false },
        { player_key: "key-d", display_name: "Dave", dropped: true },
      ],
      outcome: { Forfeit: { winner: "key-b" } },
    },
    {
      id: 2,
      round: 2,
      players: [
        { player_key: "key-a", display_name: "Alice", dropped: false },
        { player_key: "key-b", display_name: "Bob", dropped: false },
      ],
      outcome: {
        Reported: { Decisive: { winner: "key-a", game_wins: { "key-a": 2, "key-b": 1 } } },
      },
    },
    {
      id: 3,
      round: 2,
      players: [
        { player_key: "key-c", display_name: "Carol", dropped: false },
        { player_key: "key-d", display_name: "Dave", dropped: true },
      ],
      outcome: { Reported: "Draw" },
    },
    {
      id: 4,
      round: 2,
      players: [
        { player_key: "key-a", display_name: "Alice", dropped: false },
        { player_key: "key-c", display_name: "Carol", dropped: false },
        { player_key: "key-b", display_name: "Bob", dropped: false },
      ],
      // Pending: emitted with no `skip_serializing_if`, so an explicit null.
      outcome: null,
    },
  ],
  standings: [
    {
      player_key: "key-a",
      display_name: "Alice",
      dropped: false,
      match_points: 7,
      matches_played: 1,
      byes: 1,
      tiebreaks: {
        HeadToHead: {
          opponents_match_win_pct: 0.5,
          game_win_pct: 0.666_666_666_666_666_6,
          opponents_game_win_pct: 0.333_333_333_333_333_3,
        },
      },
    },
    {
      player_key: "key-b",
      display_name: "Bob",
      dropped: false,
      match_points: 3,
      matches_played: 2,
      byes: 0,
      tiebreaks: {
        Multiplayer: {
          match_win_pct: 0.25,
          opponents_avg_match_points: 4.5,
          opponents_match_win_pct: 0.6,
        },
      },
    },
  ],
};

describe("tournament wire type mirrors", () => {
  it("round-trips the probed byte shapes through JSON without loss", () => {
    // Positive reach-guards at every level: an empty fixture would round-trip
    // vacuously.
    expect(PROBED_VIEW.players).toHaveLength(4);
    expect(PROBED_VIEW.pairings).toHaveLength(5);
    expect(PROBED_VIEW.standings).toHaveLength(2);
    expect(PROBED_VIEW.summary.player_count).toBe(3);
    // `player_count` is ACTIVE entrants, so it is deliberately not
    // `players.length` — one entrant has dropped.
    expect(PROBED_VIEW.summary.player_count).not.toBe(PROBED_VIEW.players.length);

    expect(JSON.parse(JSON.stringify(PROBED_VIEW))).toEqual(PROBED_VIEW);
  });

  it("keeps arity and pairing ids as bare numbers on the wire", () => {
    const arity: MatchArity = 4;
    const pairingId: PairingId = 7;

    expect(JSON.parse(JSON.stringify({ arity, pairingId }))).toEqual({
      arity: 4,
      pairingId: 7,
    });
    // `#[serde(try_from = "u8", into = "u8")]` means the newtype never reaches
    // the wire as a wrapper object.
    expect(typeof PROBED_VIEW.summary.arity).toBe("number");
    expect(typeof PROBED_VIEW.pairings[0].id).toBe("number");

    // @ts-expect-error MatchArity crosses the wire as a bare number, never a wrapper object
    const wrappedArity: MatchArity = { arity: 4 };
    expect(wrappedArity).toBeDefined();
  });

  it("keeps the scoring policy flat", () => {
    const scoring: ScoringPolicy = { win_points: 3, draw_points: 1, loss_points: 0 };
    expect(JSON.parse(JSON.stringify(scoring))).toEqual({
      win_points: 3,
      draw_points: 1,
      loss_points: 0,
    });

    // Written on one line deliberately: `@ts-expect-error` suppresses only the
    // line immediately after it, and the shape error lands on the inner
    // property, not on the `const`.
    // @ts-expect-error the try_from/into boundary flattens RawScoringPolicy away
    const nestedScoring: ScoringPolicy = { RawScoringPolicy: { win_points: 3, draw_points: 1, loss_points: 0 } };
    expect(nestedScoring).toBeDefined();
  });

  it("rejects a flattened PairingOutcome at compile time", () => {
    // Four well-formed positives: every outcome shape the broker can emit.
    const bye: PairingOutcome = "Bye";
    const forfeit: PairingOutcome = { Forfeit: { winner: "key-b" } };
    const decisive: PairingOutcome = {
      Reported: { Decisive: { winner: "key-a", game_wins: { "key-a": 2, "key-b": 1 } } },
    };
    const draw: PairingOutcome = { Reported: "Draw" };

    expect(bye).toBe("Bye");
    expect(forfeit).toBeDefined();
    expect(decisive).toBeDefined();
    expect(draw).toBeDefined();

    // THE discriminating property of this whole mirror: `Reported` is a newtype
    // wrapping `PodOutcome`, so a decisive result nests twice. Flattening it
    // must not compile — this assertion only surfaces under `tsc`, never under
    // vitest alone.
    // @ts-expect-error Reported wraps a PodOutcome; a flattened decisive result is not one
    const flattened: PairingOutcome = { Reported: { winner: "key-a", game_wins: {} } };

    // Sibling negatives, so the directive above cannot be the only thing holding.
    // @ts-expect-error Decisive requires game_wins even when it is empty for a pod
    const missingGameWins: PairingOutcome = { Reported: { Decisive: { winner: "key-a" } } };
    // @ts-expect-error Forfeit is a struct variant, not a bare winner string
    const stringForfeit: PairingOutcome = { Forfeit: "key-b" };
    // @ts-expect-error the unit variant is spelled "Draw", not "Drawn"
    const nearMissDraw: PairingOutcome = { Reported: "Drawn" };

    expect(flattened).toBeDefined();
    expect(missingGameWins).toBeDefined();
    expect(stringForfeit).toBeDefined();
    expect(nearMissDraw).toBeDefined();
  });

  it("represents both Tiebreaks arms and refuses cross-arm field bleed", () => {
    const headToHead: Tiebreaks = {
      HeadToHead: {
        opponents_match_win_pct: 0.5,
        game_win_pct: 0.6,
        opponents_game_win_pct: 0.4,
      },
    };
    const multiplayer: Tiebreaks = {
      Multiplayer: {
        match_win_pct: 0.25,
        opponents_avg_match_points: 4.5,
        opponents_match_win_pct: 0.6,
      },
    };

    expect(JSON.parse(JSON.stringify([headToHead, multiplayer]))).toEqual([
      headToHead,
      multiplayer,
    ]);
    // The arm name selects a different field set, so it is the only safe
    // discriminator a renderer may branch on.
    expect("HeadToHead" in PROBED_VIEW.standings[0].tiebreaks).toBe(true);
    expect("Multiplayer" in PROBED_VIEW.standings[1].tiebreaks).toBe(true);

    // One line, for the same directive-scoping reason as the ScoringPolicy case
    // above: the error lands on the bled-in property, not on the `const`.
    // @ts-expect-error head-to-head axes cannot appear under the Multiplayer arm
    const bleed: Tiebreaks = { Multiplayer: { opponents_match_win_pct: 0.6, game_win_pct: 0.5, opponents_game_win_pct: 0.4 } };
    expect(bleed).toBeDefined();
  });

  it("keeps TournamentPairingView distinct from the draft pod's PairingView", () => {
    // S2: `adapter/draft-adapter.ts` already exports an incompatible
    // `PairingView` mirroring `crates/draft-core`. The two names must never be
    // conflated — hence the tournament mirror's distinct name.
    const tournamentPairing: TournamentPairingView = {
      id: 1,
      round: 1,
      players: [{ player_key: "key-a", display_name: "Alice", dropped: false }],
      outcome: "Bye",
    };
    const draftPairing: DraftPairingView = {
      round: 1,
      table: 1,
      seat_a: 0,
      name_a: "Alice",
      seat_b: 1,
      name_b: "Bob",
      match_id: "m-1",
      status: "Pending",
      winner_seat: null,
      score_a: null,
      score_b: null,
    };

    expect(tournamentPairing.players).toHaveLength(1);
    expect(draftPairing.seat_b).toBe(1);

    // @ts-expect-error the draft pod's PairingView is an unrelated, incompatible shape
    const asDraft: DraftPairingView = tournamentPairing;
    // @ts-expect-error and the tournament mirror cannot accept a draft pairing either
    const asTournament: TournamentPairingView = draftPairing;

    expect(asDraft).toBeDefined();
    expect(asTournament).toBeDefined();
  });
});
