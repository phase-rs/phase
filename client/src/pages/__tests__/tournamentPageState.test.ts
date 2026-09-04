import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import i18n from "i18next";
import { describe, expect, it } from "vitest";

import type {
  PairingOutcome,
  PlayerSummary,
  TournamentView,
} from "../../adapter/types";
import type { TournamentCredential } from "../../stores/multiplayerStore";
import {
  arityLabel,
  decisiveGameWins,
  defaultScoringForArity,
  failureLabel,
  formatTiebreakValue,
  gameWinsEntries,
  isActiveEntrant,
  isReportable,
  myPairing,
  outcomeLabelKey,
  tiebreakCells,
  viewerRelation,
  viewerRoles,
  type ViewerRelation,
} from "../tournamentPageState";

/**
 * The failure shape `failureLabel` accepts, read off the function itself
 * rather than re-spelled: `TournamentFailure` is module-private, and a
 * hand-written copy here would be a second declaration free to drift from the
 * store's vocabulary.
 */
type TournamentFailureInput = Parameters<typeof failureLabel>[0];

const seats: readonly PlayerSummary[] = [
  { player_key: "a", display_name: "Ann", dropped: false },
  { player_key: "b", display_name: "Bob", dropped: false },
];

describe("outcomeLabelKey", () => {
  // V1 — all four outcome shapes, asserting the exact object rather than
  // truthiness, so a swapped key or a dropped `winner` var goes red.
  it("maps every outcome shape to its key and vars", () => {
    expect(outcomeLabelKey("Bye", seats)).toEqual({ key: "outcome.bye" });
    expect(outcomeLabelKey({ Forfeit: { winner: "a" } }, seats)).toEqual({
      key: "outcome.forfeit",
      winner: "Ann",
    });
    expect(outcomeLabelKey({ Reported: "Draw" }, seats)).toEqual({
      key: "outcome.draw",
    });
    expect(
      outcomeLabelKey(
        { Reported: { Decisive: { winner: "b", game_wins: { a: 1, b: 2 } } } },
        seats,
      ),
    ).toEqual({ key: "outcome.decisive", winner: "Bob" });
  });

  // V1 — every key it can produce actually exists in the catalog. A key that
  // "looks right" but is not in `en/tournament.json` fails here.
  it("only produces keys the tournament catalog carries", () => {
    for (const key of [
      "outcome.bye",
      "outcome.draw",
      "outcome.forfeit",
      "outcome.decisive",
    ]) {
      expect(i18n.exists(`tournament:${key}`), key).toBe(true);
    }
    // The casing trap this module exists to prevent: the wire tags are
    // PascalCase and the catalog keys are not, so a key built from a tag
    // resolves to nothing.
    expect(i18n.exists("tournament:outcome.Bye")).toBe(false);
  });

  // V3 — an unresolvable winner key renders the raw key, never a blank.
  it("falls back to the raw player key when the winner is not among the seats", () => {
    expect(outcomeLabelKey({ Forfeit: { winner: "ghost" } }, seats)).toEqual({
      key: "outcome.forfeit",
      winner: "ghost",
    });
    // Paired positive: a resolvable key still returns the display name, so
    // "always returns the key" cannot pass.
    expect(outcomeLabelKey({ Forfeit: { winner: "a" } }, seats)).toEqual({
      key: "outcome.forfeit",
      winner: "Ann",
    });
  });
});

// V29 — arm-selective and exhaustive. Neither "always true" nor "always
// false" can pass, and the two `Reported` cases are what forecloses a
// resolution-based ("unresolved only") guard.
describe("isReportable", () => {
  const cases: ReadonlyArray<readonly [string, PairingOutcome | null, boolean]> = [
    ["pending", null, true],
    ["bye", "Bye", false],
    ["forfeit", { Forfeit: { winner: "a" } }, false],
    [
      "reported decisive",
      { Reported: { Decisive: { winner: "a", game_wins: { a: 2, b: 0 } } } },
      true,
    ],
    ["reported draw", { Reported: "Draw" }, true],
  ];

  it.each(cases)("%s", (_label, outcome, expected) => {
    expect(isReportable(outcome)).toBe(expected);
  });
});

