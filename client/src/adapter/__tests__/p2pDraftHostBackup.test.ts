import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../draft-adapter")>()),
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
import { draftProcedureFixture } from "./draftProcedureFixture";

describe("P2PDraftHost server backup", () => {
  const BACKUP_URL = "https://backup.example";
  const draftingView = {
    status: "Drafting",
    pick_number: 1,
    seats: [
      { seat_index: 0, is_bot: false, display_name: "Host", picks: [] },
      { seat_index: 1, is_bot: true, display_name: "Bot 1", picks: [] },
    ],
    current_pack: [],
    pairings: [],
    current_round: 1,
  } as unknown as DraftPlayerView;

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
      "RMABC",
      BACKUP_URL,
    );
  }

  function wireAdapter(host: P2PDraftHost): void {
    const adapter = (host as unknown as { adapter: Record<string, unknown> }).adapter;
    adapter.draftProcedure = vi.fn(async () => draftProcedureFixture({ pod_size: 2, human_seats: 1 }));
    adapter.loadCardDatabase = vi.fn(async () => 0);
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
    // The server's `is_valid_draft_code` (crates/phase-server/src/admin.rs):
    // exactly six uppercase ASCII alphanumerics, else HTTP 400 before storage.
    expect(body.draft_code).toMatch(/^[A-Z0-9]{6}$/);
  });

  // Synthetic ordering (no printed card): `initialize()` queues a lobby save
  // that is still writing when `startDraft()` queues the started snapshot. The
  // forced upload must go out with the started snapshot, not be consumed by the
  // earlier lobby save.
  it("uploads the started snapshot even when an earlier lobby save is still pending", async () => {
    const host = makeHost();
    wireAdapter(host);
    let releaseLobbySave!: () => void;
    vi.mocked(saveDraftHostSession).mockImplementationOnce(
      () => new Promise<void>((resolve) => { releaseLobbySave = resolve; }),
    );

    await host.initialize();
    const started = host.startDraft();
    // Let startDraft() reach its persist fence (queued behind the lobby save).
    await new Promise((resolve) => setTimeout(resolve, 0));
    releaseLobbySave();
    await started;
    await flushPersistQueue(host);

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const body = JSON.parse(fetchMock.mock.calls[0][1].body as string);
    expect(JSON.parse(body.snapshot_json)).toMatchObject({
      draftStarted: true,
      draftSessionJson: '{"status":"Drafting"}',
    });
  });
});
