import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../draft-adapter", () => ({
  DraftAdapter: vi.fn().mockImplementation(function () {
    return {};
  }),
}));

vi.mock("../../services/draftPersistence", () => ({
  saveDraftHostSession: vi.fn().mockResolvedValue(undefined),
  clearDraftHostSession: vi.fn(),
}));

import { P2PDraftHost } from "../p2p-draft-host";
import type { DraftPlayerView } from "../draft-adapter";
import { saveDraftHostSession } from "../../services/draftPersistence";

describe("P2PDraftHost server backup", () => {
  const BACKUP_URL = "https://backup.example";
  const draftingView: DraftPlayerView = {
    status: "Drafting",
    pick_number: 1,
    seats: [
      { seat_index: 0, is_bot: false, display_name: "Host", picks: [] },
      { seat_index: 1, is_bot: true, display_name: "Bot 1", picks: [] },
    ],
    current_pack: [],
    pairings: [],
    current_round: 1,
  } as DraftPlayerView;

  let fetchMock: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    fetchMock = vi.fn().mockResolvedValue({ ok: true });
    vi.stubGlobal("fetch", fetchMock);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  function makeHost(): P2PDraftHost {
    return new P2PDraftHost(
      { id: "host-peer-abc" } as never,
      () => () => {},
      { type: "Set", data: { set_pool_json: "{}" } } as never,
      "Premier",
      2,
      "Host",
      "Swiss",
      "Casual",
      undefined,
      "persist-backup-test",
      "ROOM01",
      BACKUP_URL,
    );
  }

  function wireAdapter(host: P2PDraftHost): void {
    const adapter = (host as unknown as { adapter: Record<string, unknown> }).adapter;
    adapter.createMultiplayerDraft = vi.fn().mockResolvedValue(undefined);
    adapter.getViewForSeat = vi.fn(async () => draftingView);
    adapter.exportSession = vi.fn().mockResolvedValue('{"status":"Drafting"}');
  }

  async function flushPersistQueue(host: P2PDraftHost): Promise<void> {
    await (host as unknown as { persistQueue: Promise<void> }).persistQueue;
  }

  it("uploads the first backup snapshot when the draft starts", async () => {
    const host = makeHost();
    wireAdapter(host);

    await host.startDraft();
    await flushPersistQueue(host);

    expect(saveDraftHostSession).toHaveBeenCalled();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock).toHaveBeenCalledWith(
      `${BACKUP_URL}/p2p-draft-backup`,
      expect.objectContaining({ method: "POST" }),
    );

    const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
    expect(body.host_peer_id).toBe("host-peer-abc");
    expect(body.draft_code).toMatch(/^draft-[0-9a-f]{8}$/);
  });
});