// V31 — `null` (no decisive result at all) and `{}` (decisive, but a pod with
// no per-game tally) are different facts and must never be collapsed.
describe("decisiveGameWins", () => {
  const cases: ReadonlyArray<
    readonly [string, PairingOutcome | null, Readonly<Record<string, number>> | null]
  > = [
    ["pending", null, null],
    ["bye", "Bye", null],
    ["forfeit", { Forfeit: { winner: "a" } }, null],
    ["reported draw", { Reported: "Draw" }, null],
    [
      "head-to-head decisive",
      { Reported: { Decisive: { winner: "a", game_wins: { a: 2, b: 1 } } } },
      { a: 2, b: 1 },
    ],
    [
      "pod decisive (single-game per MSTR)",
      { Reported: { Decisive: { winner: "a", game_wins: {} } } },
      {},
    ],
  ];

  it.each(cases)("%s", (_label, outcome, expected) => {
    const actual = decisiveGameWins(outcome);
    if (expected === null) {
      expect(actual).toBeNull();
    } else {
      expect(actual).toEqual(expected);
    }
  });

  it("distinguishes an empty pod tally from no tally at all", () => {
    // The pair RC12 pulls against: collapsing either into the other reds one
    // of these two assertions.
    expect(
      decisiveGameWins({ Reported: { Decisive: { winner: "a", game_wins: {} } } }),
    ).toEqual({});
    expect(decisiveGameWins({ Reported: "Draw" })).toBeNull();
  });
});

describe("tiebreakCells", () => {
  // V4 — both arms, right catalog keys, and every key actually resolves.
  it("projects the head-to-head arm in MTR order with resolvable keys", () => {
    const cells = tiebreakCells({
      HeadToHead: {
        opponents_match_win_pct: 0.5,
        game_win_pct: 0.66,
        opponents_game_win_pct: 0.4,
      },
    });
    expect(cells.map((cell) => cell.id)).toEqual([
      "headToHead.opponentsMatchWinPct",
      "headToHead.gameWinPct",
      "headToHead.opponentsGameWinPct",
    ]);
    expect(cells.map((cell) => cell.format)).toEqual([
      "percent",
      "percent",
      "percent",
    ]);
    expect(cells.map((cell) => cell.value)).toEqual([0.5, 0.66, 0.4]);
    for (const cell of cells) {
      expect(i18n.exists(`tournament:${cell.labelKey}`), cell.labelKey).toBe(true);
      expect(i18n.exists(`tournament:${cell.titleKey}`), cell.titleKey).toBe(true);
    }
  });

  it("projects the multiplayer arm in MSTR order with resolvable keys", () => {
    const cells = tiebreakCells({
      Multiplayer: {
        match_win_pct: 0.75,
        opponents_avg_match_points: 4.5,
        opponents_match_win_pct: 0.33,
      },
    });
    expect(cells.map((cell) => cell.id)).toEqual([
      "multiplayer.matchWinPct",
      "multiplayer.opponentsAvgMatchPoints",
      "multiplayer.opponentsMatchWinPct",
    ]);
    expect(cells.map((cell) => cell.format)).toEqual([
      "percent",
      "points",
      "percent",
    ]);
    for (const cell of cells) {
      expect(i18n.exists(`tournament:${cell.labelKey}`), cell.labelKey).toBe(true);
      expect(i18n.exists(`tournament:${cell.titleKey}`), cell.titleKey).toBe(true);
    }
  });

  // V5's structural half — both arms carry an "opponents' match-win
  // percentage" axis, so the ids must be scheme-qualified or a foreign-arm
  // row would join onto a header it does not belong under.
  it("qualifies cell ids by scheme so the two arms never collide", () => {
    const h2h = tiebreakCells({
      HeadToHead: {
        opponents_match_win_pct: 0.5,
        game_win_pct: 0.5,
        opponents_game_win_pct: 0.5,
      },
    }).map((cell) => cell.id);
    const multiplayer = tiebreakCells({
      Multiplayer: {
        match_win_pct: 0.5,
        opponents_avg_match_points: 3,
        opponents_match_win_pct: 0.5,
      },
    }).map((cell) => cell.id);
    expect(h2h.filter((id) => multiplayer.includes(id))).toEqual([]);
  });
});

