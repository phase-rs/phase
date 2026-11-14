import "fake-indexeddb/auto";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { clear as idbClear } from "idb-keyval";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";
import type { RestorableDraftSessionVerdict } from "../../adapter/draft-adapter";
import type { ActiveDraftPodMeta, PersistedDraftHostSession } from "../../services/draftPersistence";

/**
 * Drives host recovery through the real page, pod store, lifecycle store,
 * host wrapper and saved pod storage: the active pod record lives in
 * localStorage and the host session in IndexedDB. Only the peer, the
 * engine-backed host, the guest seat, the procedure loader and the page chrome are stubbed.
 */
const mocks = vi.hoisted(() => ({
  draftProcedure: vi.fn(),
  checkRestorableSession: vi.fn(),
  hostRoom: vi.fn(),
  restoreRefusal: null as string | null,
  hosts: [] as Array<{
    dispose: ReturnType<typeof vi.fn>;
    startDraft: ReturnType<typeof vi.fn>;
  }>,
}));

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  hostRoom: mocks.hostRoom,
  joinRoom: vi.fn(async () => ({
    conn: { peer: "held-host-peer" },
    peer: { id: "guest-peer", destroy: vi.fn() },
    closeConn: vi.fn(),
    destroyPeer: vi.fn(),
  })),
}));

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

vi.mock("../../adapter/p2p-draft-host", () => ({
  P2PDraftHost: class {
    readonly dispose = vi.fn(async () => {});
    readonly startDraft = vi.fn(async () => {});
    readonly onEvent = vi.fn(() => vi.fn());

    constructor() { mocks.hosts.push(this); }

    async initialize() {}
    async restoreFromPersisted() {
      // draft-wasm's import throws its refusal text as a string.
      if (mocks.restoreRefusal !== null) throw mocks.restoreRefusal;
      return null;
    }
    getHostWorkspaceState() { return null; }
    async terminateDraft() { await this.dispose(); }
  },
}));

vi.mock("../../adapter/draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter")>()),
  DraftAdapter: class {
    draftProcedure = mocks.draftProcedure;
    checkRestorableSession = mocks.checkRestorableSession;
  },
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/menu/MenuShell", () => ({
  MenuShell: ({ children }: { children: ReactNode }) => <>{children}</>,
}));
vi.mock("../../components/draft/HostControls", () => {
  const empty: readonly [] = [];
  return { HostControls: () => null, useHostDraftTopActions: () => empty };
});
vi.mock("../../components/draft/SetSelector", () => ({ SetSelector: () => null }));

import { DraftPodPage } from "../DraftPodPage";
import { isHostedPodRecoveryInFlight, useDraftPodStore } from "../../stores/draftPodStore";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { useConnectivityStore } from "../../stores/connectivityStore";
import {
  getDraftStore,
  inspectActiveDraftPod,
  loadDraftHostSession,
  saveActiveDraftPod,
  saveDraftHostSession,
} from "../../services/draftPersistence";

const savedPod: PersistedDraftHostSession = {
  persistenceId: "owner-b",
  roomCode: "BBBBB",
  kind: "Premier",
  podSize: 4,
  hostDisplayName: "Host",
  tournamentFormat: "Swiss",
  podPolicy: "Competitive",
  seatTokens: { 0: "host" },
  seatNames: { 0: "Host" },
  kickedTokens: [],
  draftStarted: false,
  draftCode: "BBBBB",
  draftSessionJson: null,
  poolInput: { type: "Set", data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
};

let seededAt = 0;

function savedRecord(overrides: Partial<ActiveDraftPodMeta> = {}): ActiveDraftPodMeta {
  return {
    id: savedPod.persistenceId,
    roomCode: savedPod.roomCode,
    kind: savedPod.kind,
    podSize: savedPod.podSize,
    hostDisplayName: savedPod.hostDisplayName,
    tournamentFormat: savedPod.tournamentFormat,
    podPolicy: savedPod.podPolicy,
    phase: "lobby",
    pickCount: 0,
    updatedAt: seededAt,
    ...overrides,
  };
}

function peerFor(roomCode: string) {
  return {
    roomCode,
    peerId: `peer-${roomCode}`,
    peer: { destroy: vi.fn() },
    onGuestConnected: vi.fn(() => vi.fn()),
    destroy: vi.fn(),
  };
}

function renderAt(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <DraftPodPage />
    </MemoryRouter>,
  );
}

/**
 * A saved draft session from the fixture draft-wasm's own test checks:
 * draft-core's serialization of a session, and the draft engine's answer to the
 * saved-session check.
 */
