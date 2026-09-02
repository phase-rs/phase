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

import { P2PDraftHost } from "../p2p-draft-host";
import { EMPTY_DRAFT_POOL_GROUPS } from "../draft-adapter";
import type { DraftPlayerView, PairingView } from "../draft-adapter";
import type { DraftMatchBinding } from "../../network/draftProtocol";
import type { PersistedDraftHostSession } from "../../services/draftPersistence";

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
function viewFor(
  status: DraftPlayerView["status"],
  round: number,
  pairings: PairingView[],
): DraftPlayerView {
  return {
    status,
    kind: "Premier",
    launch_capability: "None",
    current_pack_number: 3,
    pick_number: 14,
    pass_direction: "Left",
    current_pack: null,
    required_pick_count: 0,
    pick_selection_mode: "Direct",
    pool: [],
    draft_effects: [],
    pool_groups: EMPTY_DRAFT_POOL_GROUPS,
    seats: [],
    cards_per_pack: 14,
    pack_sizes: [14, 14, 14],
    pack_set_codes: ["TST", "TST", "TST"],
    pack_pick_steps: [14, 14, 14],
    pick_steps_per_pack: 14,
    pack_count: 3,
    min_deck_size: 40,
    addable_cards: [],
    timer_remaining_ms: null,
    standings: [],
    current_round: round,
    next_pairing_round: round + 1,
    tournament_format: "Swiss",
    pod_policy: "Casual",
    pairings,
    match_config: { match_type: "Bo1" },
  };
}

/**
 * `seatTokens: {}` is load-bearing: a non-empty map makes `restoreFromPersisted`
 * arm a 5-minute grace timer per guest seat and pause the host, leaving dangling
 * timers behind. Empty leaves `disconnectedSeats` at 0 — no timer, no pause.
 * `settlementOutbox` / `matchBindings` / `bo3State` are left unset so the
 * recovery loops are no-ops.
 */
function persistedSession(): PersistedDraftHostSession {
  return {
    persistenceId: "resume-test",
    roomCode: "ABCDE",
    kind: "Premier",
    podSize: 8,
    hostDisplayName: "Host",
    tournamentFormat: "Swiss",
    podPolicy: "Casual",
    seatTokens: {},
    seatNames: { 0: "Host" },
    kickedTokens: [],
    draftStarted: true,
    draftCode: "draft-12345678",
    draftSessionJson: '{"status":"Pairing"}',
    poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
  };
}