// V6 — percent vs points, and `0` is a value rather than an absence.
describe("formatTiebreakValue", () => {
  it("formats a percentage to one decimal place", () => {
    expect(
      formatTiebreakValue({
        id: "headToHead.gameWinPct",
        labelKey: "standings.tiebreaks.headToHead.gameWinPct",
        titleKey: "standings.tiebreaks.headToHead.gameWinPctTitle",
        value: 0.6667,
        format: "percent",
      }),
    ).toBe("66.7%");
  });

  it("formats points to two decimal places", () => {
    expect(
      formatTiebreakValue({
        id: "multiplayer.opponentsAvgMatchPoints",
        labelKey: "standings.tiebreaks.multiplayer.opponentsAvgMatchPoints",
        titleKey: "standings.tiebreaks.multiplayer.opponentsAvgMatchPointsTitle",
        value: 4.5,
        format: "points",
      }),
    ).toBe("4.50");
  });

  it("renders a zero value as a real number, not an absence", () => {
    expect(
      formatTiebreakValue({
        id: "headToHead.opponentsMatchWinPct",
        labelKey: "standings.tiebreaks.headToHead.opponentsMatchWinPct",
        titleKey: "standings.tiebreaks.headToHead.opponentsMatchWinPctTitle",
        value: 0,
        format: "percent",
      }),
    ).toBe("0.0%");
  });
});

describe("gameWinsEntries", () => {
  // V7 — seat order, not `Object.keys` order. The digit keys are the point:
  // `Object.keys({"12":…,"7":…})` yields `["7","12"]`, the exact inverse.
  it("renders all-digit player keys in seat order", () => {
    const digitSeats: readonly PlayerSummary[] = [
      { player_key: "12", display_name: "Twelve", dropped: false },
      { player_key: "7", display_name: "Seven", dropped: false },
    ];
    expect(gameWinsEntries({ "12": 2, "7": 0 }, digitSeats)).toEqual([
      { playerKey: "12", name: "Twelve", wins: 2 },
      { playerKey: "7", name: "Seven", wins: 0 },
    ]);
    // Sibling that passes under both implementations, included so the digit
    // case is visibly the discriminator.
    expect(
      gameWinsEntries({ bob: 1, alice: 2 }, [
        { player_key: "bob", display_name: "Bob", dropped: false },
        { player_key: "alice", display_name: "Alice", dropped: false },
      ]).map((entry) => entry.playerKey),
    ).toEqual(["bob", "alice"]);
  });

  // V8 — a key attributable to no seat is dropped, while the attributable
  // ones still render, so "drops everything" cannot pass.
  it("drops game-wins keys that match no seat", () => {
    expect(gameWinsEntries({ a: 2, b: 1, ghost: 5 }, seats)).toEqual([
      { playerKey: "a", name: "Ann", wins: 2 },
      { playerKey: "b", name: "Bob", wins: 1 },
    ]);
  });

  it("returns nothing for a pod's legitimately empty tally", () => {
    expect(gameWinsEntries({}, seats)).toEqual([]);
  });
});

describe("myPairing", () => {
  const view: TournamentView = {
    summary: {
      code: "ABCD1",
      name: "Friday Night Magic",
      arity: 2,
      bracket: "Swiss",
      status: "InProgress",
      player_count: 2,
      current_round: 2,
      total_rounds: 3,
      created_at: 1_700_000_000,
    },
    players: [...seats],
    pairings: [
      {
        id: 1,
        round: 1,
        players: [...seats],
        outcome: { Reported: { Decisive: { winner: "a", game_wins: { a: 2, b: 0 } } } },
      },
      { id: 2, round: 2, players: [...seats], outcome: null },
    ],
    standings: [],
  };

  // V9 — the current round's pairing, not the first one that mentions me.
  it("returns the current round's pairing", () => {
    expect(myPairing(view, "a")?.id).toBe(2);
  });

  // V10 — spectator and Registration both yield null, paired with V9's
  // positive so "always null" cannot pass.
  it("returns null for a spectator", () => {
    expect(myPairing(view, undefined)).toBeNull();
  });

  it("returns null during registration, when no round has been paired", () => {
    const registering: TournamentView = {
      ...view,
      summary: { ...view.summary, current_round: 0, status: "Registration" },
    };
    expect(myPairing(registering, "a")).toBeNull();
  });

  it("returns null for a player who is not seated this round", () => {
    expect(myPairing(view, "c")).toBeNull();
  });
});

