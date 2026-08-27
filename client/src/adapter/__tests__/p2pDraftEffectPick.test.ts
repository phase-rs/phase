import { describe, expect, it, vi } from "vitest";

import { P2PDraftGuest } from "../p2p-draft-guest";
import { P2PDraftHost } from "../p2p-draft-host";

describe("P2P draft-effect picks", () => {
  it("serializes guest draft-effect picks without a client-supplied seat", async () => {
    const guest = new P2PDraftGuest(
      {} as never,
      "host-peer",
      {} as never,
      { kind: "new", roomCode: "ABCDE", displayName: "Alice" },
    );
    const send = vi.fn(async () => {});
    (guest as unknown as { session: { send: typeof send } }).session = { send };

    await guest.submitPickWithDraftEffect("cogwork-1", ["card-1", "card-2"]);

    expect(send).toHaveBeenCalledWith({
      type: "draft_pick_with_draft_effect",
      effectCardInstanceId: "cogwork-1",
      cardInstanceIds: ["card-1", "card-2"],
    });
  });

  it("binds a guest draft-effect pick to the host-assigned seat", async () => {
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { set_pool_json: "{}" } } as never,
      "Premier",
      8,
      "Host",
      "Swiss",
      "Competitive",
    );
    const privateHost = host as unknown as {
      draftStarted: boolean;
      paused: boolean;
      handleGuestMessage: (seat: number, message: unknown) => Promise<void>;
      handlePickWithDraftEffect: ReturnType<typeof vi.fn>;
    };
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.handlePickWithDraftEffect = vi.fn(async () => {});

    await privateHost.handleGuestMessage(3, {
      type: "draft_pick_with_draft_effect",
      effectCardInstanceId: "cogwork-1",
      cardInstanceIds: ["card-1", "card-2"],
    });

    expect(privateHost.handlePickWithDraftEffect).toHaveBeenCalledWith(
      3,
      "cogwork-1",
      ["card-1", "card-2"],
    );
  });

  it("rejects host normal and draft-effect picks while paused", async () => {
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { set_pool_json: "{}" } } as never,
      "Premier",
      8,
      "Host",
      "Swiss",
      "Competitive",
    );
    const privateHost = host as unknown as {
      draftStarted: boolean;
      paused: boolean;
      adapter: {
        submitPickForSeat: ReturnType<typeof vi.fn>;
        submitPickWithDraftEffectForSeat: ReturnType<typeof vi.fn>;
      };
    };
    privateHost.draftStarted = true;
    privateHost.paused = true;
    privateHost.adapter = {
      submitPickForSeat: vi.fn(),
      submitPickWithDraftEffectForSeat: vi.fn(),
    };

    await expect(host.submitHostPick(["card-1"])).rejects.toThrow("Draft is paused");
    await expect(
      host.submitHostPickWithDraftEffect("cogwork-1", ["card-1", "card-2"]),
    ).rejects.toThrow("Draft is paused");

    expect(privateHost.adapter.submitPickForSeat).not.toHaveBeenCalled();
    expect(privateHost.adapter.submitPickWithDraftEffectForSeat).not.toHaveBeenCalled();
  });

  /**
   * Boundary D, TS half. `submit_pick_for_seat` now takes a JSON ARRAY of
   * instance ids (one whole CR 903.13b pick step). Both sides of that boundary
   * are `string` and the call is positional, so `tsc` catches NOTHING if only
   * one side moves — this assertion is the substitute for the missing compile
   * error.
   *
   * REVERT-PROBE: change `p2p-draft-host.ts`'s `applyPick` default
   * `submitPickForSeat(seat, cardInstanceIds)` to `(seat, cardInstanceIds[0])`
   * and this reds.
   */
  it("hands the pick adapter a whole pick step, not a bare id", async () => {
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { set_pool_json: "{}" } } as never,
      "Premier",
      8,
      "Host",
      "Swiss",
      "Competitive",
    );
    const privateHost = host as unknown as {
      draftStarted: boolean;
      paused: boolean;
      adapter: Record<string, ReturnType<typeof vi.fn>>;
    };
    privateHost.draftStarted = true;
    privateHost.paused = false;
    privateHost.adapter = {
      submitPickForSeat: vi.fn(async () => ({ status: "Drafting" })),
      // `resolveBotPicks` bails unless the host view says "Drafting", so this
      // keeps the test on the pick seam rather than the bot loop.
      getViewForSeat: vi.fn(async () => ({ status: "Lobby", seats: [] })),
      allPicksSubmitted: vi.fn(async () => false),
    };

    await host.submitHostPick(["card-1"]);

    expect(privateHost.adapter.submitPickForSeat).toHaveBeenCalledWith(0, ["card-1"]);
  });
});
