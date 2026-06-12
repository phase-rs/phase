import { beforeEach, describe, expect, it, vi } from "vitest";

// Mock the Supabase client so the provider's query-builder chain
// (getSupabaseClient().from(...).select(...).maybeSingle()) is fully captured
// without a live backend or the build-time __SUPABASE_*__ defines.
const { maybeSingleMock, selectMock, getClientMock } = vi.hoisted(() => {
  const maybeSingleMock = vi.fn();
  const selectMock = vi.fn(() => ({ maybeSingle: maybeSingleMock }));
  const fromMock = vi.fn(() => ({ select: selectMock }));
  const getClientMock = vi.fn(() => ({ from: fromMock }));
  return { maybeSingleMock, selectMock, getClientMock };
});

vi.mock("../supabaseClient", () => ({
  getSupabaseClient: getClientMock,
  isSupabaseConfigured: () => true,
}));

import { SupabaseSyncProvider } from "../supabaseProvider";

describe("SupabaseSyncProvider.pullMeta", () => {
  beforeEach(() => vi.clearAllMocks());

  it("reads only revision + updated_at, never the payload column", async () => {
    maybeSingleMock.mockResolvedValue({
      data: { revision: 6, updated_at: "t" },
      error: null,
    });

    await new SupabaseSyncProvider().pullMeta();

    // The egress win depends on the payload column being omitted from the wire.
    expect(selectMock).toHaveBeenCalledWith("revision, updated_at");
  });

  it("coerces a bigint-as-string revision to a number", async () => {
    // Postgres bigint can serialize as a JSON string; without coercion the
    // store's `meta.revision !== lastSyncedRevision` would see "6" !== 6 and
    // force an unnecessary full pull (re-introducing the egress) or a false
    // conflict.
    maybeSingleMock.mockResolvedValue({
      data: { revision: "6", updated_at: "t" },
      error: null,
    });

    const m = await new SupabaseSyncProvider().pullMeta();

    expect(m).toEqual({ revision: 6, updatedAt: "t" });
    expect(typeof m?.revision).toBe("number");
  });

  it("returns null when the account has never synced", async () => {
    maybeSingleMock.mockResolvedValue({ data: null, error: null });

    expect(await new SupabaseSyncProvider().pullMeta()).toBeNull();
  });
});

describe("SupabaseSyncProvider.pull", () => {
  beforeEach(() => vi.clearAllMocks());

  it("coerces a bigint-as-string revision to a number", async () => {
    maybeSingleMock.mockResolvedValue({
      data: { payload: { version: 1 }, revision: "6", updated_at: "t" },
      error: null,
    });

    const snap = await new SupabaseSyncProvider().pull();

    expect(snap?.meta.revision).toBe(6);
    expect(typeof snap?.meta.revision).toBe("number");
  });
});
