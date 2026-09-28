import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { P2PDraftHost } from "../p2p-draft-host";
import type { DraftKind, DraftProcedure, MultiplayerSeatDescriptor, PackDistribution } from "../draft-adapter";
import { draftProcedureFixture } from "./draftProcedureFixture";

/**
 * Host-side bot fill must follow the HOST'S request, and no per-kind scalar.
 *
 * This suite was written when the engine refused a bot seat under
 * `PackDistribution::SharedStackPiles`, and its subject was never that refusal:
 * it was the shape of the guard. The host asked
 * `procedure.human_seats !== procedure.pod_size`, a scalar that merely
 * CORRELATES — and correlates wrongly, because the engine's procedure table
 * seats humans in every seat for Premier, Traditional and Sealed as well
 * (`human_seats == pod_size == 8`). That silently disabled bot fill for three
 * kinds that always permitted it, leaving a one-human pod to fail
 * `createMultiplayerDraft`'s `min_pod_size` floor after `canStart` had already
 * passed on `botFillEnabled`.
 *
 * The refusal is gone — `resolve_shared_stack_bot_turns` drives a seated
 * Winston bot — so the Winston row JOINS the other four rather than being
 * deleted: every distribution the engine ships now takes bot fill, and the
 * three `human_seats === pod_size` rows still carry the original subject and
 * its revert-probe. The coverage is generalized, not lost.
 *
 * The rows below therefore carry the REAL engine scalars, because a fixture
 * with `human_seats: 1` (the shared `draftProcedureFixture` default) is
 * exactly what let the regression through: under it both predicates agree.
 */
