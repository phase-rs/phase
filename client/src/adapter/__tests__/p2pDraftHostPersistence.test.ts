import { afterEach, describe, expect, it, vi } from "vitest";

const { clearDraftHostSession, saveDraftHostSession } = vi.hoisted(() => ({
  clearDraftHostSession: vi.fn(async () => {}),
  saveDraftHostSession: vi.fn(async () => {}),
}));

vi.mock("../draft-adapter", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../draft-adapter")>();
  return {
    ...actual,
    DraftAdapter: vi.fn().mockImplementation(function () {
      return {};
    }),
  };
});

vi.mock("../../services/draftPersistence", () => ({
  clearDraftHostSession,
  saveDraftHostSession,
}));

import { P2PDraftHost } from "../p2p-draft-host";

type PersistenceHost = {
  adapter: { exportSession: () => Promise<string> };
  draftStarted: boolean;
  persistQueue: Promise<void>;
  persistSession: () => void;
};

function recoveredHost(hostDisplayName: string): P2PDraftHost {
  return new P2PDraftHost(
    { id: hostDisplayName } as never,
    () => () => {},
    { type: "Set", data: { set_pool_json: "{}" } } as never,
    "Premier",
    8,
    hostDisplayName,
    "Swiss",
    "Casual",
    undefined,
    "shared-recovery",
    "ABCDE",
  );
}

describe("P2PDraftHost persistence disposal", () => {
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("fences a disposed recovery's queued save before a newer recovery can persist", async () => {
    const stale = recoveredHost("Stale host");
    const stalePrivate = stale as unknown as PersistenceHost;
    stalePrivate.draftStarted = true;
    let resolveStaleExport!: (session: string) => void;
    stalePrivate.adapter.exportSession = vi.fn(() => new Promise<string>((resolve) => {
      resolveStaleExport = resolve;
    }));
    stalePrivate.persistSession();
    await Promise.resolve();
    expect(stalePrivate.adapter.exportSession).toHaveBeenCalledOnce();

    // Route cancellation disposes the stale recovery while its queued snapshot
    // is still loading. The fence must be set before this promise can continue.
    const disposeStale = stale.dispose();

    const current = recoveredHost("Current host");
    const currentPrivate = current as unknown as PersistenceHost;
    currentPrivate.draftStarted = false;
    currentPrivate.persistSession();
    await currentPrivate.persistQueue;

    expect(saveDraftHostSession).toHaveBeenCalledTimes(1);
    expect(saveDraftHostSession).toHaveBeenLastCalledWith(
      "shared-recovery",
      expect.objectContaining({ hostDisplayName: "Current host" }),
    );

    resolveStaleExport("{\"status\":\"Drafting\"}");
    await disposeStale;

    expect(saveDraftHostSession).toHaveBeenCalledTimes(1);
    await current.dispose();
  });
});
