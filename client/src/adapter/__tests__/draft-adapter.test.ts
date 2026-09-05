import { beforeEach, describe, expect, it, vi } from "vitest";

const wasm = vi.hoisted(() => ({
  default: vi.fn(async () => undefined),
  get_view: vi.fn(),
  create_multiplayer_draft: vi.fn(),
  submit_pick_for_seat: vi.fn(),
  submit_deck: vi.fn(),
  submit_deck_for_seat: vi.fn(),
  draft_procedure: vi.fn(),
}));

vi.mock("@wasm/draft", () => wasm);

import {
  DraftAdapter,
  drainDraftEngineOperations,
  withDraftEngineOperation,
} from "../draft-adapter";

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void;
  const promise = new Promise<void>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe("DraftAdapter engine coordinator", () => {
  beforeEach(async () => {
    await drainDraftEngineOperations();
    vi.clearAllMocks();
  });

  it("serializes local leases with host-facing public operations", async () => {
    const entered = deferred();
    const release = deferred();
    const order: string[] = [];
    wasm.get_view.mockImplementation(() => {
      order.push("host");
      return { status: "Lobby" };
    });

    const localOperation = withDraftEngineOperation(async () => {
      order.push("local-enter");
      entered.resolve();
      await release.promise;
      order.push("local-exit");
    });
    await entered.promise;

    const hostOperation = new DraftAdapter().getView();
    await Promise.resolve();
    expect(wasm.get_view).not.toHaveBeenCalled();

    release.resolve();
    await Promise.all([localOperation, hostOperation]);

    expect(order).toEqual(["local-enter", "local-exit", "host"]);
    expect(wasm.get_view).toHaveBeenCalledOnce();
  });

  it("continues after a rejected operation", async () => {
    await expect(
      withDraftEngineOperation(() => {
        throw new Error("operation failed");
      }),
    ).rejects.toThrow("operation failed");

    wasm.get_view.mockReturnValue({ status: "Lobby" });
    await expect(new DraftAdapter().getView()).resolves.toEqual({ status: "Lobby" });
    expect(wasm.get_view).toHaveBeenCalledOnce();
  });

  it("keeps the complete Commander operation surface behind one lease", async () => {
    wasm.create_multiplayer_draft.mockReturnValue({ status: "Lobby" });
    wasm.submit_pick_for_seat.mockReturnValue({ status: "Drafting" });
    wasm.submit_deck.mockReturnValue({ status: "Deckbuilding" });
    wasm.submit_deck_for_seat.mockReturnValue({ status: "Deckbuilding" });
    wasm.draft_procedure.mockReturnValue({
      commanders_required: 1,
      cube_min_deck_size: 73,
      pick_selection_mode: "Ordered",
    });
    const adapter = new DraftAdapter();

    await adapter.createMultiplayerDraft(
      { type: "Set", data: { set_pool_json: "{}" } },
      [],
      "CommanderDraft",
      7,
      "DRAFT",
      "Swiss",
      "Competitive",
    );
    await adapter.submitPickForSeat(2, ["first", "second"]);
    await adapter.submitDeck(["Island"], ["Commander"]);
    await adapter.submitDeckForSeat(2, ["Island"], ["Commander"]);
    const procedure = await adapter.draftProcedure("CommanderDraft", "Swiss");

    expect(wasm.create_multiplayer_draft).toHaveBeenCalledWith(
      JSON.stringify({ type: "Set", data: { set_pool_json: "{}" } }),
      JSON.stringify([]),
      4,
      7,
      "DRAFT",
      "Swiss",
      "Competitive",
    );
    expect(wasm.draft_procedure).toHaveBeenCalledWith(4, "Swiss");
    expect(wasm.submit_pick_for_seat).toHaveBeenCalledWith(2, '["first","second"]');
    expect(wasm.submit_deck).toHaveBeenCalledWith('["Island"]', '["Commander"]');
    expect(wasm.submit_deck_for_seat).toHaveBeenCalledWith(2, '["Island"]', '["Commander"]');
    expect(procedure.pick_selection_mode).toBe("Ordered");
    expect(procedure.cube_min_deck_size).toBe(73);
  });

  it("passes an explicit empty commander designation for local limited submissions", async () => {
    wasm.submit_deck.mockReturnValue({ status: "Deckbuilding" });

    await new DraftAdapter().submitDeck(["Island"], []);

    expect(wasm.submit_deck).toHaveBeenCalledWith('["Island"]', "[]");
  });
});
