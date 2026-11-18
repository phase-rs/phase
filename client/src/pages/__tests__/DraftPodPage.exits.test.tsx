import "fake-indexeddb/auto";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { clear as idbClear } from "idb-keyval";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { ReactNode } from "react";
import { MemoryRouter, Route, Routes, useLocation, useNavigate, type NavigateFunction } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";
import { FakeDraftDataConnection } from "../../network/__tests__/fakeDraftDataConnection";
import { DRAFT_PROTOCOL_VERSION } from "../../network/draftProtocol";
import { P2PDraftGuest } from "../../adapter/p2p-draft-guest";
import type { ActiveDraftPodMeta, PersistedDraftHostSession } from "../../services/draftPersistence";

/*
 * The pod page's exits, on the real page, pod store, lifecycle store, both
 * wrappers, the real P2PDraftGuest over a fake data connection, real saved-pod
 * storage and real routes. Only the peer, the engine-backed host, the procedure
 * loader and the page chrome's visuals are doubles.
 */
const mocks = vi.hoisted(() => ({
  draftProcedure: vi.fn(),
  checkRestorableSession: vi.fn(),
  loadCardDatabase: vi.fn(async () => {}),
  hostRoom: vi.fn(),
  joinRoom: vi.fn(),
  loadFailure: null as Error | null,
  hostStartFailure: null as Error | null,
  restoreRefusal: null as unknown,
  restoreGate: null as Promise<void> | null,
  terminateGate: null as Promise<void> | null,
  hostStartGate: null as Promise<void> | null,
  revocationGate: null as Promise<void> | null,
  clearSavedHost: null as null | ((id: string) => Promise<void>),
  restoreCalls: 0,
  hosts: [] as Array<{ dispose: ReturnType<typeof vi.fn>; terminateDraft: ReturnType<typeof vi.fn>; id: unknown }>,
}));

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  hostRoom: mocks.hostRoom,
  joinRoom: mocks.joinRoom,
}));

vi.mock("../../adapter/p2p-draft-host", () => ({
  P2PDraftHost: class {
    readonly dispose = vi.fn(async () => {});
    // As the real host's termination does, it clears the saved session.
    readonly terminateDraft = vi.fn(async () => {
      if (mocks.terminateGate) await mocks.terminateGate;
      if (mocks.clearSavedHost && this.id) await mocks.clearSavedHost(this.id as string);
    });
    readonly startDraft = vi.fn(async () => {});
    readonly onEvent = vi.fn(() => vi.fn());
    readonly id: unknown;
    constructor(...args: unknown[]) {
      this.id = args[9];
      mocks.hosts.push(this as never);
    }
    async initialize() {
      if (mocks.hostStartGate) await mocks.hostStartGate;
      if (mocks.hostStartFailure) throw mocks.hostStartFailure;
    }
    async restoreFromPersisted() {
      mocks.restoreCalls += 1;
      if (mocks.restoreGate) await mocks.restoreGate;
      if (mocks.restoreRefusal !== null) throw mocks.restoreRefusal;
      return null;
    }
    getHostWorkspaceState() { return null; }
  },
}));

vi.mock("../../adapter/draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter")>()),
  DraftAdapter: class {
    draftProcedure = mocks.draftProcedure;
    checkRestorableSession = mocks.checkRestorableSession;
    loadCardDatabase = mocks.loadCardDatabase;
  },
}));

vi.mock("../../services/draftPersistence", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../services/draftPersistence")>();
  mocks.clearSavedHost = actual.clearDraftHostSession as never;
  return {
    ...actual,
    loadDraftHostSession: async (id: string) => {
      if (mocks.loadFailure) throw mocks.loadFailure;
      return actual.loadDraftHostSession(id);
    },
    clearDraftGuestRecovery: async (hostPeerId: string) => {
      if (mocks.revocationGate) await mocks.revocationGate;
      return actual.clearDraftGuestRecovery(hostPeerId);
    },
  };
});

vi.mock("../../components/chrome/ScreenChrome", () => ({
  ScreenChrome: ({ onBack }: { onBack?: () => void }) => (onBack ? <button onClick={onBack}>Chrome Back</button> : null),
}));
vi.mock("../../components/menu/MenuShell", () => ({
  MenuShell: ({ children }: { children: ReactNode }) => <>{children}</>,
}));
vi.mock("../../components/draft/HostControls", () => {
  const empty: readonly [] = [];
  return {
    HostControls: ({ endDraftAction }: { endDraftAction: { onClick: () => void } }) => <button onClick={() => endDraftAction.onClick()}>End Draft</button>,
    useHostDraftTopActions: () => empty,
  };
});
vi.mock("../../components/draft/SetSelector", () => ({ SetSelector: () => null }));

import { DraftPodPage } from "../DraftPodPage";
import { useDraftPodStore } from "../../stores/draftPodStore";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { useConnectivityStore } from "../../stores/connectivityStore";
import {
  clearActiveDraftPod,
  getDraftStore,
  inspectActiveDraftGuest,
  inspectActiveDraftPod,
  loadDraftGuestSession,
  loadDraftHostSession,
  saveActiveDraftGuest,
  saveActiveDraftPod,
  saveDraftGuestSession,
  saveDraftHostSession,
} from "../../services/draftPersistence";

const fixturePath = resolve(dirname(fileURLToPath(import.meta.url)), "../../../../fixtures/draft-contract/fifo_legacy_admission.json");
const fixture = JSON.parse(readFileSync(fixturePath, "utf8")) as { queued_view: Record<string, unknown> };
const HOST_PEER_ID = "host-peer-ABCDE";
const LEAVE_ACK_TIMEOUT_MS = 10_000;

function rawMessage(message: Record<string, unknown>): Uint8Array {
  const json = new TextEncoder().encode(JSON.stringify(message));
  const bytes = new Uint8Array(json.length + 1);
  bytes.set(json, 1);
  return bytes;
}

function legacyView(status = "Lobby"): Record<string, unknown> {
  return {
    status,
    kind: "Premier",
    launch_capability: "None",
    distribution: "PickAndPass",
    commanders_required: 0,
    current_pack_number: 0,
    pick_number: 0,
    pass_direction: "Left",
    current_pack: null,
    required_pick_count: 1,
    pick_selection_mode: "Direct",
    pool: [],
    draft_effects: [],
    pool_groups: { creatures: [], non_creatures: [], lands: [] },
    seats: [],
    cards_per_pack: 14,
    pick_steps_per_pack: 14,
    pack_count: 3,
    min_deck_size: 40,
    addable_cards: [],
    timer_remaining_ms: null,
    standings: [],
    current_round: 0,
    next_pairing_round: 1,
    tournament_format: "Swiss",
    pod_policy: "Competitive",
    pairings: [],
    match_config: { match_type: "Bo1" },
  };
}

