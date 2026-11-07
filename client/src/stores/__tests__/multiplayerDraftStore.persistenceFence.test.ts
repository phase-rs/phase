import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";

const {
  checkRestorableSession,
  clearDraftHostSession,
  engine,
  hostRoom,
  joinRoom,
  loadDraftHostSession,
  saveActiveDraftPod,
  saveDraftHostSession,
} = vi.hoisted(() => ({
  checkRestorableSession: vi.fn(),
  clearDraftHostSession: vi.fn(async () => {}),
  /**
   * The draft engine holds one installed session, which every `DraftAdapter`
   * shares: an import writes it and an export reads it. A failed import
   * installs nothing.
   */
  engine: {
    session: null as string | null,
    imports: [] as string[],
    atImport: null as (() => void) | null,
    importFailure: null as Error | null,
    afterImport: null as (() => Promise<void>) | null,
  },
  hostRoom: vi.fn(),
  joinRoom: vi.fn(),
  loadDraftHostSession: vi.fn(async () => ({ type: "absent" })),
  saveActiveDraftPod: vi.fn(),
  saveDraftHostSession: vi.fn(async () => {}),
}));

vi.mock("../../adapter/draft-adapter", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../adapter/draft-adapter")>();
  return {
    ...actual,
    DraftAdapter: vi.fn().mockImplementation(function () {
      return {
        draftProcedure: vi.fn(async () => draftProcedureFixture({
          pod_size: 8,
          human_seats: 8,
          min_pod_size: 2,
          max_pod_size: 8,
          allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
          packs_per_player: 3,
          cards_per_pick: 1,
          distribution: "PickAndPass",
          min_deck_size: 40,
          match_config: { match_type: "Bo1" },
        })),
        checkRestorableSession,
        importSession: vi.fn(async (json: string) => {
          engine.imports.push(json);
          engine.atImport?.();
          if (engine.importFailure) throw engine.importFailure;
          engine.session = json;
          await engine.afterImport?.();
          return {
            status: "Deckbuilding", kind: "Premier", pool: [], pairings: [], current_round: 0,
            seats: [{ seat_index: 0 }], pack_count: 3, cards_per_pack: 14, min_deck_size: 40,
          };
        }),
        exportSession: vi.fn(async () => engine.session),
      };
    }),
  };
});

vi.mock("../../network/connection", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../network/connection")>();
  return { ...actual, hostRoom, joinRoom };
});

// A held guest seat, joined without a live host connection.
vi.mock("../../adapter/p2p-draft-guest", () => ({
  P2PDraftGuest: vi.fn().mockImplementation(function () {
    return {
      onEvent: () => () => {},
      initialize: vi.fn(async () => {}),
      leave: vi.fn(async () => {}),
      dispose: vi.fn(),
      get isRecoveryRevoked() {
        return false;
      },
      view: null,
      seat: null,
      token: null,
    };
  }),
}));

vi.mock("../../services/draftPersistence", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../services/draftPersistence")>();
  return {
    ...actual,
    clearDraftHostSession,
    loadDraftHostSession,
    saveActiveDraftPod,
    saveDraftHostSession,
  };
});

import { DRAFT_OFFLINE_ERROR, useMultiplayerDraftStore } from "../multiplayerDraftStore";
import { isHostedPodRecoveryFailureCurrent, useDraftPodStore } from "../draftPodStore";
import { useConnectivityStore } from "../connectivityStore";
import { DraftPodHostAdapter } from "../../adapter/draftPodHostAdapter";
import { P2PDraftHost } from "../../adapter/p2p-draft-host";
import { Draft27IncompatibilityError, admitDraft27GuestView } from "../../adapter/draft-compatibility";
import { inspectActiveDraftPod } from "../../services/draftPersistence";
import type { PersistedDraftHostSession } from "../../services/draftPersistence";

