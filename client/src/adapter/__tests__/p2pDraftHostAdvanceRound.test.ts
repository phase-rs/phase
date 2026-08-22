import { describe, expect, it, vi } from "vitest";

// Mock only DraftAdapter's constructor — the rest of the module (notably
// EMPTY_DRAFT_POOL_GROUPS, which p2p-draft-host imports at module scope) must
// stay real, so the factory spreads the original module.
vi.mock("../draft-adapter", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../draft-adapter")>();
  return {
    ...actual,
    DraftAdapter: vi.fn().mockImplementation(function () {
      return {};
    }),
  };
});

import { P2PDraftHost, type DraftHostEvent } from "../p2p-draft-host";
import { EMPTY_DRAFT_POOL_GROUPS } from "../draft-adapter";
import type { DraftPlayerView, PairingView } from "../draft-adapter";

function pairing(round: number, table: number): PairingView {
  return {
    round,
    table,
    seat_a: 0,
    name_a: "Host",
    seat_b: 1,
    name_b: "Guest",
    match_id: `r${round}-t${table}`,
    status: "Pending",
    winner_seat: null,
    score_a: null,
    score_b: null,
  };
}

/** Fully shaped so `isBotSeatFromView` (which reads `view.seats`) cannot throw. */
function viewForRound(round: number): DraftPlayerView {
  return {
    status: "MatchInProgress",
    kind: "Premier",
    current_pack_number: 3,
    pick_number: 14,
    pass_direction: "Left",
    current_pack: null,
    pool: [],
    draft_effects: [],
    pool_groups: EMPTY_DRAFT_POOL_GROUPS,
    seats: [],
    cards_per_pack: 14,
    pack_count: 3,
    min_deck_size: 40,
    addable_cards: [],
    timer_remaining_ms: null,
    standings: [],
    current_round: round,
    tournament_format: "Swiss",
    pod_policy: "Casual",
    pairings: [pairing(round, 0)],
    match_config: { match_type: "Bo1" },
  };
}

describe("P2PDraftHost.advanceRound", () => {
  it("pairs the NEXT round after advancing, reading the round back from the engine", async () => {
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { set_pool_json: "{}" } } as never,
      "Premier",
      8,
      "Host",
      "Swiss",
      "Casual",
    );

    // The engine's contract: `AdvanceRound` does NOT bump `current_round`;
    // only pairing generation commits the next one.
    let round = 1;
    const adapter = (host as unknown as { adapter: Record<string, unknown> })
      .adapter;
    adapter.advanceRound = vi.fn(async () => viewForRound(round));
    adapter.generatePairings = vi.fn(async () => {
      round += 1;
      return viewForRound(round);
    });
    adapter.getViewForSeat = vi.fn(async () => viewForRound(round));

    // Instance-level stub shadows the prototype method; both launch loops call
    // it via `this.`, so no match dispatch (and no `exportSession`) is reached.
    (
      host as unknown as { dispatchMatchLaunch: () => Promise<void> }
    ).dispatchMatchLaunch = vi.fn(async () => {});

    const events: DraftHostEvent[] = [];
    host.onEvent((event) => events.push(event));

    await host.advanceRound();

    // REVERT-FAILING ASSERTION: pre-fix the host passed `view.current_round`
    // (still 1) back into `generatePairings`, so this emitted round 1.
    const pairingsGenerated = events.find((e) => e.type === "pairingsGenerated");
    expect(pairingsGenerated).toBeDefined();
    expect(pairingsGenerated).toMatchObject({ round: 2 });

    // The event itself is still emitted (it just no longer carries a round).
    expect(events.some((e) => e.type === "roundAdvanced")).toBe(true);

    // Reach-guard for this negative: the `pairingsGenerated` assertion above
    // can only pass if generatePairings ran to completion, so an empty error
    // list here is a real negative rather than a vacuous one.
    expect(events.filter((e) => e.type === "error")).toEqual([]);
  });
});
