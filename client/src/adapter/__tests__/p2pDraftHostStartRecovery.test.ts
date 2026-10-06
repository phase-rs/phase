import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { clearDraftHostSession, loadDraftHostSession, saveDraftHostSession } = vi.hoisted(() => ({
  clearDraftHostSession: vi.fn(async () => {}),
  loadDraftHostSession: vi.fn(async () => null),
  saveDraftHostSession: vi.fn<(id: string, session: unknown) => Promise<void>>(async () => {}),
}));
vi.mock("../../services/draftPersistence", () => ({
  clearDraftHostSession,
  loadDraftHostSession,
  saveDraftHostSession,
}));

import { P2PDraftHost } from "../p2p-draft-host";
import type { DraftProcedure } from "../draft-adapter";
import { draftProcedureFixture } from "./draftProcedureFixture";

/**
 * A FAILED START MUST STAY RETRYABLE.
 *
 * `startDraftInner` opens with `if (this.draftStarted) return`. It used to raise
 * that flag before the bot loop and before the durable snapshot, so a throw from
 * either left it standing over a pod that does not exist anywhere a client can
 * see: the engine holds a draft, no snapshot was written, no guest was told, and
 * every retry hits the guard and returns silently. The host looks idle and the
 * Start button does nothing, forever.
 *
 * The flags come back down on failure now. What this pins is the RETRY, not the
 * rejection -- a test that only asserted `rejects` would pass with the bug fully
 * intact, because the first call threw either way.
 */