describe("P2PDraftHost bot fill dispatches on the host's request, not a per-kind scalar", () => {
  /**
   * `human_seats` and `pod_size` mirror `DraftKind::procedure()` in
   * `crates/draft-core/src/types.rs`. `botFillPermitted` is the ENGINE's
   * answer; it is `true` on every row now, and the field is KEPT rather than
   * deleted so a future distribution that genuinely cannot seat a bot has a
   * column to say so in, instead of arriving as a new bespoke test.
   */
  // @sync-with: crates/draft-core/src/types.rs `DraftKind::procedure`
  const ROWS: ReadonlyArray<{
    kind: Exclude<DraftKind, "Quick">;
    pod_size: number;
    human_seats: number;
    distribution: PackDistribution;
    botFillPermitted: boolean;
  }> = [
    // The three rows the scalar predicate got WRONG: humans in every seat, yet
    // `PickAndPass`, so the reducer accepts a bot seat and bot fill is legal.
    { kind: "Premier", pod_size: 8, human_seats: 8, distribution: "PickAndPass", botFillPermitted: true },
    { kind: "Traditional", pod_size: 8, human_seats: 8, distribution: "PickAndPass", botFillPermitted: true },
    { kind: "Sealed", pod_size: 8, human_seats: 8, distribution: "AllAtOnce", botFillPermitted: true },
    // The row both predicates happened to agree on.
    { kind: "CommanderDraft", pod_size: 4, human_seats: 1, distribution: "PickAndPass", botFillPermitted: true },
    // FLIPPED. This row read `botFillPermitted: false` while the reducer
    // refused a bot seat under `SharedStackPiles`. It does not any more: the
    // seat is dealt like any other and `resolve_shared_stack_bot_turns` drives
    // it. Its `human_seats === pod_size` now carries the same meaning as the
    // three rows above — a scalar that must not be read as a refusal.
    {
      kind: "Winston",
      pod_size: 2,
      human_seats: 2,
      distribution: { SharedStackPiles: { pile_count: 3 } },
      botFillPermitted: true,
    },
  ];

  // A shared-stack pod that seats a bot fetches `__CARD_DATA_URL__` before
  // `createMultiplayerDraft`. Stubbed so the Winston row exercises the seat
  // descriptors rather than the network; the fetch itself is the subject of
  // `p2pDraftHostWinstonBots.test.ts`.
  const originalFetch = globalThis.fetch;
  beforeEach(() => {
    globalThis.fetch = vi.fn(async () => new Response("{}", { status: 200 }));
  });
  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  async function startWith(row: (typeof ROWS)[number], botFillEmptySeats: boolean) {
    const procedure: DraftProcedure = draftProcedureFixture({
      pod_size: row.pod_size,
      human_seats: row.human_seats,
      distribution: row.distribution,
      allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
    });
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
      row.kind,
      row.pod_size,
      "Host",
      "Swiss",
      "Competitive",
    );
    // Typed on the seat parameter specifically: the descriptor list is the whole
    // subject of this suite, so reading it back as `unknown` would defeat the
    // assertions below.
    const createMultiplayerDraft = vi.fn(
      async (_pool: unknown, _seats: MultiplayerSeatDescriptor[]) => {},
    );
    // `Lobby`, so neither `resolveBotPicks` nor `startPickTimer` runs: this
    // test is about the seat descriptors `startDraft` hands the engine.
    const getViewForSeat = vi.fn(async () => ({ status: "Lobby" }));
    // A shared-stack pod that seats a bot loads the card database before
    // `createMultiplayerDraft`, because the Winston valuation reads card faces.
    // Stubbed here so this suite stays about the seat descriptors;
    // `p2pDraftHostWinstonBots.test.ts` owns that load's own two-sided test.
    (host as unknown as { adapter: unknown }).adapter = {
      draftProcedure: vi.fn(async () => procedure),
      createMultiplayerDraft,
      getViewForSeat,
      loadCardDatabase: vi.fn(async () => 0),
    };

    await host.initialize();
    await host.startDraft(botFillEmptySeats);

    expect(createMultiplayerDraft).toHaveBeenCalledOnce();
    return createMultiplayerDraft.mock.calls[0]![1];
  }

  /**
   * REVERT-PROBE — this is the test the regression needed and did not have.
   * Restore `botFillEmptySeats && procedure.human_seats !== procedure.pod_size`
   * and the three `human_seats === pod_size` rows red here with zero bot seats;
   * restore `&& !isSharedStackDistribution(procedure.distribution)` and the
   * Winston row reds the same way.
   */
  it.each(ROWS.filter((row) => row.botFillPermitted))(
    "fills empty $kind seats with bots even when human_seats equals pod_size",
    async (row) => {
      const seats = await startWith(row, true);

      // Reach-guard: the host seat is present, so `startDraft` really built
      // the descriptor list rather than short-circuiting somewhere earlier.
      // Without this, the bot count below could not tell "bot fill ran" from
      // "nothing ran at all".
      expect(seats.filter((seat) => seat.type === "Human")).toEqual([
        { type: "Human", player_id: 0, display_name: "Host" },
      ]);
      // Every seat the pod declares is filled, which is precisely what keeps
      // `createMultiplayerDraft` clear of the `min_pod_size` floor.
      expect(seats).toHaveLength(row.pod_size);
      expect(seats.filter((seat) => seat.type === "Bot")).toHaveLength(row.pod_size - 1);
    },
  );

  /**
   * RE-AIMED (was: "never seats a bot in a $kind pod, whose distribution the
   * reducer refuses", over the rows the engine refused). No row is refused any
   * more, so the paired negative moves to the input that still says no: the
   * HOST'S own opt-out. Run over EVERY row, including Winston, so the last
   * remaining conjunct is pinned for every distribution rather than for the one
   * kind the old single-case version happened to use.
   *
   * It is not vacuous: the reach-guard above proves the descriptor list really
   * is built for each of these kinds, so an empty bot count here is a decision
   * rather than an absent code path. Reducing `botFillAllowed` to a constant
   * `true` reds this.
   */
  it.each(ROWS)(
    "leaves a $kind pod short when the host declines bot fill",
    async (row) => {
      const seats = await startWith(row, false);

      expect(seats).toEqual([{ type: "Human", player_id: 0, display_name: "Host" }]);
      expect(seats.filter((seat) => seat.type === "Bot")).toHaveLength(0);
    },
  );
});
