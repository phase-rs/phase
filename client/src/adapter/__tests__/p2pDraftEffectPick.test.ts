import { describe, expect, it, vi } from "vitest";

import { P2PDraftGuest } from "../p2p-draft-guest";
import { P2PDraftHost } from "../p2p-draft-host";

describe("P2P draft-effect picks", () => {
  it("serializes guest draft-effect picks without a client-supplied seat", async () => {
    const guest = new P2PDraftGuest(
      {} as never,
      "host-peer",
      {} as never,
      "Alice",
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

    await expect(host.submitHostPick("card-1")).rejects.toThrow("Draft is paused");
    await expect(
      host.submitHostPickWithDraftEffect("cogwork-1", ["card-1", "card-2"]),
    ).rejects.toThrow("Draft is paused");

    expect(privateHost.adapter.submitPickForSeat).not.toHaveBeenCalled();
    expect(privateHost.adapter.submitPickWithDraftEffectForSeat).not.toHaveBeenCalled();
  });
});