function savedSession(name: string): { text: string; verdict: RestorableDraftSessionVerdict } {
  const fixture = JSON.parse(readFileSync(resolve(
    dirname(fileURLToPath(import.meta.url)),
    "../../../../fixtures/draft-contract/fifo_legacy_admission.json",
  ), "utf8")) as { saved_sessions: Record<string, { verdict: RestorableDraftSessionVerdict; session: unknown }> };
  const saved = fixture.saved_sessions[name];
  if (!saved) throw new Error(`The fixture has no saved session named ${name}`);
  return { text: JSON.stringify(saved.session), verdict: saved.verdict };
}

describe("DraftPodPage host recovery on the saved pod storage", () => {
  beforeEach(async () => {
    vi.clearAllMocks();
    await useMultiplayerDraftStore.getState().leave(true);
    useMultiplayerDraftStore.getState().reset();
    useDraftPodStore.getState().reset();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    mocks.hosts.length = 0;
    mocks.restoreRefusal = null;
    mocks.checkRestorableSession.mockReset().mockResolvedValue({ type: "Admitted" });
    mocks.hostRoom.mockReset().mockImplementation(async () => peerFor(savedPod.roomCode));
    mocks.draftProcedure.mockResolvedValue(draftProcedureFixture({
      allowed_pod_sizes: [3, 4, 5, 6, 7, 8],
      distribution: "PickAndPass",
    }));
    localStorage.clear();
    await idbClear(getDraftStore());
    await saveDraftHostSession(savedPod.persistenceId, savedPod);
    // The record was saved a minute before this recovery reads it.
    seededAt = Date.now() - 60_000;
    saveActiveDraftPod(savedRecord());
  });

  afterEach(async () => {
    cleanup();
    await useMultiplayerDraftStore.getState().leave(true);
    localStorage.clear();
  });

  it("publishes the recovered setup on entry and lets its lobby start the draft", async () => {
    const user = userEvent.setup();
    renderAt("/draft-pod?entry=host");

    await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBe("host"));
    expect(mocks.hostRoom).toHaveBeenCalledOnce();
    expect(mocks.hostRoom).toHaveBeenLastCalledWith(expect.anything(), { preferredRoomCode: savedPod.roomCode });
    expect(useMultiplayerDraftStore.getState().hostPersistenceId).toBe(savedPod.persistenceId);
    expect(mocks.hosts).toHaveLength(1);
    const stored = inspectActiveDraftPod();
    expect(stored).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId, roomCode: savedPod.roomCode } });
    expect(stored.type === "present" ? stored.capture.updatedAt : null).toBeGreaterThan(seededAt);

    await waitFor(() => expect(useDraftPodStore.getState().procedureCacheKey).toEqual({
      kind: "Premier",
      tournamentFormat: "Swiss",
    }));
    expect(useDraftPodStore.getState().config).toMatchObject({
      setCode: "TST",
      packs: [{ code: "TST", name: "TST" }],
      podSize: 4,
    });
    expect(useDraftPodStore.getState().allowedPodSizes).toEqual([3, 4, 5, 6, 7, 8]);
    expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
    const start = await screen.findByRole("button", { name: "Start Draft" });
    expect(start).toBeEnabled();

    await user.click(start);

    await waitFor(() => expect(mocks.hosts[0]?.startDraft).toHaveBeenCalledOnce());
    expect(mocks.hosts[0]?.startDraft).toHaveBeenCalledWith(true);
  });

  it("publishes the recovered setup after Try Reconnecting", async () => {
    mocks.hostRoom.mockRejectedValueOnce(new Error("peer server unavailable"));
    const user = userEvent.setup();
    renderAt("/draft-pod?entry=host");
    const retry = await screen.findByRole("button", { name: "Try Reconnecting" });
    expect(useDraftPodStore.getState().hostRecoveryFailure?.kind).toBe("initializationFailure");

    await user.click(retry);

    await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBe("host"));
    expect(mocks.hostRoom).toHaveBeenCalledTimes(2);
    expect(mocks.hostRoom).toHaveBeenLastCalledWith(expect.anything(), { preferredRoomCode: savedPod.roomCode });
    expect(useMultiplayerDraftStore.getState().hostPersistenceId).toBe(savedPod.persistenceId);
    const stored = inspectActiveDraftPod();
    expect(stored).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId, roomCode: savedPod.roomCode } });
    expect(stored.type === "present" ? stored.capture.updatedAt : null).toBeGreaterThan(seededAt);

    await waitFor(() => expect(useDraftPodStore.getState().procedureCacheKey).toEqual({
      kind: "Premier",
      tournamentFormat: "Swiss",
    }));
    expect(useDraftPodStore.getState().config).toMatchObject({
      setCode: "TST",
      packs: [{ code: "TST", name: "TST" }],
      podSize: 4,
    });
    expect(useDraftPodStore.getState().allowedPodSizes).toEqual([3, 4, 5, 6, 7, 8]);
    expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
    expect(screen.queryByRole("button", { name: "Try Reconnecting" })).toBeNull();
  });

  it("ends the host Try Reconnecting recovered when its page route ends", async () => {
    mocks.hostRoom.mockRejectedValueOnce(new Error("peer server unavailable"));
    const user = userEvent.setup();
    const rendered = renderAt("/draft-pod?entry=host");
    await user.click(await screen.findByRole("button", { name: "Try Reconnecting" }));

    await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBe("host"));
    expect(mocks.hostRoom).toHaveBeenCalledTimes(2);
    expect(mocks.hostRoom).toHaveBeenLastCalledWith(expect.anything(), { preferredRoomCode: savedPod.roomCode });
    expect(useMultiplayerDraftStore.getState().hostPersistenceId).toBe(savedPod.persistenceId);
    expect(mocks.hosts).toHaveLength(1);
    const stored = inspectActiveDraftPod();
    expect(stored).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId, roomCode: savedPod.roomCode } });
    expect(stored.type === "present" ? stored.capture.updatedAt : null).toBeGreaterThan(seededAt);
    // The recovery has settled, so the page has kept its retry.
    await waitFor(() => expect(isHostedPodRecoveryInFlight()).toBe(false));

    rendered.unmount();

    await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBeNull());
    await waitFor(() => expect(mocks.hosts[0]?.dispose).toHaveBeenCalled());
  });

  it("ends the host an entry recovered when its page route ends", async () => {
    const rendered = renderAt("/draft-pod?entry=host");

    await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBe("host"));
    expect(mocks.hostRoom).toHaveBeenCalledOnce();
    expect(mocks.hostRoom).toHaveBeenLastCalledWith(expect.anything(), { preferredRoomCode: savedPod.roomCode });
    expect(useMultiplayerDraftStore.getState().hostPersistenceId).toBe(savedPod.persistenceId);
    expect(mocks.hosts).toHaveLength(1);
    const stored = inspectActiveDraftPod();
    expect(stored).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId, roomCode: savedPod.roomCode } });
    expect(stored.type === "present" ? stored.capture.updatedAt : null).toBeGreaterThan(seededAt);
    await waitFor(() => expect(isHostedPodRecoveryInFlight()).toBe(false));

    rendered.unmount();

    await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBeNull());
    await waitFor(() => expect(mocks.hosts[0]?.dispose).toHaveBeenCalled());
  });

  it("publishes the recovered setup when host entry switches from a held guest seat", async () => {
    await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    expect(useMultiplayerDraftStore.getState().role).toBe("guest");

    renderAt("/draft-pod?entry=host");

    await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBe("host"));
    expect(mocks.hostRoom).toHaveBeenCalledOnce();
    expect(mocks.hostRoom).toHaveBeenLastCalledWith(expect.anything(), { preferredRoomCode: savedPod.roomCode });
    expect(useMultiplayerDraftStore.getState().hostPersistenceId).toBe(savedPod.persistenceId);
    const stored = inspectActiveDraftPod();
    expect(stored).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId, roomCode: savedPod.roomCode } });
    expect(stored.type === "present" ? stored.capture.updatedAt : null).toBeGreaterThan(seededAt);

    await waitFor(() => expect(useDraftPodStore.getState().procedureCacheKey).toEqual({
      kind: "Premier",
      tournamentFormat: "Swiss",
    }));
    expect(useDraftPodStore.getState().config).toMatchObject({
      setCode: "TST",
      packs: [{ code: "TST", name: "TST" }],
      podSize: 4,
    });
  });

  it.each([
    ["another pod's record", { id: "owner-c" }],
    ["a newer save of the same pod's record", {}],
  ] as const)("does not take over when %s is saved before the handoff", async (_record, overrides) => {
    let releasePeer!: () => void;
    const pending = peerFor(savedPod.roomCode);
    mocks.hostRoom.mockImplementationOnce(() => new Promise((resolve) => {
      releasePeer = () => resolve(pending);
    }));
    renderAt("/draft-pod?entry=host");
    await waitFor(() => expect(mocks.hostRoom).toHaveBeenCalledOnce());
    expect(mocks.hostRoom).toHaveBeenLastCalledWith(expect.anything(), { preferredRoomCode: savedPod.roomCode });
    expect(isHostedPodRecoveryInFlight()).toBe(true);
    const replacement = savedRecord({ ...overrides, updatedAt: Date.now() });
    saveActiveDraftPod(replacement);

    releasePeer();

    await waitFor(() => expect(isHostedPodRecoveryInFlight()).toBe(false));
    expect(pending.destroy).toHaveBeenCalledOnce();
    expect(useMultiplayerDraftStore.getState().role).toBeNull();
    expect(mocks.hosts).toHaveLength(0);
    expect(useDraftPodStore.getState().procedureCacheKey).toBeNull();
    expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
    expect(inspectActiveDraftPod()).toMatchObject({
      type: "present",
      capture: { id: replacement.id, roomCode: replacement.roomCode, updatedAt: replacement.updatedAt },
    });
  });

  describe("with a started saved pod whose session the draft engine checks", () => {
    // The client's top-level check admits this session; the engine decides it.
    const saved = { ...savedPod, draftStarted: true, draftSessionJson: JSON.stringify({ status: "Drafting", config: { rng_seed: 7 } }) };

    beforeEach(async () => {
      await saveDraftHostSession(savedPod.persistenceId, saved);
      saveActiveDraftPod(savedRecord({ phase: "drafting" }));
      // draft-wasm's import throws its refusal text as a string.
      mocks.restoreRefusal = "FIFO drafting is not supported by this runtime";
    });

    it("reports the pod as incompatible before any peer when the engine refuses its session", async () => {
      mocks.checkRestorableSession.mockResolvedValue({ type: "Incompatible" });

      renderAt("/draft-pod?entry=host");

      // Reach: the recovery settled with a recorded failure for the saved pod.
      await waitFor(() => expect(useDraftPodStore.getState().hostRecoveryFailure).not.toBeNull());
      expect(useDraftPodStore.getState().hostRecoveryFailure?.capture).toMatchObject({ id: savedPod.persistenceId });
      expect(useDraftPodStore.getState().hostRecoveryFailure?.kind).toBe("incompatible");
      expect(useDraftPodStore.getState().configError).toBe("podError.incompatible");
      expect(await screen.findByText("This draft is incompatible with your protocol version.")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Try Reconnecting" })).toBeEnabled();
      expect(mocks.hostRoom).not.toHaveBeenCalled();
      expect(mocks.hosts).toHaveLength(0);
      expect(mocks.checkRestorableSession).toHaveBeenCalledWith(saved.draftSessionJson);
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      await expect(loadDraftHostSession(savedPod.persistenceId)).resolves.toMatchObject({ type: "admitted", raw: saved });
    });

    it("ends the recovery as a connection failure before any peer when the engine cannot answer", async () => {
      mocks.checkRestorableSession.mockRejectedValue(new Error("draft engine unavailable"));

      renderAt("/draft-pod?entry=host");

      // Reach: the recovery settled with a recorded failure for the saved pod.
      await waitFor(() => expect(useDraftPodStore.getState().hostRecoveryFailure).not.toBeNull());
      expect(useDraftPodStore.getState().hostRecoveryFailure?.capture).toMatchObject({ id: savedPod.persistenceId });
      expect(useDraftPodStore.getState().hostRecoveryFailure?.kind).toBe("initializationFailure");
      expect(useDraftPodStore.getState().configError).toBe("podError.connection");
      expect(mocks.hostRoom).not.toHaveBeenCalled();
      expect(mocks.hosts).toHaveLength(0);
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      await expect(loadDraftHostSession(savedPod.persistenceId)).resolves.toMatchObject({ type: "admitted", raw: saved });
    });
  });

  describe("with a started saved pod whose session the draft engine refuses", () => {
    it.each([
      ["a FIFO flow", "fifo_drafting"],
      ["Review", "legacy_review"],
      ["residual FIFO state", "legacy_carrying_fifo"],
    ] as const)("reports a saved pod with %s as incompatible before any peer once the draft engine refuses it", async (_label, name) => {
      const { text, verdict } = savedSession(name);
      const saved = { ...savedPod, draftStarted: true, draftSessionJson: text };
      await saveDraftHostSession(savedPod.persistenceId, saved);
      saveActiveDraftPod(savedRecord({ phase: "drafting" }));
      mocks.checkRestorableSession.mockResolvedValue(verdict);

      renderAt("/draft-pod?entry=host");

      // Reach: the recovery settled with a recorded failure for the saved pod.
      await waitFor(() => expect(useDraftPodStore.getState().hostRecoveryFailure).not.toBeNull());
      expect(useDraftPodStore.getState().hostRecoveryFailure?.capture).toMatchObject({ id: savedPod.persistenceId });
      expect(mocks.checkRestorableSession).toHaveBeenCalledOnce();
      expect(mocks.checkRestorableSession).toHaveBeenCalledWith(text);
      expect(useDraftPodStore.getState().hostRecoveryFailure?.kind).toBe("incompatible");
      expect(useDraftPodStore.getState().configError).toBe("podError.incompatible");
      expect(await screen.findByText("This draft is incompatible with your protocol version.")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Try Reconnecting" })).toBeEnabled();
      expect(mocks.hostRoom).not.toHaveBeenCalled();
      expect(mocks.hosts).toHaveLength(0);
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      await expect(loadDraftHostSession(savedPod.persistenceId)).resolves.toMatchObject({ type: "admitted", raw: saved });
    });
  });
});