function makeHost(restoredView: DraftPlayerView) {
  const host = new P2PDraftHost(
    { id: "host" } as never,
    () => () => {},
    { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
    "Premier",
    8,
    "Host",
    "Swiss",
    "Casual",
  );

  const adapter = (host as unknown as { adapter: Record<string, unknown> })
    .adapter;
  adapter.importSession = vi.fn(async () => restoredView);
  adapter.generatePairings = vi.fn(async () =>
    viewFor("MatchInProgress", restoredView.current_round + 1, [
      pairing(restoredView.current_round + 1, 0),
    ]),
  );
  adapter.getViewForSeat = vi.fn(async () =>
    viewFor("MatchInProgress", restoredView.current_round + 1, [
      pairing(restoredView.current_round + 1, 0),
    ]),
  );

  // Instance-level stubs shadow the prototype methods; both launch paths call
  // them via `this.`, so no real match dispatch (and no `exportSession`) runs.
  (
    host as unknown as { dispatchMatchLaunch: () => Promise<void> }
  ).dispatchMatchLaunch = vi.fn(async () => {});
  (
    host as unknown as { dispatchMatchLaunchesForSeat: () => Promise<void> }
  ).dispatchMatchLaunchesForSeat = vi.fn(async () => {});

  return { host, adapter };
}

describe("P2PDraftHost.restoreFromPersisted — pairing-window recovery", () => {
  it("regenerates pairings when resuming in the Pairing window at round >= 1", async () => {
    // The measured production state: `AdvanceRound` leaves `current_round` at 1
    // and the previous round's pairings still filter through the view.
    const { host, adapter } = makeHost(
      viewFor("Pairing", 1, [pairing(1, 0)]),
    );

    const restored = await host.restoreFromPersisted(persistedSession());

    // REVERT-FAILING ASSERTION: pre-fix the predicate also required
    // `view.pairings.length === 0`, so this branch was dead for every round >= 1.
    expect(adapter.generatePairings).toHaveBeenCalled();
    // The caller (`draftPodHostAdapter:236-240`) emits this view and derives host
    // status from it, so a stale `Pairing` return strands the user on the pairing
    // screen. Asserting the returned *status*, not that `getViewForSeat` was
    // called: `generatePairings()` already calls it twice internally
    // (`p2p-draft-host.ts:1022,1036`), so a call-count assertion is vacuous here.
    expect(restored?.status).toBe("MatchInProgress");
  });

  it("regenerates pairings when resuming at round 0 with no pairings yet", async () => {
    // Paired positive control: this passes both before and after the fix. Its
    // job is to prove the harness actually reaches the branch — `importSession`
    // resolves, the `draftSessionJson` guard passes, and the spy is wired — so
    // the case above failing cannot mean "the test never got there".
    const { host, adapter } = makeHost(viewFor("Pairing", 0, []));

    await host.restoreFromPersisted(persistedSession());

    expect(adapter.generatePairings).toHaveBeenCalled();
  });

  it("does not regenerate pairings when resuming mid-match", async () => {
    const { host, adapter } = makeHost(
      viewFor("MatchInProgress", 1, [pairing(1, 0)]),
    );

    await host.restoreFromPersisted(persistedSession());

    // Reach-guard: a "not called" result cannot be satisfied by the method
    // bailing out before the branch.
    expect(adapter.importSession).toHaveBeenCalled();
    expect(adapter.generatePairings).not.toHaveBeenCalled();
  });

  it("does not regenerate pairings when resuming with the round complete", async () => {
    // Proves the fix did not broaden to "any non-MatchInProgress status".
    const { host, adapter } = makeHost(
      viewFor("RoundComplete", 1, [pairing(1, 0)]),
    );

    await host.restoreFromPersisted(persistedSession());

    expect(adapter.importSession).toHaveBeenCalled();
    expect(adapter.generatePairings).not.toHaveBeenCalled();
  });

  it("rotates only an active legacy bot authority to its human participant", async () => {
    const activePairing = {
      ...pairing(1, 0),
      match_id: "bot-active",
      seat_a: 1,
      name_a: "Bot",
      seat_b: 2,
      name_b: "Human",
      status: "InProgress" as const,
    };
    const restoredView = viewFor("MatchInProgress", 1, [activePairing]);
    restoredView.seats = [
      { seat_index: 1, is_bot: true },
      { seat_index: 2, is_bot: false },
    ] as DraftPlayerView["seats"];
    const { host } = makeHost(restoredView);
    const legacyBinding: DraftMatchBinding = {
      podId: "draft-12345678",
      matchId: "bot-active",
      round: 1,
      sessionKey: "session",
      lease: "lease",
      nonce: "nonce",
      revision: 0,
      matchAuthoritySeat: 1,
    };
    const futureBinding: DraftMatchBinding = {
      ...legacyBinding,
      matchId: "future-match",
      round: 2,
      matchAuthoritySeat: 1,
    };

    await host.restoreFromPersisted({
      ...persistedSession(),
      matchBindings: [legacyBinding, futureBinding],
    });

    const bindings = (host as unknown as {
      matchBindings: Map<string, DraftMatchBinding>;
      dispatchMatchLaunch: ReturnType<typeof vi.fn>;
    }).matchBindings;
    expect(bindings.get("bot-active")).toEqual({ ...legacyBinding, matchAuthoritySeat: 2 });
    expect(bindings.get("future-match")).toEqual(futureBinding);
    expect((host as unknown as { dispatchMatchLaunch: ReturnType<typeof vi.fn> }).dispatchMatchLaunch)
      .not.toHaveBeenCalled();
  });
});
