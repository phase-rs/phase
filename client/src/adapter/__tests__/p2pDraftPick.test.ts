import { describe, expect, it, vi } from "vitest";

import { P2PDraftHost, type DraftHostEvent } from "../p2p-draft-host";

/**
 * U13b — the plural pick channel. Every test here drives the real host seam
 * (`handleGuestMessage` / `resolveBotPicks` / `autoPickAllPending`) rather than
 * asserting a message shape, because the defect this phase closes is a runtime
 * one: a one-id pick into a Commander pod is refused by `apply_pick_inner` with
 * `WrongPickCardCount`, and `resolveBotPicks` has no try/catch, so it strands
 * the round.
 *
 * Modelled on `p2pDraftEffectPick.test.ts`, including its `REVERT-PROBE:`
 * convention: every test names the exact line whose reversion it catches.
 */

type Card = { instance_id: string };

function pack(...ids: string[]): Card[] {
  return ids.map((instance_id) => ({ instance_id }));
}

function newHost(kind: "CommanderDraft" | "Premier" = "CommanderDraft") {
  return new P2PDraftHost(
    { id: "host" } as never,
    () => () => {},
    { type: "Set", data: { set_pool_json: "{}" } } as never,
    kind,
    4,
    "Host",
    "Swiss",
    "Competitive",
  );
}

type PrivateHost = {
  draftStarted: boolean;
  paused: boolean;
  picksThisRound: Set<number>;
  guestSessions: Map<number, { send: ReturnType<typeof vi.fn> }>;
  adapter: Record<string, ReturnType<typeof vi.fn>>;
  handleGuestMessage: (seat: number, message: unknown) => Promise<void>;
  resolveBotPicks: (options: { emit: boolean; persist: boolean }) => Promise<void>;
  autoPickAllPending: () => Promise<void>;
};

function asPrivate(host: P2PDraftHost): PrivateHost {
  return host as unknown as PrivateHost;
}

/** The three adapter methods `applyPick` reaches, plus a per-seat view stub. */
function stubAdapter(viewForSeat: (seat: number) => Record<string, unknown>) {
  return {
    submitPickForSeat: vi.fn(async () => ({ status: "Drafting" })),
    getViewForSeat: vi.fn(async (seat: number) => viewForSeat(seat)),
    allPicksSubmitted: vi.fn(async () => false),
  } as unknown as Record<string, ReturnType<typeof vi.fn>>;
}