// V11 — none / one / both authorities. A boolean return could not express
// the both-at-once case, which is the normal path for a playing organizer.
describe("viewerRoles", () => {
  const now = 1_700_000_000_000;

  it("expresses no authority", () => {
    expect(viewerRoles(undefined).size).toBe(0);
    expect(viewerRoles({ updatedAt: now } satisfies TournamentCredential).size).toBe(0);
  });

  it("expresses one authority", () => {
    expect(Array.from(viewerRoles({ organizerToken: "o", updatedAt: now }))).toEqual([
      "organizer",
    ]);
    expect(Array.from(viewerRoles({ playerToken: "p", updatedAt: now }))).toEqual([
      "player",
    ]);
  });

  it("expresses both authorities at once", () => {
    const roles = viewerRoles({
      organizerToken: "o",
      playerToken: "p",
      playerKey: "a",
      updatedAt: now,
    });
    expect(roles.size).toBe(2);
    expect(roles.has("organizer")).toBe(true);
    expect(roles.has("player")).toBe(true);
  });
});

// V12 — mirrors `ScoringPolicy::default_for_arity`'s `2n-1 / 1 / 0`. A
// hardcoded 3/1/0 reds the arity-4 case.
describe("defaultScoringForArity", () => {
  it.each([
    [2, 3],
    [4, 7],
    [128, 255],
  ])("arity %i prefills %i win points", (arity, winPoints) => {
    expect(defaultScoringForArity(arity)).toEqual({
      win_points: winPoints,
      draw_points: 1,
      loss_points: 0,
    });
  });
});

// V13 — head-to-head vs pod, carrying the seat count the catalog interpolates.
describe("arityLabel", () => {
  it("labels arity 2 as head-to-head", () => {
    expect(arityLabel(2)).toEqual({ key: "arity.headToHead" });
  });

  it.each([3, 4])("labels arity %i as a pod carrying its seat count", (arity) => {
    expect(arityLabel(arity)).toEqual({ key: "arity.pod", seats: arity });
  });
});

// V7 (phase 5) — total over the credential shapes, and organizer-dominant.
describe("viewerRelation", () => {
  const now = 1_700_000_000_000;
  const cases: [string, TournamentCredential | undefined, ViewerRelation][] = [
    ["an organizer-only credential", { organizerToken: "o", updatedAt: now }, "organizer"],
    ["a player-only credential", { playerToken: "p", updatedAt: now }, "entered"],
    ["no credential at all", undefined, "spectating"],
    // The playing organizer — phase 2 documents this as the NORMAL path, not
    // an exotic one. Precedence resolves it to the stronger claim.
    [
      "a credential holding both tokens",
      { organizerToken: "o", playerToken: "p", playerKey: "a", updatedAt: now },
      "organizer",
    ],
  ];

  it.each(cases)("resolves %s to %s", (_label, credential, expected) => {
    expect(viewerRelation(viewerRoles(credential))).toBe(expected);
  });

  it("returns spectating for a credential carrying neither token", () => {
    expect(viewerRelation(viewerRoles({ updatedAt: now }))).toBe("spectating");
  });

  // Reach-guard: a function that answered "spectating" unconditionally would
  // satisfy two of the six assertions above. Pin that every member really is
  // produced, so the matrix cannot pass degenerately.
  it("produces every member of the union across those inputs", () => {
    const produced = new Set(
      cases.map(([, credential]) => viewerRelation(viewerRoles(credential))),
    );
    expect([...produced].sort()).toEqual(["entered", "organizer", "spectating"]);
  });
});