const installedSession = JSON.stringify({ status: "Drafting", config: { rng_seed: 1 } });
const recoveredSession = JSON.stringify({ status: "Drafting", config: { rng_seed: 2 } });
const ownSession: Record<string, string> = {
  "installed-owner": installedSession,
  "recovered-b": recoveredSession,
};
/** A started saved host whose record carries a draft session. */
const recoveredHost: PersistedDraftHostSession = {
  persistenceId: "recovered-b",
  roomCode: "BBBBB",
  kind: "Premier",
  podSize: 8,
  hostDisplayName: "Host",
  tournamentFormat: "Swiss",
  podPolicy: "Competitive",
  seatTokens: { 0: "host" },
  seatNames: { 0: "Host" },
  kickedTokens: [],
  draftStarted: true,
  draftCode: "recovered-b",
  draftSessionJson: recoveredSession,
  poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
};

/** The draft sessions saved under one persistence id, in order. */
function savedUnder(persistenceId: string): (string | null)[] {
  return saveDraftHostSession.mock.calls
    .filter((call) => (call as unknown[])[0] === persistenceId)
    .map((call) => ((call as unknown[])[1] as PersistedDraftHostSession).draftSessionJson);
}

/** Hosts A, starts it on its own draft session, and runs A's own save path once. */
async function startInstalledHost(): Promise<{ saveA: () => Promise<void> }> {
  const started = vi.spyOn(P2PDraftHost.prototype, "initialize");
  await expect(useMultiplayerDraftStore.getState().hostDraft({
    poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    kind: "Premier",
    podSize: 8,
    hostDisplayName: "Host",
    tournamentFormat: "Swiss",
    podPolicy: "Competitive",
    persistenceId: "installed-owner",
  })).resolves.toEqual({ status: "opened" });
  const hostA = started.mock.contexts[0] as P2PDraftHost;
  Reflect.set(hostA, "draftStarted", true);
  engine.session = installedSession;
  const saveA = () => (Reflect.get(hostA, "persistSessionStrict") as () => Promise<void>).call(hostA);
  await saveA();
  // Reach: A's own save path saves A's session under A's id.
  expect(savedUnder("installed-owner")).toContain(installedSession);
  return { saveA };
}

/** Holds the recovering host after its import has written the engine's session. */
function holdAfterImport(): { release: () => void; reached: Promise<void> } {
  let release!: () => void;
  let reach!: () => void;
  const released = new Promise<void>((resolve) => {
    release = resolve;
  });
  const reached = new Promise<void>((resolve) => {
    reach = resolve;
  });
  engine.afterImport = () => {
    reach();
    return released;
  };
  return { release, reached };
}

/** The typed refusal a restore raises for a draft this client does not admit. */
function incompatibleRestore(): Draft27IncompatibilityError {
  const refusal = admitDraft27GuestView({ status: "Review" });
  if (refusal.ok) throw new Error("A Review draft must be refused");
  return new Draft27IncompatibilityError(refusal.error);
}

