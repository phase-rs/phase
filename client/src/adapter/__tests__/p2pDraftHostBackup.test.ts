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

import { generateP2pDraftCode, P2PDraftHost } from "../p2p-draft-host";
import type { DraftPlayerView } from "../draft-adapter";
import { saveDraftHostSession } from "../../services/draftPersistence";
import {
  resolveP2pBackupEndpoint,
  wsUrlToHttpOrigin,
} from "../../config/multiplayerServer";

describe("P2P draft backup contract", () => {
  it("generateP2pDraftCode matches the server 6-char uppercase contract", () => {
    const code = generateP2pDraftCode(() => new Uint8Array([0, 25, 26, 35, 1, 10]));
    expect(code).toBe("AZ09BK");
    expect(code).toMatch(/^[A-Z0-9]{6}$/);
  });

  it("wsUrlToHttpOrigin strips /ws for the backup HTTP base", () => {
    expect(wsUrlToHttpOrigin("wss://lobby.phase-rs.dev/ws")).toBe(
      "https://lobby.phase-rs.dev",
    );
    expect(wsUrlToHttpOrigin("ws://127.0.0.1:9374/ws")).toBe("http://127.0.0.1:9374");
    expect(resolveP2pBackupEndpoint("wss://lobby.phase-rs.dev/ws")).toBe(
      "https://lobby.phase-rs.dev",
    );
  });
});

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
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    fetchMock = vi.fn().mockResolvedValue({ ok: true, status: 200 });
    vi.stubGlobal("fetch", fetchMock);
    warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    warnSpy.mockRestore();
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

  it("uploads a server-valid draft code on draft start", async () => {
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
    expect(body.draft_code).toMatch(/^[A-Z0-9]{6}$/);
    expect(body.draft_code).not.toMatch(/^draft-/);
  });

  it("logs non-2xx upload responses instead of treating them as success", async () => {
    fetchMock.mockResolvedValue({ ok: false, status: 400 });
    const host = makeHost();
    wireAdapter(host);

    await host.startDraft();
    await flushPersistQueue(host);

    expect(warnSpy).toHaveBeenCalledWith(
      expect.stringContaining("server backup upload failed: HTTP 400"),
    );
  });
});