// V8 (phase 5) — total over every failure shape, with the two
// `not_authorized` roles landing on DIFFERENT keys (a mapping keyed on
// `reason` alone would collapse them).
describe("failureLabel", () => {
  const cases: [string, TournamentFailureInput, string][] = [
    [
      "an organizer refusal",
      { ok: false, reason: "not_authorized", role: "organizer", message: "nope" },
      "errors.notOrganizer",
    ],
    [
      "a player refusal",
      { ok: false, reason: "not_authorized", role: "player", message: "nope" },
      "errors.notEntered",
    ],
    [
      "a broker rejection",
      { ok: false, reason: "rejected", message: "Tournament not found: X" },
      "errors.serverRejected",
    ],
    ["a timeout", { ok: false, reason: "timeout", message: "slow" }, "errors.timedOut"],
    [
      "a lost connection",
      { ok: false, reason: "connection_lost", message: "gone" },
      "errors.connectionLost",
    ],
    ["an abort", { ok: false, reason: "aborted", message: "cancelled" }, "errors.aborted"],
    // V16 — the wire member added with correlated tournament settlement. The
    // `never` terminal in `failureLabel` is a compile-time gate, so the row
    // below is the runtime half: the arm really produces this key, and the key
    // really resolves in the catalog (the `i18n.exists` case at the bottom of
    // this block runs over this same table).
    [
      "an unsupported broker",
      { ok: false, reason: "unsupported", message: "cannot confirm" },
      "errors.unsupported",
    ],
  ];

  it.each(cases)("maps %s to %s", (_label, failure, key) => {
    expect(failureLabel(failure).key).toBe(key);
  });

  // The broker's text is passed through byte-identically. Translating it, or
  // substituting client copy for it, would discard the only diagnostic the
  // `Error` frame carries.
  it("carries the broker's message verbatim on a rejection", () => {
    const message = "Tournament not found: X";
    const label = failureLabel({ ok: false, reason: "rejected", message });
    expect(label).toEqual({ key: "errors.serverRejected", message });
  });

  // Exactly one arm carries an interpolation variable, so a consumer's
  // `"message" in label` narrowing is total.
  it("attaches a message to the rejection arm and to no other", () => {
    const withMessage = cases.filter(([, failure]) => "message" in failureLabel(failure));
    expect(withMessage.map(([, , key]) => key)).toEqual(["errors.serverRejected"]);
  });

  it("maps the two not_authorized roles to different keys", () => {
    expect(
      failureLabel({ ok: false, reason: "not_authorized", role: "organizer", message: "" }).key,
    ).not.toBe(
      failureLabel({ ok: false, reason: "not_authorized", role: "player", message: "" }).key,
    );
  });

  // Every `errors.*` key this function claims really resolves in the catalog.
  it.each(cases)("resolves %s's key in the English catalog", (_label, _failure, key) => {
    expect(i18n.exists(`tournament:${key}`)).toBe(true);
  });
});

// V22 (phase 5) — the client mirror of `authorize_player`'s C2 conjunct.
describe("isActiveEntrant", () => {
  const view: TournamentView = {
    summary: {
      code: "TOUR01",
      name: "Friday Night Magic",
      arity: 4,
      bracket: "Swiss",
      status: "InProgress",
      player_count: 1,
      current_round: 1,
      total_rounds: 3,
      created_at: 1_700_000_000,
    },
    // A full history: the dropped entrant STAYS listed, which is why absence
    // from this array means "not an entrant here", never "dropped".
    players: [
      { player_key: "bob", display_name: "Bob", dropped: false },
      { player_key: "alice", display_name: "Alice", dropped: true },
    ],
    pairings: [],
    standings: [],
  };

  const cases: [string, string | undefined, boolean][] = [
    ["an active entrant", "bob", true],
    ["a dropped entrant", "alice", false],
    // Discriminates the identity join from the flag: a function that only read
    // `dropped` would answer `true` here.
    ["a key belonging to no entrant of this view", "cara", false],
    ["a spectator holding no player key", undefined, false],
  ];

  it.each(cases)("answers %s (key %s) with %s", (_label, playerKey, expected) => {
    expect(isActiveEntrant(view, playerKey)).toBe(expected);
  });

  // Reach-guard: a constant `false` is the trivial way to pass a matrix of
  // three falses. Pin that exactly one input is admitted.
  it("admits exactly one of those four inputs", () => {
    const admitted = cases.filter(([, playerKey]) => isActiveEntrant(view, playerKey));
    expect(admitted.map(([label]) => label)).toEqual(["an active entrant"]);
  });
});

// V28 — the module must never pull the store's runtime in. A value import
// costs 925ms of import time (against 14ms for a type-only one) and drags the
// persistence middleware's localStorage access into every consumer.
describe("tournamentPageState store boundary", () => {
  const source = readFileSync(
    resolve(dirname(fileURLToPath(import.meta.url)), "../tournamentPageState.ts"),
    "utf8",
  );
  const STORE_IMPORT = /import\s+(type\s+)?\{[^}]*\}\s+from\s+"[^"]*multiplayerStore"/g;

  it("imports from multiplayerStore with `import type` only", () => {
    const matches = Array.from(source.matchAll(STORE_IMPORT));
    expect(matches.length).toBeGreaterThan(0);
    for (const match of matches) {
      expect(match[1], match[0]).toBe("type ");
    }
  });

  it("has a detector that would catch a value import", () => {
    // Positive control: a regex that matched nothing could not fail above.
    const offending = `import { useMultiplayerStore } from "../stores/multiplayerStore";`;
    const matches = Array.from(offending.matchAll(STORE_IMPORT));
    expect(matches.length).toBe(1);
    expect(matches[0][1]).toBeUndefined();
  });
});