describe("P2P plural pick channel", () => {
  /**
   * V3. The wire carries a whole CR 903.13b pick step and the host must hand
   * every id to `submitPickForSeat`, in order, unmangled.
   *
   * REVERT-PROBE: change `p2p-draft-host.ts`'s `applyPick` default
   * `submitPickForSeat(seat, cardInstanceIds)` to `(seat, [cardInstanceIds[0]])`
   * and this reds.
   */
  it("routes a guest's whole pick step to the adapter, in order", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    // Seat 2 has no `seatNames` entry (the constructor sets only seat 0) and no
    // `guestSessions` entry, so `isBotSeat(2)` is true and `applyPick`'s
    // `options.resolveBots && !this.isBotSeat(seat)` is already false — that is
    // the guard that actually fires and keeps this test on the pick seam. The
    // stub's "Lobby" is a real second belt (`resolveBotPicks` returns unless the
    // host view says "Drafting") but it is never reached here.
    privateHost.adapter = stubAdapter(() => ({ status: "Lobby", seats: [] }));

    await privateHost.handleGuestMessage(2, {
      type: "draft_pick",
      cardInstanceIds: ["card-1", "card-2"],
    });

    expect(privateHost.adapter.submitPickForSeat).toHaveBeenCalledWith(2, [
      "card-1",
      "card-2",
    ]);
    // Paired positive reach-guard: the call RESOLVED and the seat entered
    // `picksThisRound`. A thrown `assertPickAllowed` would otherwise satisfy a
    // bare not-called negative vacuously.
    expect(privateHost.picksThisRound.has(2)).toBe(true);
  });

  /**
   * The plan's one judgement call (§R5). On the draft-effect path `applyPick`'s
   * second positional argument — and therefore the `pickReceived` payload —
   * names the cards the seat DRAFTED, never the effect card that paid for them.
   * Nothing else in the tree pins that: `pickReceived` has no other test reader,
   * and both candidates are `string[]`, so `tsc` catches nothing if the argument
   * moves. This is the only revert-probe for the change.
   *
   * REVERT-PROBE: change `handlePickWithDraftEffect`'s second `applyPick`
   * argument from `cardInstanceIds` back to `[effectCardInstanceId]` and this
   * reds — the emitted payload becomes `["cogwork-1"]`.
   */
  it("reports the drafted cards, not the effect card, on the draft-effect path", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = {
      ...stubAdapter(() => ({ status: "Lobby", seats: [] })),
      submitPickWithDraftEffectForSeat: vi.fn(async () => ({ status: "Drafting" })),
    };
    const events: DraftHostEvent[] = [];
    host.onEvent((event) => events.push(event));

    await privateHost.handleGuestMessage(2, {
      type: "draft_pick_with_draft_effect",
      effectCardInstanceId: "cogwork-1",
      cardInstanceIds: ["card-1", "card-2"],
    });

    // Reach-guard: the run really took the draft-effect path, and the effect
    // card really did reach the engine — through the `submitPick` override,
    // which is the one place it belongs. Without this the payload assertion
    // below could pass on a run that never reached the effect branch at all.
    expect(
      privateHost.adapter.submitPickWithDraftEffectForSeat,
    ).toHaveBeenCalledWith(2, "cogwork-1", ["card-1", "card-2"]);
    expect(privateHost.adapter.submitPickForSeat).not.toHaveBeenCalled();

    expect(events.filter((e) => e.type === "pickReceived")).toEqual([
      {
        type: "pickReceived",
        seatIndex: 2,
        cardInstanceIds: ["card-1", "card-2"],
      },
    ]);
  });

  /**
   * V4. RED AT BASE: before this phase `resolveBotPicks` submitted exactly one
   * random card, which a Commander pod refuses.
   *
   * REVERT-PROBE: change `resolveBotPicks`'s
   * `randomDistinctCards(pack, view.required_pick_count)` back to
   * `[pack[randomIndex].instance_id]` and the length assertion reds.
   */
  it("a bot seat drafts a whole step, not one card", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter((seat) =>
      seat === 0
        ? { status: "Drafting", seats: [{ seat_index: 1, is_bot: true }] }
        : {
            status: "Drafting",
            current_pack: pack("c1", "c2", "c3"),
            required_pick_count: 2,
          },
    );

    await privateHost.resolveBotPicks({ emit: false, persist: false });

    expect(privateHost.adapter.submitPickForSeat).toHaveBeenCalledTimes(1);
    const [seat, ids] = privateHost.adapter.submitPickForSeat.mock.calls[0] as [
      number,
      string[],
    ];
    expect(seat).toBe(1);
    expect(ids).toHaveLength(2);
    // Distinct: `apply_pick_inner` refuses a repeated id with
    // `DuplicatePickCardId`.
    expect(new Set(ids).size).toBe(2);
    expect(ids.every((id) => ["c1", "c2", "c3"].includes(id))).toBe(true);
  });

  /**
   * The count is the VIEW's, never the kind's. A `kind === "CommanderDraft" ? 2
   * : 1` implementation, or a literal 2, passes the test above and fails this
   * one — which is the whole reason the engine publishes the number.
   */
  it("reads the count from the view, not from the kind", async () => {
    const host = newHost("CommanderDraft");
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter((seat) =>
      seat === 0
        ? { status: "Drafting", seats: [{ seat_index: 1, is_bot: true }] }
        : {
            status: "Drafting",
            current_pack: pack("c1", "c2", "c3"),
            // CR 903.13b's odd-pack final step, on a Commander session.
            required_pick_count: 1,
          },
    );

    await privateHost.resolveBotPicks({ emit: false, persist: false });

    const [, ids] = privateHost.adapter.submitPickForSeat.mock.calls[0] as [
      number,
      string[],
    ];
    expect(ids).toHaveLength(1);
  });

  it("a one-card pack yields a one-card step, with no out-of-range slice", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter((seat) =>
      seat === 0
        ? { status: "Drafting", seats: [{ seat_index: 1, is_bot: true }] }
        : {
            status: "Drafting",
            current_pack: pack("only-card"),
            required_pick_count: 1,
          },
    );

    await privateHost.resolveBotPicks({ emit: false, persist: false });

    expect(privateHost.adapter.submitPickForSeat).toHaveBeenCalledWith(1, [
      "only-card",
    ]);
  });

  /**
   * V5. The pick-timer sweep owes whole steps too. Its `try`/`catch` logs and
   * swallows, so the assertion is on `submitPickForSeat`'s call payload — never
   * on a rejected promise.
   *
   * REVERT-PROBE: change `autoPickAllPending`'s
   * `randomDistinctCards(view.current_pack, view.required_pick_count)` back to
   * a single `view.current_pack[randomIndex].instance_id` and this reds.
   */
  it("the timer sweep drafts whole steps", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter((seat) =>
      seat === 1
        ? {
            status: "Drafting",
            current_pack: pack("c1", "c2", "c3"),
            required_pick_count: 2,
          }
        : { status: "Drafting", seats: [], current_pack: [], required_pick_count: 0 },
    );

    await privateHost.autoPickAllPending();

    expect(privateHost.adapter.submitPickForSeat).toHaveBeenCalledTimes(1);
    const [seat, ids] = privateHost.adapter.submitPickForSeat.mock.calls[0] as [
      number,
      string[],
    ];
    expect(seat).toBe(1);
    expect(ids).toHaveLength(2);
    expect(new Set(ids).size).toBe(2);
  });

  it("the timer sweep skips a seat already in picksThisRound", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = stubAdapter((seat) =>
      seat === 1 || seat === 2
        ? {
            status: "Drafting",
            current_pack: pack("c1", "c2", "c3"),
            required_pick_count: 2,
          }
        : { status: "Drafting", seats: [], current_pack: [], required_pick_count: 0 },
    );
    privateHost.picksThisRound.add(1);

    await privateHost.autoPickAllPending();

    const seats = privateHost.adapter.submitPickForSeat.mock.calls.map(
      (call) => (call as [number, string[]])[0],
    );
    expect(seats).not.toContain(1);
    // Paired positive: seat 2, unseeded and otherwise identical, IS picked for
    // in the same sweep — so the negative above cannot pass by way of a sweep
    // that did nothing at all.
    expect(seats).toContain(2);
  });

  /**
   * V11. A pick submitted while the draft is paused is refused, and nothing
   * reaches the engine.
   *
   * REVERT-PROBE: remove `handleGuestMessage`'s `canGuestPick(seat)` guard on
   * the `draft_pick` case and this reds.
   */
  it("refuses a pick while paused and submits nothing", async () => {
    const host = newHost();
    const privateHost = asPrivate(host);
    privateHost.draftStarted = true;
    privateHost.paused = true;
    privateHost.adapter = stubAdapter(() => ({ status: "Lobby", seats: [] }));
    const send = vi.fn();
    privateHost.guestSessions.set(2, { send });

    await privateHost.handleGuestMessage(2, {
      type: "draft_pick",
      cardInstanceIds: ["card-1", "card-2"],
    });

    expect(send).toHaveBeenCalledWith({
      type: "draft_error",
      reason: "Draft is paused",
    });
    expect(privateHost.adapter.submitPickForSeat).not.toHaveBeenCalled();

    // Paired positive: the identical fixture with `paused = false` DOES submit,
    // so the negative above is not a fixture that could never pick at all.
    privateHost.paused = false;
    await privateHost.handleGuestMessage(2, {
      type: "draft_pick",
      cardInstanceIds: ["card-1", "card-2"],
    });
    expect(privateHost.adapter.submitPickForSeat).toHaveBeenCalledWith(2, [
      "card-1",
      "card-2",
    ]);
  });
});