describe("P2PDraftHost start recovery", () => {
  const originalFetch = globalThis.fetch;

  beforeEach(() => {
    globalThis.fetch = vi.fn(async () => new Response("{}", { status: 200 })) as typeof fetch;
    saveDraftHostSession.mockReset();
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  function hostWithAdapter() {
    const procedure: DraftProcedure = draftProcedureFixture({
      pod_size: 2,
      human_seats: 2,
      distribution: { SharedStackPiles: { pile_count: 3 } },
      allowed_pod_sizes: [2, 3, 4],
    });
    const host = new P2PDraftHost(
      { id: "host" } as never,
      () => () => {},
      { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } } as never,
      "Winston",
      2,
      "Host",
      "Swiss",
      "Competitive",
    );
    const createMultiplayerDraft = vi.fn(async () => {});
    // A DISTINGUISHABLE session blob per attempt, so a snapshot can be traced
    // back to the start that produced it.
    const exportSession = vi.fn(async () => "SESSION-A");
    (host as unknown as { adapter: unknown }).adapter = {
      draftProcedure: vi.fn(async () => procedure),
      createMultiplayerDraft,
      // `Lobby`, so the bot loop is skipped and the persist below is the only
      // thing that can fail -- the failure this test is about.
      getViewForSeat: vi.fn(async () => ({ status: "Lobby" })),
      exportSession,
      loadCardDatabase: vi.fn(async () => 0),
    };
    // Persistence is a no-op without an id, which would make the fixture unable
    // to fail at all.
    (host as unknown as { persistenceId: string }).persistenceId = "start-recovery";
    return { host, createMultiplayerDraft, exportSession };
  }

  it("retries a start whose durable snapshot failed", async () => {
    const { host, createMultiplayerDraft } = hostWithAdapter();
    // AFTER `initialize`, which persists once on its own -- arming the rejection
    // before it would spend the single failure on the wrong write and leave the
    // start to succeed, which is exactly how this test first failed.
    saveDraftHostSession.mockResolvedValue(undefined);
    await host.initialize();
    saveDraftHostSession
      .mockRejectedValueOnce(new Error("IndexedDB unavailable"))
      .mockResolvedValue(undefined);

    await expect(host.startDraft(true)).rejects.toThrow("IndexedDB unavailable");

    // Reach guard: the first attempt really did get as far as the engine.
    expect(createMultiplayerDraft).toHaveBeenCalledOnce();

    // THE CLAIM. Leave `draftStarted` standing on the failure path and this
    // second call returns at the guard, `createMultiplayerDraft` stays at one,
    // and the pod is stranded with the host reporting nothing wrong.
    await expect(host.startDraft(true)).resolves.toBeUndefined();
    expect(createMultiplayerDraft).toHaveBeenCalledTimes(2);
  });

  /**
   * A ROLLED-BACK START MUST NOT COME BACK LATER.
   *
   * `enqueuePersistSession` keeps a failed engine-backed snapshot in
   * `pendingDraftSnapshot` and flushes it AHEAD of newer state on the next
   * persist. That is right for a pick or a deck submission: those record a
   * reducer result already applied and owed to the player, so they must be
   * replayed rather than recomputed.
   *
   * A failed START is the opposite. The rollback unwinds it, so the draft the
   * snapshot describes is abandoned — and retaining it meant the next save
   * wrote the abandoned draft to IndexedDB, where a reload would restore a pod
   * the host had already been told did not start.
   *
   * The existing retry row proves the second start REACHES the adapter. It
   * cannot see this: the resurrection happens on the persist queue, after the
   * call it asserts on.
   *
   * REVERT-FAILING ON THE PAIR: the option and the rollback clear each suppress
   * the resurrection on their own, so removing BOTH is what reddens this row.
   * The row after it pins the clear by itself, with a snapshot queued before
   * the start's own persist -- which the option cannot reach.
   */
  it("does not resurrect the abandoned draft on a later save", async () => {
    const { host, exportSession } = hostWithAdapter();
    saveDraftHostSession.mockResolvedValue(undefined);
    await host.initialize();
    saveDraftHostSession
      .mockRejectedValueOnce(new Error("IndexedDB unavailable"))
      .mockResolvedValue(undefined);

    await expect(host.startDraft(true)).rejects.toThrow("IndexedDB unavailable");
    saveDraftHostSession.mockClear();

    // The retry produces a DIFFERENT session, so anything carrying the first
    // one is the abandoned draft rather than the live one.
    exportSession.mockResolvedValue("SESSION-B");
    await expect(host.startDraft(true)).resolves.toBeUndefined();

    const written = saveDraftHostSession.mock.calls
      .map(([, snapshot]) => (snapshot as { draftSessionJson?: unknown }).draftSessionJson);
    // Reach guard: the retry really did persist, so "no SESSION-A" is not
    // vacuously true over an empty list.
    expect(written).toContain("SESSION-B");
    expect(written).not.toContain("SESSION-A");
  });

  /**
   * THE ROLLBACK CLEAR, PINNED ON ITS OWN.
   *
   * `retainFailedDraftSnapshot: false` only governs whether THIS start's own
   * failure queues a snapshot. A snapshot queued earlier — by any prior failed
   * engine-backed save — is already sitting in `pendingDraftSnapshot`, and
   * `enqueuePersistSession` flushes it AHEAD of newer state. The rollback has to
   * drop it, or the abandoned draft still reaches IndexedDB by that route.
   *
   * REVERT-FAILING: delete `pendingDraftSnapshot = null` from the rollback and
   * this reds while the row above stays green, which is the whole point of
   * having both.
   */
  it("drops a snapshot queued before the failed start", async () => {
    const { host, exportSession } = hostWithAdapter();
    saveDraftHostSession.mockResolvedValue(undefined);
    await host.initialize();

    // Queued by something earlier than this start, so the option above cannot
    // be what suppresses it.
    (host as unknown as { pendingDraftSnapshot: unknown }).pendingDraftSnapshot = {
      persistenceId: "start-recovery",
      draftSessionJson: "SESSION-STALE",
    };
    saveDraftHostSession
      .mockRejectedValueOnce(new Error("IndexedDB unavailable"))
      .mockResolvedValue(undefined);

    await expect(host.startDraft(true)).rejects.toThrow("IndexedDB unavailable");
    saveDraftHostSession.mockClear();

    exportSession.mockResolvedValue("SESSION-B");
    await expect(host.startDraft(true)).resolves.toBeUndefined();

    const written = saveDraftHostSession.mock.calls
      .map(([, snapshot]) => (snapshot as { draftSessionJson?: unknown }).draftSessionJson);
    expect(written).toContain("SESSION-B");
    expect(written).not.toContain("SESSION-STALE");
  });

  it("does not restart a draft that started cleanly", async () => {
    // The paired positive: the rollback must not weaken the guard it rolls back.
    // Without this, "always allow a restart" would pass the test above.
    const { host, createMultiplayerDraft } = hostWithAdapter();
    saveDraftHostSession.mockResolvedValue(undefined);

    await host.initialize();
    await host.startDraft(true);
    expect(createMultiplayerDraft).toHaveBeenCalledOnce();

    await host.startDraft(true);
    expect(createMultiplayerDraft).toHaveBeenCalledOnce();
  });
});