const welcome = (view: unknown) => ({
  type: "draft_welcome",
  draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
  draftToken: "guest-token",
  seatIndex: 2,
  draftCode: "draft-xyz",
  workspaceState: null,
  view,
});

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
const startedPod: PersistedDraftHostSession = {
  ...savedPod,
  draftStarted: true,
  draftSessionJson: JSON.stringify({ status: "Drafting", config: { rng_seed: 7 } }),
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

function deferred<T = void>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
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

let pathname = "";
let navigateTo: NavigateFunction | null = null;
function Where() {
  const location = useLocation();
  pathname = location.pathname + location.search;
  navigateTo = useNavigate();
  return null;
}

function renderApp(entries: string[]) {
  return render(
    <MemoryRouter initialEntries={entries} initialIndex={entries.length - 1}>
      <Where />
      <Routes>
        <Route path="/draft-pod" element={<DraftPodPage />} />
        <Route path="/" element={<div data-testid="home" />} />
        <Route path="/game/:id" element={<div data-testid="game" />} />
      </Routes>
    </MemoryRouter>,
  );
}

const settle = (ms = 60) => act(async () => { await new Promise((done) => setTimeout(done, ms)); });


const LOBBY_TEXT = "Waiting for host to start the draft...";
const REFUSAL_TEXT = "This draft is incompatible with your protocol version.";
const CONNECTION_TEXT = "Connection Error";

describe("DraftPodPage exits", () => {
  const rejections: string[] = [];
  const onRejection = (reason: unknown) => {
    rejections.push(reason instanceof Error ? reason.message : String(reason));
  };
  let conns: FakeDraftDataConnection[] = [];
  const conn = () => conns[conns.length - 1]!;
  let leaveSpy: ReturnType<typeof vi.spyOn>;
  let disposeSpy: ReturnType<typeof vi.spyOn>;
  let timerSpy: ReturnType<typeof vi.spyOn>;
  const openedRoom = () => {
    const opened = new FakeDraftDataConnection();
    (opened as unknown as { peer: string }).peer = HOST_PEER_ID;
    conns.push(opened);
    return {
      conn: opened,
      peer: {
        id: "guest-peer",
        destroy: vi.fn(),
        connect: () => {
          const dialed = new FakeDraftDataConnection();
          dialed.open = false;
          (dialed as unknown as { peer: string }).peer = HOST_PEER_ID;
          conns.push(dialed);
          return dialed;
        },
        on: vi.fn(),
        off: vi.fn(),
      },
      closeConn: vi.fn(),
      destroyPeer: vi.fn(),
    };
  };

  beforeEach(async () => {
    vi.clearAllMocks();
    rejections.length = 0;
    process.on("unhandledRejection", onRejection);
    await useMultiplayerDraftStore.getState().leave(true).catch(() => undefined);
    useMultiplayerDraftStore.getState().reset();
    useDraftPodStore.getState().reset();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    mocks.hosts.length = 0;
    mocks.loadFailure = null;
    mocks.hostStartFailure = null;
    mocks.restoreRefusal = null;
    mocks.restoreGate = null;
    mocks.terminateGate = null;
    mocks.hostStartGate = null;
    mocks.revocationGate = null;
    mocks.restoreCalls = 0;
    mocks.checkRestorableSession.mockReset().mockResolvedValue({ type: "Admitted" });
    mocks.hostRoom.mockReset().mockImplementation(async () => peerFor(savedPod.roomCode));
    mocks.draftProcedure.mockReset().mockResolvedValue(draftProcedureFixture({ allowed_pod_sizes: [3, 4, 5, 6, 7, 8], distribution: "PickAndPass" }));
    localStorage.clear();
    await idbClear(getDraftStore());
    conns = [];
    mocks.joinRoom.mockReset().mockImplementation(async () => openedRoom());
    leaveSpy = vi.spyOn(P2PDraftGuest.prototype, "leave");
    disposeSpy = vi.spyOn(P2PDraftGuest.prototype, "dispose");
    timerSpy = vi.spyOn(globalThis, "setTimeout");
    seededAt = Date.now() - 60_000;
  });

  afterEach(async () => {
    cleanup();
    mocks.terminateGate = null;
    mocks.hostStartGate = null;
    mocks.revocationGate = null;
    leaveSpy.mockRestore();
    disposeSpy.mockRestore();
    await useMultiplayerDraftStore.getState().leave(true).catch(() => undefined);
    await settle(20);
    // A leave no host answered keeps its 10 s timer: it ends with the row that sent it.
    for (const [index, call] of timerSpy.mock.calls.entries()) {
      if (call[1] === LEAVE_ACK_TIMEOUT_MS) clearTimeout(timerSpy.mock.results[index]!.value as ReturnType<typeof setTimeout>);
    }
    timerSpy.mockRestore();
    process.off("unhandledRejection", onRejection);
    localStorage.clear();
    navigateTo = null;
  });

  async function seedSavedSeat() {
    await saveDraftGuestSession(HOST_PEER_ID, { draftToken: "guest-token", seatIndex: 2, draftCode: "draft-xyz", roomCode: "ABCDE", displayName: "Alice" });
    saveActiveDraftGuest({ roomCode: "ABCDE", displayName: "Alice", hostPeerId: HOST_PEER_ID });
  }

  async function seedSavedPod(session: PersistedDraftHostSession = savedPod) {
    await saveDraftHostSession(session.persistenceId, session);
    saveActiveDraftPod(savedRecord(session.draftStarted ? { phase: "drafting" } : {}));
  }

  async function openGuest(view: Record<string, unknown> = legacyView()) {
    const joined = useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    await vi.waitFor(() => expect(conns.length).toBeGreaterThan(0));
    await vi.waitFor(() => expect(conn().sentRaw.length).toBeGreaterThan(0));
    await conn().receiveRaw(rawMessage(welcome(view)));
    await expect(joined).resolves.toMatchObject({ status: "opened" });
  }

  async function openRunningHost() {
    mocks.hostRoom.mockImplementationOnce(async () => peerFor("AAAAA"));
    await useMultiplayerDraftStore.getState().hostDraft({
      poolInput: savedPod.poolInput,
      kind: savedPod.kind,
      podSize: 6,
      hostDisplayName: "Host A",
      tournamentFormat: savedPod.tournamentFormat,
      podPolicy: savedPod.podPolicy,
      persistenceId: "owner-a",
    } as never);
    expect(mocks.hostRoom).toHaveBeenCalledOnce();
    expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", phase: "lobby", hostPersistenceId: "owner-a" });
    return mocks.hosts[0]!;
  }

  const failPeer = () => { mocks.hostRoom.mockReset().mockRejectedValue(new Error("unavailable-id")); };
  const clickMenu = async () => {
    await userEvent.setup().click(await screen.findByRole("button", { name: "Return to Menu" }));
    await settle(80);
  };
  const expectSeatKept = async () => {
    const locator = inspectActiveDraftGuest();
    expect(locator.type).toBe("present");
    if (locator.type !== "present") return;
    await expect(loadDraftGuestSession(locator.meta.hostPeerId, locator.meta)).resolves.toMatchObject({ draftToken: "guest-token" });
  };

  describe("a failed host recovery's exit", () => {
    const failures: Array<[string, () => void, PersistedDraftHostSession]> = [
      ["a saved pod read failure", () => { mocks.loadFailure = new Error("read failed"); }, savedPod],
      ["the draft engine refusing the saved session", () => { mocks.checkRestorableSession.mockResolvedValue({ type: "Incompatible" }); }, startedPod],
      ["a check that cannot answer", () => { mocks.checkRestorableSession.mockRejectedValue(new Error("no engine")); }, startedPod],
      ["a procedure load failure", () => { mocks.draftProcedure.mockRejectedValue(new Error("no wasm")); }, savedPod],
      ["a peer allocation failure", failPeer, savedPod],
      ["a host start failure", () => { mocks.hostStartFailure = new Error("host start failed"); }, savedPod],
    ];

    it.each(failures)("keeps a connected guest seat running after %s", async (_name, arrange, session) => {
      await openGuest();
      await seedSavedPod(session);
      arrange();
      renderApp(["/", "/draft-pod?entry=host"]);
      // Reach: the failure covers the guest's own screen, with the guest held.
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();
      expect(useDraftPodStore.getState().hostRecoveryFailure).not.toBeNull();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby" });

      await clickMenu();

      expect(pathname).toBe("/");
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(disposeSpy).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby" });
      expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      await expectSeatKept();
      expect(rejections).toEqual([]);
    });

    it("leaves a guest between connections with its seat kept", async () => {
      await openGuest();
      await seedSavedPod();
      failPeer();
      conn().simulateClose();
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();

      await clickMenu();

      expect(pathname).toBe("/");
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(disposeSpy).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby" });
      expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      await expectSeatKept();
      expect(rejections).toEqual([]);
    });

    it("keeps a host of another pod running, with its published setup", async () => {
      const running = await openRunningHost();
      useDraftPodStore.setState({ config: { ...useDraftPodStore.getState().config, setName: "Pod A", podSize: 6 } });
      // Another tab rewrites the shared saved pod record.
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();
      expect(useMultiplayerDraftStore.getState().hostPersistenceId).toBe("owner-a");

      await clickMenu();

      expect(pathname).toBe("/");
      expect(running.terminateDraft).not.toHaveBeenCalled();
      expect(running.dispose).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", phase: "lobby", hostPersistenceId: "owner-a" });
      expect(useDraftPodStore.getState().config).toMatchObject({ setName: "Pod A", podSize: 6 });
      expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
      expect(rejections).toEqual([]);
    });

    it("gives Back the same exit while a guest it covers is still connecting", async () => {
      void useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
      await vi.waitFor(() => expect(conns.length).toBeGreaterThan(0));
      await vi.waitFor(() => expect(conn().sentRaw.length).toBeGreaterThan(0));
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();
      expect(useDraftPodStore.getState().hostRecoveryFailure).not.toBeNull();
      expect(useMultiplayerDraftStore.getState().phase).toBe("connecting");

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(80);

      expect(pathname).toBe("/");
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(disposeSpy).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "connecting" });
      expect(rejections).toEqual([]);
    });

    it("keeps a new join with no room yet through that Back, which then lands in its lobby", async () => {
      const room = deferred<ReturnType<typeof openedRoom>>();
      mocks.joinRoom.mockReset().mockImplementation(() => room.promise);
      void useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
      await waitFor(() => expect(mocks.joinRoom).toHaveBeenCalledOnce());
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "connecting" });

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(80);

      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "connecting" });
      expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();

      const opened = openedRoom();
      await act(async () => { room.resolve(opened); });
      await vi.waitFor(() => expect(opened.conn.sentRaw.length).toBeGreaterThan(0));
      await act(async () => { await opened.conn.receiveRaw(rawMessage(welcome(legacyView()))); });
      await settle(40);

      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby" });
      await expectSeatKept();
      expect(rejections).toEqual([]);
    });

    it.each([
      ["a guest's refusal", async () => {
        await act(async () => { await conn().receiveRaw(rawMessage({ type: "draft_state_update", view: fixture.queued_view })); });
      }, REFUSAL_TEXT],
      ["a guest's own error", async () => {
        await act(async () => { await conn().receiveRaw(rawMessage({ type: "draft_error", reason: "The guest's own error" })); });
      }, "The guest's own error"],
    ] as const)("names the failed recovery, not %s, on its screen", async (_name, arrange, covered) => {
      await openGuest();
      await arrange();
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);

      expect(await screen.findByText(CONNECTION_TEXT)).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Try Reconnecting" })).toBeEnabled();
      expect(screen.queryByText(covered)).toBeNull();
    });

    it("ends a guest that its host has already refused, with its seat kept", async () => {
      await openGuest();
      await act(async () => { await conn().receiveRaw(rawMessage({ type: "draft_state_update", view: fixture.queued_view })); });
      expect(useMultiplayerDraftStore.getState().phase).toBe("error");
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();

      await clickMenu();

      expect(pathname).toBe("/");
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(disposeSpy).toHaveBeenCalledOnce();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      await expectSeatKept();
      expect(rejections).toEqual([]);
    });

    it("ends a host already in error, with its saved session kept", async () => {
      const running = await openRunningHost();
      useMultiplayerDraftStore.setState({ phase: "error" });
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();

      await clickMenu();

      expect(pathname).toBe("/");
      expect(running.terminateDraft).not.toHaveBeenCalled();
      expect(running.dispose).toHaveBeenCalledOnce();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      expect(rejections).toEqual([]);
    });

    it("clears a join that had already failed", async () => {
      mocks.joinRoom.mockReset().mockRejectedValue(new Error("no such room"));
      await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "error" });
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();

      await clickMenu();

      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle", error: null });
      expect(useDraftPodStore.getState()).toMatchObject({ hostRecoveryFailure: null, configError: null });
    });

    it("resets the pod setup and keeps the saved pod when nothing is running", async () => {
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();
      expect(useDraftPodStore.getState().configError).toBe("podError.connection");
      useDraftPodStore.setState({ config: { ...useDraftPodStore.getState().config, setName: "Typed" } });

      await clickMenu();

      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState().role).toBeNull();
      expect(useDraftPodStore.getState()).toMatchObject({ hostRecoveryFailure: null, configError: null });
      expect(useDraftPodStore.getState().config.setName).toBe("");
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      await expect(loadDraftHostSession(savedPod.persistenceId)).resolves.toMatchObject({ type: "admitted" });
    });

    it("leaves nothing running after a restore that failed past the handover", async () => {
      await openGuest();
      await seedSavedPod(startedPod);
      mocks.restoreRefusal = "FIFO drafting is not supported by this runtime";
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();
      // Reach: the handover retired the guest and the restore ended its successor.
      expect(mocks.restoreCalls).toBe(1);
      expect(disposeSpy).toHaveBeenCalledOnce();
      expect(mocks.hosts[0]!.dispose).toHaveBeenCalledOnce();

      await clickMenu();

      expect(pathname).toBe("/");
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState().role).toBeNull();
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      await expectSeatKept();
    });

    it.each([
      ["draft_kicked", "kicked"],
      ["draft_host_left", "hostLeft"],
    ] as const)("keeps a guest revoked by %s on its own screen for a later visit", async (type, phase) => {
      await openGuest();
      await act(async () => { await conn().receiveRaw(rawMessage({ type, reason: "gone" })); });
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();

      await clickMenu();

      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase });
      expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(disposeSpy).not.toHaveBeenCalled();

      await act(async () => { navigateTo!("/draft-pod"); });
      await settle(40);
      expect(screen.queryByRole("button", { name: "Try Reconnecting" })).toBeNull();
      await clickMenu();

      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(inspectActiveDraftGuest().type).toBe("absent");
      expect(rejections).toEqual([]);
    });
  });

  describe("a guest with no session to carry a leave", () => {
    const openOnPage = async (view: Record<string, unknown> = legacyView()) => {
      await openGuest(view);
      renderApp(["/", "/draft-pod?entry=guest"]);
      await settle(40);
    };
    const reconnectDial = async () => {
      conn().simulateClose();
      await act(async () => { await new Promise((done) => setTimeout(done, 1_100)); });
      const dial = conn();
      await act(async () => { dial.simulateOpen(); });
      await vi.waitFor(() => expect(dial.sentRaw.length).toBeGreaterThan(0));
      return dial;
    };
    const expectLeftLocally = async () => {
      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(disposeSpy).toHaveBeenCalledOnce();
      await expectSeatKept();
      expect(rejections).toEqual([]);
    };

    it.each([
      ["draft_state_update", { type: "draft_state_update", view: fixture.queued_view }],
      ["draft_pick_ack", { type: "draft_pick_ack", view: fixture.queued_view }],
      ["draft_deck_submit_ack", { type: "draft_deck_submit_ack", submissionId: "submission-1", view: fixture.queued_view }],
    ] as const)("leaves after refusing a later view on %s", async (_carrier, frame) => {
      await openOnPage();
      expect(screen.getByText(LOBBY_TEXT)).toBeInTheDocument();
      await act(async () => { await conn().receiveRaw(rawMessage(frame)); });
      // Reach: the refusal's own screen, whose only control is the exit.
      expect(await screen.findByText(REFUSAL_TEXT)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Try Reconnecting" })).toBeNull();
      expect(useMultiplayerDraftStore.getState().guestRecoveryFailure).toMatchObject({ kind: "incompatible", code: "fifo_flow" });

      await clickMenu();

      await expectLeftLocally();
    });

    it("leaves after refusing a later view on a background reconnect's acknowledgement", { timeout: 15_000 }, async () => {
      await openOnPage();
      const dial = await reconnectDial();
      await act(async () => {
        await dial.receiveRaw(rawMessage({ type: "draft_reconnect_ack", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, seatIndex: 2, draftCode: "draft-xyz", workspaceState: null, view: fixture.queued_view }));
      });
      expect(await screen.findByText(REFUSAL_TEXT)).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Try Reconnecting" })).toBeNull();
      expect(useMultiplayerDraftStore.getState().guestRecoveryFailure).toMatchObject({ kind: "incompatible", code: "fifo_flow" });

      await clickMenu();

      await expectLeftLocally();
    });

    it("leaves after a background reconnect is rejected for its protocol", { timeout: 15_000 }, async () => {
      await openOnPage();
      const dial = await reconnectDial();
      await act(async () => {
        await dial.receiveRaw(rawMessage({ type: "draft_reconnect_rejected", kind: "ProtocolMismatch", reason: "Refresh both windows" }));
      });
      expect(await screen.findByText("Refresh both windows")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Try Reconnecting" })).toBeNull();

      await clickMenu();

      await expectLeftLocally();
    });

    it("leaves from a retryable failure's screen between connections", { timeout: 15_000 }, async () => {
      await openOnPage();
      const dial = await reconnectDial();
      await act(async () => {
        await dial.receiveRaw(rawMessage({ type: "draft_reconnect_rejected", kind: "NoReconnectWindow", reason: "Host is restarting" }));
      });
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();

      await clickMenu();

      await expectLeftLocally();
    });

    it("leaves the lobby between connections", async () => {
      await openOnPage();
      conn().simulateClose();
      await settle(20);
      expect(screen.getByText(LOBBY_TEXT)).toBeInTheDocument();

      await userEvent.setup().click(screen.getByRole("button", { name: /Leave/ }));
      await settle(80);

      await expectLeftLocally();
    });

    it("can reconnect later with the seat it kept", async () => {
      await openOnPage();
      conn().simulateClose();
      await settle(20);
      await userEvent.setup().click(screen.getByRole("button", { name: /Leave/ }));
      await settle(80);
      await expectLeftLocally();
      const before = conns.length;

      await act(async () => { navigateTo!("/draft-pod?entry=guest"); });
      await vi.waitFor(() => expect(conns.length).toBeGreaterThan(before));
      await vi.waitFor(() => expect(conn().sentRaw.length).toBeGreaterThan(0));
      await act(async () => {
        await conn().receiveRaw(rawMessage({ type: "draft_reconnect_ack", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, seatIndex: 2, draftCode: "draft-xyz", workspaceState: null, view: legacyView() }));
      });
      await settle(40);

      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby" });
      expect(screen.getByText(LOBBY_TEXT)).toBeInTheDocument();
    });

    it("leaves the completed pod's screen between connections", async () => {
      await openOnPage(legacyView("Complete"));
      conn().simulateClose();
      await settle(20);
      expect(useMultiplayerDraftStore.getState().phase).toBe("complete");

      await clickMenu();

      await expectLeftLocally();
    });

    it("leaves a Commander game with its seat kept, and the pod page it returns to reconnects that seat", async () => {
      await openGuest(legacyView("Complete"));
      useMultiplayerDraftStore.setState({ commanderLaunch: { gameId: "game-1", hostPeerId: "host-peer" } as never });
      conn().simulateClose();
      await settle(20);
      renderApp(["/", "/game/game-1"]);
      await settle(20);
      // Reach: a completed pod's guest in its game, with a launch and no session.
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "complete" });
      expect(useMultiplayerDraftStore.getState().commanderLaunch).not.toBeNull();
      const before = conns.length;

      // Its callers make this call, then open the pod page whatever it answers.
      await act(async () => { await useMultiplayerDraftStore.getState().endCommanderSession(); });
      await settle(40);

      expect(useMultiplayerDraftStore.getState().commanderLaunch).toBeNull();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(disposeSpy).toHaveBeenCalledOnce();
      await expectSeatKept();
      expect(conns).toHaveLength(before);

      await act(async () => { navigateTo!("/draft-pod"); });
      await vi.waitFor(() => expect(conns.length).toBeGreaterThan(before));
      await vi.waitFor(() => expect(conn().sentRaw.length).toBeGreaterThan(0));
      await settle(20);

      const [first] = conn().sentRaw;
      expect(JSON.parse(new TextDecoder().decode(first!.slice(1)))).toMatchObject({ type: "draft_reconnect", draftToken: "guest-token" });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "connecting" });
      expect(pathname).toBe("/draft-pod");
      expect(rejections).toEqual([]);
    });

    it("goes back while a new join is still connecting", async () => {
      void useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
      await vi.waitFor(() => expect(conns.length).toBeGreaterThan(0));
      await vi.waitFor(() => expect(conn().sentRaw.length).toBeGreaterThan(0));
      renderApp(["/", "/draft-pod"]);
      await settle(40);
      expect(useMultiplayerDraftStore.getState().phase).toBe("connecting");

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(80);

      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(leaveSpy).not.toHaveBeenCalled();
      const sent = conn();
      expect(sent.open).toBe(false);
      expect(inspectActiveDraftGuest().type).toBe("absent");

      // A welcome that was already on its way finds nothing to land in.
      await act(async () => { await sent.receiveRaw(rawMessage(welcome(legacyView()))); });
      await settle(40);

      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(inspectActiveDraftGuest().type).toBe("absent");
      expect(rejections).toEqual([]);
    });

    it("ends a new join that goes back before its room opens, sending nothing", async () => {
      const room = deferred<ReturnType<typeof openedRoom>>();
      mocks.joinRoom.mockReset().mockImplementation(() => room.promise);
      useDraftPodStore.setState({ joinCode: "ABCDE", guestDisplayName: "Alice" });
      renderApp(["/", "/draft-pod"]);
      await settle(40);
      void useDraftPodStore.getState().joinPod();
      await waitFor(() => expect(mocks.joinRoom).toHaveBeenCalledOnce());
      await settle(10);
      // Reach: the join is pending and its guest does not exist yet.
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "connecting" });

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(40);

      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });

      const opened = openedRoom();
      await act(async () => { room.resolve(opened); });
      await settle(40);

      expect(opened.destroyPeer).toHaveBeenCalledOnce();
      expect(opened.conn.sentRaw).toHaveLength(0);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(inspectActiveDraftGuest().type).toBe("absent");
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(rejections).toEqual([]);
    });

    it("stays ended when that new join's room then fails", async () => {
      const room = deferred<ReturnType<typeof openedRoom>>();
      mocks.joinRoom.mockReset().mockImplementation(() => room.promise);
      useDraftPodStore.setState({ joinCode: "ABCDE", guestDisplayName: "Alice" });
      renderApp(["/", "/draft-pod"]);
      await settle(40);
      void useDraftPodStore.getState().joinPod();
      await waitFor(() => expect(mocks.joinRoom).toHaveBeenCalledOnce());
      await settle(10);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "connecting" });

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(40);
      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });

      await act(async () => { room.reject(new Error("Connection timed out")); });
      await settle(40);

      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle", error: null });
      expect(useDraftPodStore.getState().configError).toBeNull();
      expect(rejections).toEqual([]);
    });

    it("leaves between the handshakes of a first reconnect, with its seat kept", async () => {
      await seedSavedSeat();
      renderApp(["/", "/draft-pod?entry=guest"]);
      await vi.waitFor(() => expect(conn().sentRaw.length).toBeGreaterThan(0));
      await settle(20);
      await act(async () => { conn().simulateClose(); });
      await settle(40);
      // Reach: the first handshake failed and the next waits for its backoff.
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "connecting" });
      const dials = conns.length;

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(1_200);

      await expectLeftLocally();
      expect(conns).toHaveLength(dials);
    });

    it.each([
      ["guest", "/draft-pod?entry=guest"],
      ["plain", "/draft-pod"],
    ])("stops a reconnect on the %s route that goes back before its room opens", async (_route, entry) => {
      await seedSavedSeat();
      const room = deferred<ReturnType<typeof openedRoom>>();
      mocks.joinRoom.mockReset().mockImplementation(() => room.promise);
      renderApp(["/", entry]);
      await waitFor(() => expect(mocks.joinRoom).toHaveBeenCalledOnce());
      await settle(20);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "connecting" });

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(40);
      const opened = openedRoom();
      await act(async () => { room.resolve(opened); });
      await settle(40);

      expect(pathname).toBe("/");
      expect(opened.destroyPeer).toHaveBeenCalledOnce();
      expect(opened.conn.sentRaw).toHaveLength(0);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(leaveSpy).not.toHaveBeenCalled();
      await expectSeatKept();
      expect(rejections).toEqual([]);
    });

    it("asks the host when a reconnect's session can already carry the leave", async () => {
      await seedSavedSeat();
      renderApp(["/", "/draft-pod?entry=guest"]);
      await vi.waitFor(() => expect(conn().sentRaw.length).toBeGreaterThan(0));
      await settle(20);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "connecting" });

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(40);

      expect(leaveSpy).toHaveBeenCalledOnce();
      expect(pathname).toBe("/draft-pod?entry=guest");
      await expectSeatKept();

      await act(async () => { await conn().receiveRaw(rawMessage({ type: "draft_leave_ack", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "guest-token" })); });
      await settle(80);

      expect(pathname).toBe("/");
      expect(inspectActiveDraftGuest().type).toBe("absent");
      expect(rejections).toEqual([]);
    });

    it("leaves on a second click after its sent leave lost the session", async () => {
      await openOnPage();
      const leaveButton = screen.getByRole("button", { name: /Leave/ });
      await userEvent.setup().click(leaveButton);
      await settle(20);
      expect(leaveSpy).toHaveBeenCalledOnce();

      await act(async () => { conn().simulateClose(); });
      await settle(40);
      // The first click's leave fails, and the page stays.
      expect(pathname).toBe("/draft-pod?entry=guest");
      expect(rejections).toEqual(["Draft host disconnected before acknowledging leave"]);

      await userEvent.setup().click(screen.getByRole("button", { name: /Leave/ }));
      await settle(80);

      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(leaveSpy).toHaveBeenCalledOnce();
      await expectSeatKept();
      expect(rejections).toHaveLength(1);
    });

    it("still asks the host when a session can carry the leave", async () => {
      await openOnPage();
      const leaveButton = screen.getByRole("button", { name: /Leave/ });

      await userEvent.setup().click(leaveButton);
      await settle(40);

      // The seat stays until the host acknowledges.
      expect(leaveSpy).toHaveBeenCalledOnce();
      expect(pathname).toBe("/draft-pod?entry=guest");
      expect(useMultiplayerDraftStore.getState().role).toBe("guest");
      await expectSeatKept();

      await act(async () => { await conn().receiveRaw(rawMessage({ type: "draft_leave_ack", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "guest-token" })); });
      await settle(80);

      expect(pathname).toBe("/");
      expect(inspectActiveDraftGuest().type).toBe("absent");
    });

    it.each(["draft_kicked", "draft_host_left"])("leaves a revoked guest after %s", async (type) => {
      await openOnPage();
      await act(async () => { await conn().receiveRaw(rawMessage({ type, reason: "gone" })); });
      expect(useMultiplayerDraftStore.getState().role).toBe("guest");

      await clickMenu();

      expect(pathname).toBe("/");
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(inspectActiveDraftGuest().type).toBe("absent");
    });
  });

  describe("a failure from an earlier page mount", () => {
    it.each(["guest seat", "host of another pod"] as const)("does not cover a held %s on a later mount at /draft-pod", async (session) => {
      if (session === "guest seat") await openGuest();
      else await openRunningHost();
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();
      const failure = useDraftPodStore.getState().hostRecoveryFailure;
      expect(failure?.routeToken).toBe(1);

      await act(async () => { navigateTo!(-1); });
      await settle(40);
      mocks.hostRoom.mockClear();
      await act(async () => { navigateTo!("/draft-pod"); });
      await settle(40);
      // Any later render of the route, such as a window resize.
      await act(async () => { window.dispatchEvent(new Event("resize")); });
      await settle(40);

      expect(pathname).toBe("/draft-pod");
      expect(screen.queryByRole("button", { name: "Return to Menu" })).toBeNull();
      expect(screen.getByRole("button", { name: /Leave/ })).toBeInTheDocument();
      expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
      expect(mocks.hostRoom).not.toHaveBeenCalled();
    });

    it("does not cover a held guest seat after a retry that failed again", async () => {
      await openGuest();
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      await userEvent.setup().click(await screen.findByRole("button", { name: "Try Reconnecting" }));
      await settle(80);
      // Reach: the retry's own failure, on a later route token.
      expect(useDraftPodStore.getState().hostRecoveryFailure?.routeToken).toBe(2);
      expect(screen.getByRole("button", { name: "Try Reconnecting" })).toBeEnabled();

      await act(async () => { navigateTo!(-1); });
      await settle(40);
      mocks.hostRoom.mockClear();
      await act(async () => { navigateTo!("/draft-pod"); });
      await settle(40);
      await act(async () => { window.dispatchEvent(new Event("resize")); });
      await settle(40);

      expect(screen.queryByRole("button", { name: "Return to Menu" })).toBeNull();
      expect(screen.getByRole("button", { name: /Leave/ })).toBeInTheDocument();
      expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
      expect(mocks.hostRoom).not.toHaveBeenCalled();
    });

    it("gives way to a fresh recovery attempt when nothing is held", async () => {
      await seedSavedPod();
      failPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      expect(await screen.findByRole("button", { name: "Try Reconnecting" })).toBeEnabled();
      await act(async () => { navigateTo!(-1); });
      await settle(40);
      mocks.hostRoom.mockClear();

      await act(async () => { navigateTo!("/draft-pod"); });
      await settle(80);

      expect(mocks.hostRoom).toHaveBeenCalledOnce();
    });
  });

  describe("a recovery that ends before its handover", () => {
    it.each([
      ["its route ends", () => { navigateTo!("/"); }],
      ["the browser goes offline", () => { useConnectivityStore.setState({ browserOnline: false }); }],
      ["the setup changes", () => { useDraftPodStore.getState().setConfig({ tournamentFormat: "SingleElimination" } as never); }],
    ] as const)("leaves a held guest seat untouched when %s", async (_name, trigger) => {
      await openGuest();
      await seedSavedPod(startedPod);
      const peer = peerFor(savedPod.roomCode);
      let releasePeer!: () => void;
      mocks.hostRoom.mockReset().mockImplementation(() => new Promise((resolve) => { releasePeer = () => resolve(peer); }));
      renderApp(["/", "/draft-pod?entry=host"]);
      await waitFor(() => expect(mocks.hostRoom).toHaveBeenCalledOnce());

      await act(async () => { trigger(); });
      await act(async () => { releasePeer(); });
      await settle(60);

      expect(peer.destroy).toHaveBeenCalledOnce();
      // The provisional host, where one was built, never reached its restore.
      for (const host of mocks.hosts) expect(host.dispose).toHaveBeenCalledOnce();
      expect(mocks.restoreCalls).toBe(0);
      expect(leaveSpy).not.toHaveBeenCalled();
      expect(disposeSpy).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby" });
      expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      await expectSeatKept();
    });

    describe("during its card data load", () => {
      const cubePod: PersistedDraftHostSession = {
        ...startedPod,
        poolInput: {
          type: "Cube",
          data: {
            cube_name: "Test Cube",
            cube_list_text: "1 Lightning Bolt\n",
            cube_draft_settings: { pod_size: 4, pack_count: 1, cards_per_pack: 2, min_deck_size: 4, addable_cards: { policy: "StandardBasics", custom: [] } },
          },
        },
      } as never;
      const okResponse = { ok: true, status: 200, text: async () => "{}" };
      let releaseLoad!: () => void;
      // As the signaling server does, it refuses an id a live peer still holds.
      const live = new Set<string>();
      const peers: Array<ReturnType<typeof peerFor>> = [];

      beforeEach(async () => {
        live.clear();
        peers.length = 0;
        mocks.loadCardDatabase.mockClear();
        mocks.hostRoom.mockReset().mockImplementation(async (_signal: unknown, options?: { preferredRoomCode?: string }) => {
          const code = options?.preferredRoomCode ?? "ZZZZZ";
          if (live.has(code)) throw new Error("Failed to create room: unavailable-id");
          live.add(code);
          const peer = peerFor(code);
          peer.destroy.mockImplementation(() => { live.delete(code); });
          peers.push(peer);
          return peer;
        });
        vi.stubGlobal("fetch", vi.fn(() => new Promise((resolve) => { releaseLoad = () => resolve(okResponse); })));
        await seedSavedPod(cubePod);
      });
      afterEach(() => { vi.unstubAllGlobals(); });

      const enter = async () => {
        await act(async () => { navigateTo!("/draft-pod?entry=host"); });
        await settle(40);
      };

      it("closes the room at the route's end, and the next attempt waits for that", async () => {
        renderApp(["/", "/draft-pod?entry=host"]);
        await waitFor(() => expect(peers).toHaveLength(1));
        await settle(40);
        expect(fetch).toHaveBeenCalledOnce();
        expect(peers[0]!.destroy).not.toHaveBeenCalled();

        await act(async () => { navigateTo!("/"); });
        await settle(40);
        expect(peers[0]!.destroy).toHaveBeenCalledOnce();
        expect(useMultiplayerDraftStore.getState().role).toBeNull();

        await enter();
        expect(mocks.hostRoom).toHaveBeenCalledOnce();
        expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();

        await act(async () => { releaseLoad(); });
        await waitFor(() => expect(mocks.hostRoom).toHaveBeenCalledTimes(2));
        await settle(40);
        await act(async () => { releaseLoad(); });
        await settle(80);
        expect(screen.queryByRole("button", { name: "Try Reconnecting" })).toBeNull();
        expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", phase: "lobby", hostPersistenceId: savedPod.persistenceId });
        expect(mocks.restoreCalls).toBe(1);
      });

      it("closes the room at once on Back and keeps the saved pod", async () => {
        renderApp(["/", "/draft-pod?entry=host"]);
        await waitFor(() => expect(peers).toHaveLength(1));
        await settle(40);

        await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
        await settle(40);

        expect(pathname).toBe("/");
        expect(peers[0]!.destroy).toHaveBeenCalledOnce();
        await act(async () => { releaseLoad(); });
        await settle(40);
        expect(peers[0]!.destroy).toHaveBeenCalledOnce();
        expect(mocks.restoreCalls).toBe(0);
        expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      });

      it("keeps the room until the load settles when the browser goes offline", async () => {
        await openGuest();
        renderApp(["/", "/draft-pod?entry=host"]);
        await waitFor(() => expect(peers).toHaveLength(1));
        await settle(40);

        await act(async () => { useConnectivityStore.setState({ browserOnline: false }); });
        await settle(40);
        expect(fetch).toHaveBeenCalledOnce();
        expect(peers[0]!.destroy).not.toHaveBeenCalled();

        await act(async () => { releaseLoad(); });
        await settle(60);
        expect(peers[0]!.destroy).toHaveBeenCalledOnce();
        expect(mocks.restoreCalls).toBe(0);
        expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby" });
      });
    });
  });

  describe("a recovery with nothing running in the tab", () => {
    it.each([
      ["a saved pod", savedPod],
      ["a draft in progress", startedPod],
    ] as const)("goes back at once before the handover of %s, keeping what was saved", async (_name, session) => {
      await seedSavedPod(session);
      const peer = peerFor(savedPod.roomCode);
      const allocation = deferred<typeof peer>();
      mocks.hostRoom.mockReset().mockImplementation(() => allocation.promise);
      renderApp(["/", "/draft-pod?entry=host"]);
      await waitFor(() => expect(mocks.hostRoom).toHaveBeenCalledOnce());
      await settle(10);
      // Reach: nothing has been installed, and Back is offered.
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(30);

      expect(pathname).toBe("/");
      expect(useMultiplayerDraftStore.getState().role).toBeNull();

      await act(async () => { allocation.resolve(peer); });
      await settle(60);

      expect(peer.destroy).toHaveBeenCalledOnce();
      expect(mocks.restoreCalls).toBe(0);
      expect(useMultiplayerDraftStore.getState().role).toBeNull();
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      await expect(loadDraftHostSession(savedPod.persistenceId)).resolves.toMatchObject({ type: "admitted" });
      expect(rejections).toEqual([]);
    });
  });

  describe("a host's exit that a recovery's handover overtakes", () => {
    const holdPeer = () => {
      const peer = peerFor(savedPod.roomCode);
      const allocation = deferred<typeof peer>();
      mocks.hostRoom.mockReset().mockImplementation(() => allocation.promise);
      return { peer, allocation };
    };
    const expectRecoveryEndedWithItsSavedPod = async () => {
      const recovered = mocks.hosts[mocks.hosts.length - 1]!;
      expect(recovered.id).toBe(savedPod.persistenceId);
      expect(recovered.terminateDraft).not.toHaveBeenCalled();
      expect(recovered.dispose).toHaveBeenCalledOnce();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      await expect(loadDraftHostSession(savedPod.persistenceId)).resolves.toMatchObject({ type: "admitted" });
    };

    it.each([
      ["a saved pod", savedPod],
      ["a draft in progress", startedPod],
    ] as const)("ends only the running host when %s takes over during its lobby leave", async (_name, session) => {
      const running = await openRunningHost();
      await seedSavedPod(session);
      const { peer, allocation } = holdPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      await waitFor(() => expect(mocks.hostRoom).toHaveBeenCalledOnce());
      await settle(10);
      const termination = deferred();
      mocks.terminateGate = termination.promise;

      await userEvent.setup().click(screen.getByRole("button", { name: /Leave/ }));
      await settle(20);
      expect(running.terminateDraft).toHaveBeenCalledOnce();

      await act(async () => { allocation.resolve(peer); });
      await settle(40);
      mocks.terminateGate = null;
      await act(async () => { termination.resolve(); });
      await settle(100);

      expect(pathname).toBe("/");
      expect(running.terminateDraft).toHaveBeenCalledOnce();
      await expectRecoveryEndedWithItsSavedPod();
      expect(rejections).toEqual([]);
    });

    it("ends only the running host when a saved pod takes over during End Draft", async () => {
      const running = await openRunningHost();
      await seedSavedPod();
      const { peer, allocation } = holdPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      await waitFor(() => expect(mocks.hostRoom).toHaveBeenCalledOnce());
      await settle(10);
      const termination = deferred();
      mocks.terminateGate = termination.promise;
      window.confirm = () => true;

      await userEvent.setup().click(screen.getAllByRole("button", { name: "End Draft" })[0]!);
      await settle(20);
      expect(running.terminateDraft).toHaveBeenCalledOnce();

      await act(async () => { allocation.resolve(peer); });
      await settle(40);
      mocks.terminateGate = null;
      await act(async () => { termination.resolve(); });
      await settle(100);

      expect(pathname).toBe("/");
      expect(running.terminateDraft).toHaveBeenCalledOnce();
      await expectRecoveryEndedWithItsSavedPod();
      expect(rejections).toEqual([]);
    });

    it("keeps the saved pod that takes over after the host answered a guest's lobby leave", async () => {
      await openGuest();
      await seedSavedPod();
      const { peer, allocation } = holdPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      await waitFor(() => expect(mocks.hostRoom).toHaveBeenCalledOnce());
      await settle(10);
      const revocation = deferred();
      mocks.revocationGate = revocation.promise;

      await userEvent.setup().click(screen.getByRole("button", { name: /Leave/ }));
      await settle(20);
      await act(async () => { await conn().receiveRaw(rawMessage({ type: "draft_leave_ack", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "guest-token" })); });
      await settle(30);
      // Reach: the answer is in and the revocation still saving, so the page has not moved.
      expect(leaveSpy).toHaveBeenCalledOnce();
      expect(pathname).toBe("/draft-pod?entry=host");

      await act(async () => { allocation.resolve(peer); });
      await settle(40);
      // Reach: the handover has retired the guest and the recovered host is installed.
      expect(mocks.hosts).toHaveLength(1);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: savedPod.persistenceId });

      mocks.revocationGate = null;
      await act(async () => { revocation.resolve(); });
      await settle(100);

      expect(pathname).toBe("/");
      await expectRecoveryEndedWithItsSavedPod();
      expect(inspectActiveDraftGuest().type).toBe("absent");
      expect(rejections).toEqual([]);
    });

    it("gives way to a leave the host answered before the handover", async () => {
      await openGuest();
      await seedSavedPod();
      const { peer, allocation } = holdPeer();
      renderApp(["/", "/draft-pod?entry=host"]);
      await waitFor(() => expect(mocks.hostRoom).toHaveBeenCalledOnce());
      await settle(10);

      await userEvent.setup().click(screen.getByRole("button", { name: /Leave/ }));
      await settle(20);
      await act(async () => { await conn().receiveRaw(rawMessage({ type: "draft_leave_ack", draftProtocolVersion: DRAFT_PROTOCOL_VERSION, draftToken: "guest-token" })); });
      await settle(60);
      expect(pathname).toBe("/");

      await act(async () => { allocation.resolve(peer); });
      await settle(60);

      expect(peer.destroy).toHaveBeenCalledOnce();
      expect(mocks.hosts).toHaveLength(0);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(inspectActiveDraftGuest().type).toBe("absent");
      expect(inspectActiveDraftPod()).toMatchObject({ type: "present", capture: { id: savedPod.persistenceId } });
      expect(rejections).toEqual([]);
    });
  });

  describe("a host still starting", () => {
    const hostConfig = (persistenceId: string) => ({
      poolInput: savedPod.poolInput,
      kind: savedPod.kind,
      podSize: 6,
      hostDisplayName: "Host A",
      tournamentFormat: savedPod.tournamentFormat,
      podPolicy: savedPod.podPolicy,
      persistenceId,
    } as never);
    const startThenHold = async () => {
      const start = deferred();
      mocks.hostStartGate = start.promise;
      void useMultiplayerDraftStore.getState().hostDraft(hostConfig("owner-a"));
      await waitFor(() => expect(mocks.hosts).toHaveLength(1));
      return start;
    };

    it("drops a new join still waiting for that host's teardown", async () => {
      const start = await startThenHold();
      useDraftPodStore.setState({ joinCode: "ABCDE", guestDisplayName: "Alice" });
      renderApp(["/", "/draft-pod"]);
      await settle(30);
      void useDraftPodStore.getState().joinPod();
      await settle(20);
      // Reach: the join has retired the host and waits for its teardown.
      expect(mocks.joinRoom).not.toHaveBeenCalled();

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(30);
      expect(pathname).toBe("/");

      mocks.hostStartGate = null;
      await act(async () => { start.resolve(); });
      await settle(60);

      expect(mocks.joinRoom).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(inspectActiveDraftGuest().type).toBe("absent");
      expect(rejections).toEqual([]);
    });

    it("drops a second host start still waiting for the first one's teardown", async () => {
      const start = await startThenHold();
      renderApp(["/", "/draft-pod"]);
      await settle(30);
      mocks.hostRoom.mockClear();
      void useMultiplayerDraftStore.getState().hostDraft(hostConfig("owner-c"));
      await settle(20);
      expect(mocks.hostRoom).not.toHaveBeenCalled();

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(30);
      expect(pathname).toBe("/");

      mocks.hostStartGate = null;
      await act(async () => { start.resolve(); });
      await settle(60);

      expect(mocks.hostRoom).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(inspectActiveDraftPod().type).toBe("absent");
      expect(rejections).toEqual([]);
    });

    it("goes back once its peer has settled, leaving nothing saved", async () => {
      const peer = peerFor("AAAAA");
      const allocation = deferred<typeof peer>();
      mocks.hostRoom.mockReset().mockImplementation(() => allocation.promise);
      renderApp(["/", "/draft-pod"]);
      await settle(30);
      void useMultiplayerDraftStore.getState().hostDraft(hostConfig("owner-a"));
      await waitFor(() => expect(mocks.hostRoom).toHaveBeenCalledOnce());
      await settle(10);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", phase: "connecting" });

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(30);
      // A host's teardown waits for its start to settle.
      expect(pathname).toBe("/draft-pod");

      await act(async () => { allocation.resolve(peer); });
      await settle(60);

      expect(pathname).toBe("/");
      expect(peer.destroy).toHaveBeenCalledOnce();
      expect(mocks.hosts).toHaveLength(0);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(inspectActiveDraftPod().type).toBe("absent");
      expect(rejections).toEqual([]);
    });
  });

  describe("Try Reconnecting over a held guest seat", () => {
    it("retires the seat with its recovery kept once the recovered host takes over", async () => {
      await openGuest();
      await seedSavedPod();
      mocks.hostRoom.mockRejectedValueOnce(new Error("peer unavailable"));
      renderApp(["/", "/draft-pod?entry=host"]);
      const retry = await screen.findByRole("button", { name: "Try Reconnecting" });
      expect(mocks.hostRoom).toHaveBeenCalledOnce();
      expect(disposeSpy).not.toHaveBeenCalled();

      await userEvent.setup().click(retry);
      await waitFor(() => expect(useMultiplayerDraftStore.getState().role).toBe("host"));
      await settle(40);

      expect(leaveSpy).not.toHaveBeenCalled();
      expect(disposeSpy).toHaveBeenCalledOnce();
      await expectSeatKept();
      expect(useDraftPodStore.getState().config).toMatchObject({ setName: "TST", podSize: savedPod.podSize });

      await act(async () => { navigateTo!("/"); });
      await settle(60);

      expect(useMultiplayerDraftStore.getState().role).toBeNull();
      expect(mocks.hosts[0]!.dispose).toHaveBeenCalledOnce();
    });
  });

  describe("a recovery with its restore held", () => {
    async function holdRestore(path: "entry" | "retry") {
      await seedSavedPod(startedPod);
      let release!: () => void;
      mocks.restoreGate = new Promise((done) => { release = done; });
      if (path === "retry") mocks.hostRoom.mockRejectedValueOnce(new Error("peer unavailable"));
      renderApp(["/", "/draft-pod?entry=host"]);
      if (path === "retry") {
        const retry = await screen.findByRole("button", { name: "Try Reconnecting" });
        expect(mocks.hostRoom).toHaveBeenCalledOnce();
        await userEvent.setup().click(retry);
      }
      await waitFor(() => expect(mocks.restoreCalls).toBe(1));
      // Reach: the handover has made this host the installed owner.
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", hostPersistenceId: savedPod.persistenceId });
      return release;
    }
    const routeEnd = async () => {
      await act(async () => { navigateTo!("/"); });
      await settle(60);
    };

    it.each(["entry", "retry"] as const)("ends a host recovered by %s with its route when the browser goes offline during the restore", async (path) => {
      const release = await holdRestore(path);
      useConnectivityStore.setState({ browserOnline: false });
      release();
      await settle(60);
      expect(useMultiplayerDraftStore.getState().role).toBe("host");
      expect(useDraftPodStore.getState().configError).toBe("offline.startUnavailable");

      await routeEnd();

      expect(useMultiplayerDraftStore.getState().role).toBeNull();
      expect(mocks.hosts[0]!.dispose).toHaveBeenCalledOnce();
      expect(mocks.hosts[0]!.terminateDraft).not.toHaveBeenCalled();
      await expect(loadDraftHostSession(savedPod.persistenceId)).resolves.toMatchObject({ type: "admitted" });
    });

    it.each(["entry", "retry"] as const)("ends a host recovered by %s with its route when the setup changes during the restore", async (path) => {
      const release = await holdRestore(path);
      useDraftPodStore.getState().setConfig({ tournamentFormat: "SingleElimination" } as never);
      release();
      await settle(60);
      expect(useMultiplayerDraftStore.getState().role).toBe("host");

      await routeEnd();

      expect(useMultiplayerDraftStore.getState().role).toBeNull();
      expect(mocks.hosts[0]!.dispose).toHaveBeenCalledOnce();
      expect(mocks.hosts[0]!.terminateDraft).not.toHaveBeenCalled();
      await expect(loadDraftHostSession(savedPod.persistenceId)).resolves.toMatchObject({ type: "admitted" });
    });

    it("ends the host when its route ends during the restore, publishing nothing", async () => {
      const release = await holdRestore("entry");

      await routeEnd();

      expect(useMultiplayerDraftStore.getState().role).toBeNull();
      expect(mocks.hosts[0]!.dispose).toHaveBeenCalledOnce();
      release();
      await settle(60);
      expect(useMultiplayerDraftStore.getState().role).toBeNull();
      expect(useDraftPodStore.getState().procedureCacheKey).toBeNull();
      expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
      await expect(loadDraftHostSession(savedPod.persistenceId)).resolves.toMatchObject({ type: "admitted" });
    });

    const records: Array<[string, () => void, string]> = [
      ["absent", () => { clearActiveDraftPod(); }, "absent"],
      ["invalid", () => { localStorage.setItem("phase-active-draft-pod", "{not json"); }, "invalid"],
      ["another pod's", () => { saveActiveDraftPod(savedRecord({ id: "owner-z", updatedAt: Date.now() })); }, "present"],
    ];
    it.each((["entry", "retry"] as const).flatMap((path) => records.map(([name, change, after]) => [path, name, change, after] as const)))(
      "publishes the setup of a host recovered by %s whose saved record is then %s",
      async (path, _name, change, after) => {
        const release = await holdRestore(path);
        change();
        release();
        await settle(60);

        expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", phase: "lobby", hostPersistenceId: savedPod.persistenceId });
        expect(useDraftPodStore.getState().config).toMatchObject({ kind: savedPod.kind, podSize: savedPod.podSize, setName: "TST" });
        expect(useDraftPodStore.getState().hostRecoveryFailure).toBeNull();
        expect(inspectActiveDraftPod().type).toBe(after);

        await routeEnd();

        expect(useMultiplayerDraftStore.getState().role).toBeNull();
        expect(mocks.hosts[0]!.dispose).toHaveBeenCalledOnce();
      },
    );
  });
});