describe("multiplayerDraftStore recovered-host persistence fence", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    let hostNumber = 0;
    hostRoom.mockImplementation(async () => {
      hostNumber++;
      return {
        roomCode: "ABCDE",
        peerId: `phase2-ABCDE-${hostNumber}`,
        peer: { destroy: vi.fn() } as never,
        onGuestConnected: vi.fn(() => vi.fn()),
        destroy: vi.fn(),
      };
    });
  });

  afterEach(async () => {
    await useMultiplayerDraftStore.getState().leave(true);
  });

  it("holds a replacement recovery until a route-aborted host's in-flight save drains", async () => {
    let releaseStaleSave!: () => void;
    saveDraftHostSession.mockImplementationOnce(
      () => new Promise<void>((resolve) => {
        releaseStaleSave = resolve;
      }),
    );
    const config = {
      poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier" as const,
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss" as const,
      podPolicy: "Competitive" as const,
      persistenceId: "shared-recovery",
    };
    const controller = new AbortController();

    await expect(useMultiplayerDraftStore.getState().hostDraft({
      ...config,
      signal: controller.signal,
    })).resolves.toEqual({ status: "opened" });
    await Promise.resolve();
    expect(saveDraftHostSession).toHaveBeenCalledOnce();

    controller.abort();
    const replacement = useMultiplayerDraftStore.getState().hostDraft(config);
    await Promise.resolve();
    await Promise.resolve();

    expect(hostRoom).toHaveBeenCalledOnce();
    expect(saveDraftHostSession).toHaveBeenCalledOnce();

    releaseStaleSave();

    await expect(replacement).resolves.toEqual({ status: "opened" });
    expect(hostRoom).toHaveBeenCalledTimes(2);
    expect(saveDraftHostSession).toHaveBeenCalledTimes(2);
  });

  it("holds a replacement recovery until a route-aborted delayed hostRoom is destroyed", async () => {
    const staleHostResult = {
      roomCode: "ABCDE",
      peerId: "phase2-ABCDE-stale",
      peer: { destroy: vi.fn() } as never,
      onGuestConnected: vi.fn(() => vi.fn()),
      destroy: vi.fn(),
    };
    let resolveStaleHostRoom!: (result: typeof staleHostResult) => void;
    hostRoom.mockReset();
    hostRoom.mockImplementationOnce(
      () => new Promise<typeof staleHostResult>((resolve) => {
        resolveStaleHostRoom = resolve;
      }),
    );
    hostRoom.mockImplementationOnce(async () => {
      expect(staleHostResult.destroy).toHaveBeenCalledOnce();
      return {
        roomCode: "ABCDE",
        peerId: "phase2-ABCDE-current",
        peer: { destroy: vi.fn() } as never,
        onGuestConnected: vi.fn(() => vi.fn()),
        destroy: vi.fn(),
      };
    });
    const config = {
      poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
      kind: "Premier" as const,
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss" as const,
      podPolicy: "Competitive" as const,
      persistenceId: "shared-recovery",
    };
    const controller = new AbortController();
    const stale = useMultiplayerDraftStore.getState().hostDraft({
      ...config,
      signal: controller.signal,
    });
    for (let turn = 0; turn < 50 && hostRoom.mock.calls.length === 0; turn++) await Promise.resolve();

    controller.abort();
    const replacement = useMultiplayerDraftStore.getState().hostDraft(config);
    await Promise.resolve();
    await Promise.resolve();

    expect(hostRoom).toHaveBeenCalledOnce();
    resolveStaleHostRoom(staleHostResult);

    await expect(stale).resolves.toEqual({ status: "superseded" });
    await expect(replacement).resolves.toEqual({ status: "opened" });
    expect(hostRoom).toHaveBeenCalledTimes(2);
  });

  describe("a recovery whose draft engine check refuses or cannot answer", () => {
    afterEach(() => {
      vi.restoreAllMocks();
    });

    const recovered: PersistedDraftHostSession = {
      persistenceId: "recovered-pod",
      roomCode: "BBBBB",
      kind: "Premier",
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss",
      podPolicy: "Competitive",
      seatTokens: { 0: "host" },
      seatNames: { 0: "Host" },
      kickedTokens: [],
      draftStarted: true,
      draftCode: "recovered-pod",
      draftSessionJson: JSON.stringify({ status: "Drafting", config: { rng_seed: 7 } }),
      poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
    };

    /** Installs a live host A, then offers the lifecycle a recovery of the saved pod. */
    async function recoverBesideInstalledHost() {
      await expect(useMultiplayerDraftStore.getState().hostDraft({
        poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
        kind: "Premier",
        podSize: 8,
        hostDisplayName: "Host",
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
        persistenceId: "installed-owner",
      })).resolves.toEqual({ status: "opened" });
      const installedPeer = await hostRoom.mock.results[0]!.value;
      // Reach: host A is installed and owns the pod session.
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: "installed-owner" });
      const initialize = vi.spyOn(DraftPodHostAdapter.prototype, "initialize");
      const outcome = await useMultiplayerDraftStore.getState().resumeHostedDraft({
        config: {
          poolInput: recovered.poolInput,
          kind: recovered.kind,
          podSize: recovered.podSize,
          hostDisplayName: recovered.hostDisplayName,
          tournamentFormat: recovered.tournamentFormat,
          podPolicy: recovered.podPolicy,
          persistenceId: recovered.persistenceId,
          preferredRoomCode: recovered.roomCode,
          recovery: { session: recovered, raw: recovered },
        },
        isCurrent: () => true,
      });
      return { outcome, initialize, installedPeer };
    }

    it("reports a refused recovery as incompatible before it builds a host or touches the installed one", async () => {
      checkRestorableSession.mockResolvedValueOnce({ type: "Incompatible" });

      const { outcome, initialize, installedPeer } = await recoverBesideInstalledHost();

      expect(outcome).toBe("incompatible");
      expect(initialize).not.toHaveBeenCalled();
      expect(hostRoom).toHaveBeenCalledOnce();
      expect(installedPeer.destroy).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: "installed-owner" });
      expect(checkRestorableSession).toHaveBeenCalledWith(recovered.draftSessionJson);
    });

    it("reports an unanswered check as an initialization failure before it builds a host or touches the installed one", async () => {
      checkRestorableSession.mockRejectedValueOnce(new Error("draft engine unavailable"));

      const { outcome, initialize, installedPeer } = await recoverBesideInstalledHost();

      expect(outcome).toBe("initializationFailure");
      expect(initialize).not.toHaveBeenCalled();
      expect(hostRoom).toHaveBeenCalledOnce();
      expect(installedPeer.destroy).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: "installed-owner" });
      expect(checkRestorableSession).toHaveBeenCalledWith(recovered.draftSessionJson);
    });
  });

  describe("a recovery whose saved draft session meets an installed host's draft engine session", () => {
    beforeEach(() => {
      engine.session = null;
      engine.imports = [];
      engine.atImport = null;
      engine.importFailure = null;
      engine.afterImport = null;
      checkRestorableSession.mockResolvedValue({ type: "Admitted" });
    });

    afterEach(() => {
      vi.restoreAllMocks();
    });

    function recover(options: { isCurrent?: () => boolean; signal?: AbortSignal } = {}) {
      return useMultiplayerDraftStore.getState().resumeHostedDraft({
        config: {
          poolInput: recoveredHost.poolInput,
          kind: recoveredHost.kind,
          podSize: recoveredHost.podSize,
          hostDisplayName: recoveredHost.hostDisplayName,
          tournamentFormat: recoveredHost.tournamentFormat,
          podPolicy: recoveredHost.podPolicy,
          persistenceId: recoveredHost.persistenceId,
          preferredRoomCode: recoveredHost.roomCode,
          signal: options.signal,
          recovery: { session: recoveredHost, raw: recoveredHost },
        },
        isCurrent: options.isCurrent ?? (() => true),
        routeSignal: new AbortController().signal,
      });
    }

    /** The installed host and whether the engine's session is that host's own. */
    function installedOwner() {
      const { role, hostPersistenceId } = useMultiplayerDraftStore.getState();
      return {
        role,
        onOwnSession: hostPersistenceId !== null && engine.session === ownSession[hostPersistenceId],
      };
    }

    it("never lets the installed host save the recovering host's draft session", async () => {
      const { saveA } = await startInstalledHost();
      const held = holdAfterImport();
      const outcome = recover();
      await held.reached;
      // Reach: B's restore reached the import with B's exact saved session.
      expect(engine.imports).toEqual([recoveredSession]);

      await saveA();
      held.release();
      await outcome;

      expect(savedUnder("installed-owner")).not.toContain(recoveredSession);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: "recovered-b" });
    });

    it("leaves the installed host on its own draft session when the recovery is superseded after its import", async () => {
      await startInstalledHost();
      let current = true;
      const held = holdAfterImport();
      const outcome = recover({ isCurrent: () => current });
      await held.reached;
      // Reach: B's restore reached the import with B's exact saved session.
      expect(engine.imports).toEqual([recoveredSession]);

      current = false;
      held.release();
      await outcome;

      expect(installedOwner()).toEqual({ role: "host", onOwnSession: true });
    });

    it("leaves the installed host untouched when the recovery ends during its peer allocation", async () => {
      await startInstalledHost();
      let releaseRoom!: () => void;
      hostRoom.mockImplementationOnce(() => new Promise((resolve) => {
        releaseRoom = () => resolve({
          roomCode: "BBBBB",
          peerId: "host-BBBBB-held",
          peer: { destroy: vi.fn() } as never,
          onGuestConnected: vi.fn(() => vi.fn()),
          destroy: vi.fn(),
        });
      }));
      let current = true;
      const outcome = recover({ isCurrent: () => current });
      for (let turn = 0; turn < 50 && hostRoom.mock.calls.length < 2; turn++) {
        await new Promise((resolve) => setTimeout(resolve, 0));
      }
      // Reach: the engine check answered for B's exact saved session.
      expect(checkRestorableSession).toHaveBeenCalledWith(recoveredSession);
      expect(hostRoom).toHaveBeenCalledTimes(2);

      current = false;
      releaseRoom();

      await expect(outcome).resolves.toBe("superseded");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: "installed-owner" });
      expect(engine.session).toBe(installedSession);
      expect(engine.imports).toEqual([]);
    });

    it.each([
      ["an error", () => new Error("Invalid draft snapshot: restore refused"), "initializationFailure"],
      ["a compatibility refusal", incompatibleRestore, "incompatible"],
    ] as const)("ends a recovering host whose restore fails with %s and keeps both saved records", async (_label, failure, expected) => {
      const { saveA } = await startInstalledHost();
      engine.importFailure = failure();

      const outcome = await recover();

      // Reach: B's exact saved session reached the import.
      expect(engine.imports).toEqual([recoveredSession]);
      expect(outcome).toBe(expected);
      expect(useMultiplayerDraftStore.getState().hostPersistenceId).not.toBe("recovered-b");
      expect(clearDraftHostSession).not.toHaveBeenCalled();
      await saveA();
      expect(savedUnder("installed-owner")).not.toContain(recoveredSession);
    });

    it("leaves the installed host on its own draft session when the recovery attempt is aborted after its import", async () => {
      await startInstalledHost();
      const attempt = new AbortController();
      const held = holdAfterImport();
      const outcome = recover({ signal: attempt.signal });
      await held.reached;
      // Reach: B's restore reached the import with B's exact saved session.
      expect(engine.imports).toEqual([recoveredSession]);

      attempt.abort();
      held.release();
      await outcome;

      expect(installedOwner()).toEqual({ role: "host", onOwnSession: true });
    });
  });

  describe("the pod store's recovery of a saved host whose record carries a draft session", () => {
    const savedPod = {
      id: "recovered-b",
      roomCode: "BBBBB",
      kind: "Premier" as const,
      podSize: 8,
      hostDisplayName: "Host",
      tournamentFormat: "Swiss" as const,
      podPolicy: "Competitive" as const,
      phase: "drafting" as const,
      pickCount: 0,
    };
    const savedAt = Date.now() - 60_000;
    const lifecycleOutcomes: string[] = [];
    const roomRequests: (string | undefined)[] = [];
    let roomGate: Promise<void> | null = null;
    let roomFailure: Error | null = null;
    let startFailure: Error | null = null;
    let lifecycleResume: ReturnType<typeof useMultiplayerDraftStore.getState>["resumeHostedDraft"];

    beforeEach(async () => {
      const actual = await vi.importActual<typeof import("../../services/draftPersistence")>("../../services/draftPersistence");
      // These rows run the real saved pod record writer.
      saveActiveDraftPod.mockImplementation(actual.saveActiveDraftPod);
      localStorage.clear();
      useDraftPodStore.getState().reset();
      engine.session = null;
      engine.imports = [];
      engine.atImport = null;
      engine.importFailure = null;
      engine.afterImport = null;
      roomRequests.length = 0;
      roomGate = null;
      roomFailure = null;
      startFailure = null;
      checkRestorableSession.mockResolvedValue({ type: "Admitted" });
      loadDraftHostSession.mockImplementation(async (...args: unknown[]) => (
        args[0] === recoveredHost.persistenceId
          ? { type: "admitted", session: recoveredHost, raw: recoveredHost }
          : { type: "absent" }
      ));
      hostRoom.mockImplementation(async (_signal: AbortSignal | undefined, options: { preferredRoomCode?: string } = {}) => {
        roomRequests.push(options.preferredRoomCode);
        if (roomGate) await roomGate;
        if (roomFailure) throw roomFailure;
        const roomCode = options.preferredRoomCode ?? "ABCDE";
        return {
          roomCode,
          peerId: `host-${roomCode}`,
          peer: { destroy: vi.fn() } as never,
          onGuestConnected: vi.fn(() => {
            if (startFailure) throw startFailure;
            return vi.fn();
          }),
          destroy: vi.fn(),
        };
      });
      joinRoom.mockImplementation(async () => ({
        conn: { peer: "host-ABCDE" },
        peer: { id: "guest-peer", destroy: vi.fn() },
        closeConn: vi.fn(),
        destroyPeer: vi.fn(),
      }));
      // B's saved pod record, written one minute ago.
      saveActiveDraftPod({ ...savedPod, updatedAt: savedAt });
      lifecycleOutcomes.length = 0;
      lifecycleResume = useMultiplayerDraftStore.getState().resumeHostedDraft;
      useMultiplayerDraftStore.setState({
        resumeHostedDraft: async (attempt) => {
          const outcome = await lifecycleResume(attempt);
          lifecycleOutcomes.push(outcome);
          return outcome;
        },
      });
    });

    afterEach(() => {
      useMultiplayerDraftStore.setState({ resumeHostedDraft: lifecycleResume });
      useConnectivityStore.getState().setForcedOffline(false);
      saveActiveDraftPod.mockReset();
      loadDraftHostSession.mockReset();
      vi.restoreAllMocks();
      localStorage.clear();
    });

    /** What the pod page reads after a recovery on one route. */
    function afterRecovery(routeToken: number) {
      const pod = useDraftPodStore.getState();
      const record = inspectActiveDraftPod();
      return {
        failureKind: pod.hostRecoveryFailure?.kind ?? null,
        failureCurrent: isHostedPodRecoveryFailureCurrent(pod.hostRecoveryFailure, routeToken),
        configError: pod.configError,
        recordNames: record.type === "present" ? record.capture.id : record.type,
      };
    }

    it.each([
      [
        "the import rejects",
        () => {
          engine.importFailure = new Error("Invalid draft snapshot: restore refused");
        },
        "initializationFailure",
        "podError.connection",
      ],
      [
        "the import rejects as incompatible",
        () => {
          engine.importFailure = incompatibleRestore();
        },
        "incompatible",
        "podError.incompatible",
      ],
      [
        "the host start fails after the restore",
        () => {
          startFailure = new Error("Host start failed");
        },
        "initializationFailure",
        "podError.connection",
      ],
    ] as const)("reports a failure after the handover, with its retry, when %s", async (_label, arrange, kind, configError) => {
      arrange();

      const outcome = await useDraftPodStore.getState().resumeHostedPod({ entry: "host", routeToken: 1 });

      // Reach: B's exact saved session reached the import.
      expect(engine.imports).toEqual([recoveredSession]);
      expect(outcome).toBe(kind);
      expect(lifecycleOutcomes).toEqual([kind]);
      expect(afterRecovery(1)).toEqual({ failureKind: kind, failureCurrent: true, configError, recordNames: "recovered-b" });
      expect(useMultiplayerDraftStore.getState().hostPersistenceId).not.toBe("recovered-b");
      expect(clearDraftHostSession).not.toHaveBeenCalled();
    });

    it("reports an offline recovery whose import rejects after the handover without a failure", async () => {
      engine.atImport = () => useConnectivityStore.getState().setForcedOffline(true);
      engine.importFailure = new Error("Invalid draft snapshot: restore refused");

      const outcome = await useDraftPodStore.getState().resumeHostedPod({ entry: "host", routeToken: 2 });

      // Reach: B's exact saved session reached the import.
      expect(engine.imports).toEqual([recoveredSession]);
      expect(outcome).toBe("offline");
      expect(lifecycleOutcomes).toEqual(["offline"]);
      expect(afterRecovery(2)).toEqual({
        failureKind: null,
        failureCurrent: false,
        configError: DRAFT_OFFLINE_ERROR,
        recordNames: "recovered-b",
      });
      expect(useMultiplayerDraftStore.getState().hostPersistenceId).not.toBe("recovered-b");
      expect(clearDraftHostSession).not.toHaveBeenCalled();
    });

    it.each(["an installed host", "a held guest seat"] as const)("reports a failure after the handover, with its retry, beside %s", async (owner) => {
      let saveA: (() => Promise<void>) | null = null;
      if (owner === "an installed host") {
        ({ saveA } = await startInstalledHost());
      } else {
        await expect(useMultiplayerDraftStore.getState().joinDraft({
          kind: "new",
          roomCode: "ABCDE",
          displayName: "Alice",
        })).resolves.toEqual({ status: "opened" });
      }
      // Reach: the owner is installed.
      expect(useMultiplayerDraftStore.getState().role).toBe(owner === "an installed host" ? "host" : "guest");
      // Another tab records B's saved pod after the owner's own.
      saveActiveDraftPod({ ...savedPod, updatedAt: savedAt });
      engine.importFailure = new Error("Invalid draft snapshot: restore refused");

      const outcome = await useDraftPodStore.getState().resumeHostedPod({ entry: "host", routeToken: 3 });

      // Reach: B's exact saved session reached the import.
      expect(engine.imports).toEqual([recoveredSession]);
      expect(outcome).toBe("initializationFailure");
      expect(lifecycleOutcomes).toEqual(["initializationFailure"]);
      expect(afterRecovery(3)).toEqual({
        failureKind: "initializationFailure",
        failureCurrent: true,
        configError: "podError.connection",
        recordNames: "recovered-b",
      });
      expect(useMultiplayerDraftStore.getState().hostPersistenceId).not.toBe("recovered-b");
      expect(clearDraftHostSession).not.toHaveBeenCalled();
      if (saveA) {
        await saveA();
        expect(savedUnder("installed-owner")).not.toContain(recoveredSession);
      }
    });

    it("resumes on the retry after a recovery failed past the handover", async () => {
      engine.importFailure = new Error("Invalid draft snapshot: restore refused");
      const first = await useDraftPodStore.getState().resumeHostedPod({ entry: "host", routeToken: 4 });
      // Reach: B's exact saved session reached the import.
      expect(engine.imports).toEqual([recoveredSession]);
      expect(first).toBe("initializationFailure");
      const failure = useDraftPodStore.getState().hostRecoveryFailure;
      // Reach: the failure was current, and no setup was published, before the retry.
      expect(isHostedPodRecoveryFailureCurrent(failure, 4)).toBe(true);
      expect(useDraftPodStore.getState().config.setCode).toBe("");
      engine.importFailure = null;

      // The page retries on a new route token, naming the failure it shows.
      const outcome = await useDraftPodStore.getState().resumeHostedPod({
        routeToken: 5,
        expectedFailure: failure ?? undefined,
      });

      expect(outcome).toBe("resumed");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: "recovered-b" });
      expect(useDraftPodStore.getState()).toMatchObject({ hostRecoveryFailure: null, config: { setCode: "TST" } });
      expect(engine.imports).toEqual([recoveredSession, recoveredSession]);
    });

    it.each(["released", "rejected"] as const)("supersedes a recovery whose saved pod record is rewritten before the handover, the peer then %s", async (end) => {
      let releaseRoom!: () => void;
      roomGate = new Promise<void>((resolve) => {
        releaseRoom = resolve;
      });
      if (end === "rejected") roomFailure = new Error("Peer unavailable");
      const pending = useDraftPodStore.getState().resumeHostedPod({ entry: "host", routeToken: 6 });
      for (let turn = 0; turn < 50 && roomRequests.length === 0; turn++) {
        await new Promise((resolve) => setTimeout(resolve, 0));
      }
      // Reach: the peer was requested for B's room code.
      expect(roomRequests).toEqual(["BBBBB"]);

      // The same pod's record, written again with a later update time.
      saveActiveDraftPod({ ...savedPod, updatedAt: Date.now() });
      releaseRoom();
      const outcome = await pending;

      expect(outcome).toBe("superseded");
      expect(engine.imports).toEqual([]);
      expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
      expect(useMultiplayerDraftStore.getState().role).toBeNull();
    });
  });
});
