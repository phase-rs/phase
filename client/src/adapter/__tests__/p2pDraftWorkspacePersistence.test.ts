import { beforeEach, describe, expect, it, vi } from "vitest";

const persistenceMocks = vi.hoisted(() => ({
  saveDraftHostSession: vi.fn(async () => {}),
  clearDraftHostSession: vi.fn(async () => {}),
  saveDraftGuestSession: vi.fn(async () => {}),
  saveActiveDraftGuest: vi.fn(),
  loadDraftDeckSubmission: vi.fn(async () => null),
  saveDraftDeckSubmission: vi.fn(async () => {}),
  clearDraftDeckSubmission: vi.fn(async () => {}),
  clearDraftGuestRecovery: vi.fn(async () => {}),
  clearDraftGuestSession: vi.fn(async () => {}),
}));

vi.mock("../../services/draftPersistence", () => persistenceMocks);
vi.mock("../draft-adapter", () => ({
  DraftAdapter: vi.fn().mockImplementation(function () {
    return {};
  }),
  EMPTY_DRAFT_POOL_GROUPS: {
    color_groups: [],
    type_groups: [],
    cmc_groups: [],
    rarity_groups: [],
    type_filter_options: [],
    color_filter_options: [],
    color_counts: { white: 0, blue: 0, black: 0, red: 0, green: 0 },
    workspace_capabilities: { rarity_group_order: null },
    workspace_row_classification: {
      creature_instance_ids: [],
      noncreature_instance_ids: [],
    },
  },
}));

import type { DraftPlayerView } from "../draft-adapter";
import { P2PDraftGuest } from "../p2p-draft-guest";
import { P2PDraftHost } from "../p2p-draft-host";
import { DRAFT_PROTOCOL_VERSION, encodeDraftWireMessage, type DraftP2PMessage } from "../../network/draftProtocol";
import { FakeDraftDataConnection } from "../../network/__tests__/fakeDraftDataConnection";
import type { PersistedDraftHostSession } from "../../services/draftPersistence";
import type { DraftWorkspaceState } from "../../components/draft/workspace/types";

function card(instanceId: string) {
  return {
    instance_id: instanceId,
    name: `Card ${instanceId}`,
    set_code: "TST",
    collector_number: "1",
    rarity: "common",
    colors: [],
    cmc: 1,
    type_line: "Creature",
  };
}

function view(...instanceIds: string[]): DraftPlayerView {
  return { launch_capability: "None", pool: instanceIds.map(card) } as unknown as DraftPlayerView;
}

function workspace(
  placements: DraftWorkspaceState["placements"] = {},
  virtualBasics: DraftWorkspaceState["virtualBasics"] = [],
): DraftWorkspaceState {
  return { schemaVersion: 1, placements, virtualBasics };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function makeHost(persistenceId?: string): P2PDraftHost {
  const host = new P2PDraftHost(
    { id: "host" } as never,
    () => () => {},
    { type: "Set", data: { set_pool_json: "{}" } } as never,
    "Premier",
    8,
    "Host",
    "Swiss",
    "Competitive",
    60_000,
    persistenceId,
    "ABCDE",
  );
  (host as unknown as { procedure: { packs_per_player: number; min_deck_size: number } }).procedure = {
    packs_per_player: 3,
    min_deck_size: 40,
  };
  return host;
}

function persisted(
  snapshots: Record<number, DraftWorkspaceState>,
): PersistedDraftHostSession {
  return {
    persistenceId: "persisted",
    roomCode: "ABCDE",
    kind: "Premier",
    podSize: 8,
    hostDisplayName: "Host",
    tournamentFormat: "Swiss",
    podPolicy: "Competitive",
    seatTokens: {},
    seatNames: { 0: "Host" },
    kickedTokens: [],
    draftStarted: true,
    draftCode: "draft-code",
    draftSessionJson: "{}",
    poolInput: { type: "Set", data: { set_pool_json: "{}" } } as never,
    perSeatWorkspaceSnapshots: snapshots,
  };
}

type PrivateHost = {
  adapter: {
    getViewForSeat: (seat: number) => Promise<DraftPlayerView>;
    importSession?: (json: string, version: number) => Promise<DraftPlayerView>;
    setSeatConnected?: (seat: number, connected: boolean) => Promise<void>;
  };
  perSeatWorkspaceSnapshots: Map<number, DraftWorkspaceState>;
  mutationQueue: Promise<void>;
  persistQueue: Promise<void>;
  persistSession: () => void;
  persistSessionStrict: () => Promise<void>;
  runDetachedMutation: (label: string, operation: () => Promise<unknown>) => void;
  recoverSettlementOutbox: (view: DraftPlayerView) => Promise<void>;
  procedure: unknown;
  handleGuestMessage: (seat: number, message: DraftP2PMessage) => Promise<void>;
  handleNewGuest: (session: SessionStub, displayName: string) => Promise<void>;
  handleReconnect: (session: SessionStub, token: string) => Promise<void>;
  seatTokens: Map<number, string>;
  disconnectedSeats: Map<number, { disconnectedAt: number; timer: ReturnType<typeof setTimeout> | null }>;
  draftStarted: boolean;
  guestSessions: Map<number, SessionStub>;
};

type SessionStub = {
  send: (message: DraftP2PMessage) => Promise<void>;
  onMessage: (listener: (message: DraftP2PMessage) => void) => () => void;
  onDisconnect: (listener: () => void) => () => void;
  close: () => void;
};

describe("P2P draft workspace persistence", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("serializes guest and host workspace updates with the authoritative mutation queue", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost;
    const firstView = deferred<DraftPlayerView>();
    const lookupOrder: number[] = [];
    privateHost.adapter = {
      getViewForSeat: vi.fn(async (seat) => {
        lookupOrder.push(seat);
        return seat === 1 ? firstView.promise : view("host-card");
      }),
    };
    privateHost.persistSessionStrict = vi.fn(async () => {});
    privateHost.guestSessions.set(1, {
      send: vi.fn(async () => {}), onMessage: () => () => {}, onDisconnect: () => () => {}, close: () => {},
    });

    privateHost.runDetachedMutation("guest message", () => privateHost.handleGuestMessage(1, {
      type: "draft_workspace_update",
      workspaceState: workspace(),
    }));
    await Promise.resolve();
    const hostUpdate = host.updateHostWorkspace(workspace());
    await Promise.resolve();
    expect(lookupOrder).toEqual([1]);

    firstView.resolve(view("guest-card"));
    await hostUpdate;

    expect(lookupOrder).toEqual([1, 0]);
    expect(privateHost.perSeatWorkspaceSnapshots.get(1)?.placements).toHaveProperty("guest-card");
    expect(privateHost.perSeatWorkspaceSnapshots.get(0)?.placements).toHaveProperty("host-card");
    expect(privateHost.persistSessionStrict).toHaveBeenCalledTimes(2);
  });

  it("keeps a later pick behind an in-flight guest workspace update", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost & {
      adapter: PrivateHost["adapter"] & {
        submitPickForSeat: (seat: number, cardInstanceIds: string[]) => Promise<DraftPlayerView>;
        allPicksSubmitted: () => Promise<boolean>;
      };
    };
    const workspaceView = deferred<DraftPlayerView>();
    const order: string[] = [];
    privateHost.draftStarted = true;
    privateHost.adapter = {
      getViewForSeat: vi.fn(async (seat) => {
        if (seat === 1) {
          order.push("workspace-view");
          return workspaceView.promise;
        }
        return view("host");
      }),
      submitPickForSeat: vi.fn(async () => {
        order.push("pick");
        return view("picked");
      }),
      allPicksSubmitted: vi.fn(async () => false),
    };
    privateHost.persistSessionStrict = vi.fn(async () => { order.push("persist-workspace"); });
    privateHost.guestSessions.set(1, {
      send: vi.fn(async () => {}), onMessage: () => () => {}, onDisconnect: () => () => {}, close: () => {},
    });

    privateHost.runDetachedMutation("guest message", () => privateHost.handleGuestMessage(1, {
      type: "draft_workspace_update",
      workspaceState: workspace(),
    }));
    await Promise.resolve();
    const pick = host.submitHostPick(["picked"]);
    await Promise.resolve();

    expect(order).toEqual(["workspace-view"]);

    workspaceView.resolve(view("guest"));
    await pick;

    expect(order.indexOf("persist-workspace")).toBeLessThan(order.indexOf("pick"));
  });

  it("propagates host operation error without poisoning later work", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost;
    privateHost.adapter = {
      getViewForSeat: vi.fn()
        .mockRejectedValueOnce(new Error("view failed"))
        .mockResolvedValue(view("later")),
    };
    privateHost.persistSessionStrict = vi.fn(async () => {});

    await expect(host.updateHostWorkspace(workspace())).rejects.toThrow("view failed");
    expect(privateHost.perSeatWorkspaceSnapshots.size).toBe(0);
    expect(privateHost.persistSessionStrict).not.toHaveBeenCalled();

    await host.updateHostWorkspace(workspace());
    expect(privateHost.perSeatWorkspaceSnapshots.get(0)?.placements).toHaveProperty("later");
    expect(privateHost.persistSessionStrict).toHaveBeenCalledOnce();
  });

  it("reports guest validation error and processes the next update", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost;
    const sent: DraftP2PMessage[] = [];
    privateHost.adapter = { getViewForSeat: vi.fn(async () => view("valid")) };
    privateHost.persistSessionStrict = vi.fn(async () => {});
    privateHost.guestSessions.set(1, {
      send: vi.fn(async (message) => { sent.push(message); }),
      onMessage: () => () => {}, onDisconnect: () => () => {},
      close: () => {},
    });

    privateHost.runDetachedMutation("guest message", () => privateHost.handleGuestMessage(1, {
      type: "draft_workspace_update",
      workspaceState: workspace({ bad: { zone: "deck", row: 2, column: 0, order: 0 } }),
    }));
    privateHost.runDetachedMutation("guest message", () => privateHost.handleGuestMessage(1, {
      type: "draft_workspace_update",
      workspaceState: workspace(),
    }));
    await privateHost.mutationQueue;

    expect(sent).toEqual([{ type: "draft_error", reason: "placement bad has an invalid row" }]);
    expect(privateHost.perSeatWorkspaceSnapshots.get(1)?.placements).toHaveProperty("valid");
    expect(privateHost.persistSessionStrict).toHaveBeenCalledOnce();
  });

  it("rejects an unknown-placement flood before snapshotting or persisting", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost;
    const sent: DraftP2PMessage[] = [];
    privateHost.adapter = { getViewForSeat: vi.fn(async () => view("only-card")) };
    privateHost.persistSessionStrict = vi.fn(async () => {});
    privateHost.guestSessions.set(1, {
      send: vi.fn(async (message) => { sent.push(message); }),
      onMessage: () => () => {}, onDisconnect: () => () => {}, close: () => {},
    });
    const placements = Object.fromEntries(
      Array.from({ length: 1002 }, (_, index) => [
        `unknown-${index}`,
        { zone: "deck" as const, row: 0, column: 0, order: index },
      ]),
    );

    privateHost.runDetachedMutation("guest message", () => privateHost.handleGuestMessage(1, {
      type: "draft_workspace_update",
      workspaceState: workspace(placements),
    }));
    await privateHost.mutationQueue;

    expect(sent).toEqual([{ type: "draft_error", reason: "placements cannot exceed 1001 entries" }]);
    expect(privateHost.perSeatWorkspaceSnapshots.has(1)).toBe(false);
    expect(privateHost.persistSessionStrict).not.toHaveBeenCalled();
  });

  it("rechecks the source session after a workspace view await", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost & {
      applyWorkspaceUpdate: (
        seat: number,
        state: DraftWorkspaceState,
        sourceSession?: SessionStub,
      ) => Promise<void>;
    };
    const blockedView = deferred<DraftPlayerView>();
    privateHost.adapter = {
      getViewForSeat: vi.fn()
        .mockResolvedValueOnce(blockedView.promise)
        .mockResolvedValue(view("later")),
    };
    privateHost.persistSessionStrict = vi.fn(async () => {});
    const oldSession: SessionStub = {
      send: vi.fn(async () => {}), onMessage: () => () => {}, onDisconnect: () => () => {}, close: () => {},
    };
    const newSession: SessionStub = {
      send: vi.fn(async () => {}), onMessage: () => () => {}, onDisconnect: () => () => {}, close: () => {},
    };
    privateHost.guestSessions.set(1, oldSession);
    const stale = privateHost.applyWorkspaceUpdate(1, workspace(), oldSession);
    privateHost.guestSessions.set(1, newSession);
    blockedView.resolve(view("stale"));
    await stale;
    expect(privateHost.perSeatWorkspaceSnapshots.has(1)).toBe(false);
    expect(privateHost.persistSessionStrict).not.toHaveBeenCalled();

    await privateHost.applyWorkspaceUpdate(1, workspace(), newSession);
    expect(privateHost.perSeatWorkspaceSnapshots.get(1)?.placements).toHaveProperty("later");
    expect(privateHost.persistSessionStrict).toHaveBeenCalledOnce();
  });

  it("normalizes stale, missing, and collision identities without error", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost;
    privateHost.adapter = { getViewForSeat: vi.fn(async () => view("kept", "missing", "collision")) };
    privateHost.persistSessionStrict = vi.fn(async () => {});

    await host.updateHostWorkspace(workspace(
      {
        kept: { zone: "sideboard", row: 1, column: 4, order: 7 },
        stale: { zone: "deck", row: 0, column: 0, order: 2 },
        collision: { zone: "deck", row: 0, column: 0, order: 3 },
      },
      [
        { instanceId: "virtual", name: "Island" },
        { instanceId: "collision", name: "Plains" },
      ],
    ));

    expect(host.getHostWorkspaceState()).toEqual(workspace(
      {
        kept: { zone: "sideboard", row: 1, column: 4, order: 7 },
        collision: { zone: "deck", row: 0, column: 0, order: 3 },
        missing: { zone: "deck", row: 0, column: 0, order: 4 },
        virtual: { zone: "deck", row: 0, column: 0, order: 5 },
      },
      [{ instanceId: "virtual", name: "Island" }],
    ));
    expect(privateHost.persistSessionStrict).toHaveBeenCalledOnce();
  });

  it("restores and reconciles seat zero before returning the view", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost;
    const importedView = deferred<DraftPlayerView>();
    privateHost.adapter = { importSession: vi.fn(() => importedView.promise), getViewForSeat: vi.fn() };
    privateHost.persistSession = vi.fn();
    privateHost.recoverSettlementOutbox = vi.fn(async () => {});
    const restore = host.restoreFromPersisted(persisted({
      0: workspace({ stale: { zone: "deck", row: 0, column: 0, order: 0 } }),
      1: workspace(),
    }));

    expect(host.getHostWorkspaceState()?.placements).toHaveProperty("stale");
    importedView.resolve(view("fresh"));
    await expect(restore).resolves.toBeDefined();
    expect(host.getHostWorkspaceState()?.placements).toEqual({
      fresh: { zone: "deck", row: 0, column: 0, order: 0 },
    });
    expect(privateHost.perSeatWorkspaceSnapshots.has(1)).toBe(true);
    expect(privateHost.persistSession).toHaveBeenCalledOnce();
  });

  it("leaves unchanged and missing seat-zero restores unpersisted", async () => {
    const cases: Array<Record<number, DraftWorkspaceState>> = [
      { 0: workspace({ kept: { zone: "deck", row: 0, column: 0, order: 0 } }) },
      {},
    ];
    for (const snapshots of cases) {
      const host = makeHost();
      const privateHost = host as unknown as PrivateHost;
      privateHost.adapter = {
        importSession: vi.fn(async () => view(...("0" in snapshots ? ["kept"] : []))),
        getViewForSeat: vi.fn(),
      };
      privateHost.persistSession = vi.fn();
      privateHost.recoverSettlementOutbox = vi.fn(async () => {});

      await host.restoreFromPersisted(persisted(snapshots));
      expect(privateHost.persistSession).not.toHaveBeenCalled();
      expect(host.getHostWorkspaceState()).toEqual("0" in snapshots ? snapshots[0] : null);
    }
  });

  it("drops only a corrupt seat-zero restore before returning", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost;
    privateHost.adapter = {
      importSession: vi.fn(async () => view()),
      getViewForSeat: vi.fn(),
    };
    privateHost.persistSession = vi.fn();
    privateHost.recoverSettlementOutbox = vi.fn(async () => {});

    await host.restoreFromPersisted(persisted({
      0: workspace({ bad: { zone: "deck", row: 2, column: 0, order: 0 } }),
      1: workspace(),
    }));

    expect(host.getHostWorkspaceState()).toBeNull();
    expect(privateHost.perSeatWorkspaceSnapshots.has(1)).toBe(true);
    expect(privateHost.persistSession).toHaveBeenCalledOnce();
  });

  it("persists the complete per-seat snapshot map", async () => {
    const host = makeHost("persisted");
    const privateHost = host as unknown as PrivateHost;
    privateHost.adapter = {
      getViewForSeat: vi.fn(async (seat) => view(`card-${seat}`)),
    };
    privateHost.perSeatWorkspaceSnapshots.set(1, workspace());

    await host.updateHostWorkspace(workspace());
    await privateHost.persistQueue;

    expect(persistenceMocks.saveDraftHostSession).toHaveBeenCalledWith(
      "persisted",
      expect.objectContaining({
        perSeatWorkspaceSnapshots: {
          0: expect.objectContaining({ placements: expect.objectContaining({ "card-0": expect.anything() }) }),
          1: workspace(),
        },
      }),
    );
  });

  it("sends explicit null to a new guest", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost;
    const sent: DraftP2PMessage[] = [];
    const session: SessionStub = {
      send: vi.fn(async (message) => { sent.push(message); }),
      onMessage: () => () => {}, onDisconnect: () => () => {},
      close: () => {},
    };

    await privateHost.handleNewGuest(session, "Guest");

    expect(sent[0]).toMatchObject({
      type: "draft_welcome",
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      workspaceState: null,
    });
  });

  it("reconnects with only the authenticated seat snapshot", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost;
    const acknowledgement = deferred<DraftP2PMessage>();
    const lookups: number[] = [];
    privateHost.draftStarted = true;
    privateHost.seatTokens.set(1, "seat-1-token");
    privateHost.disconnectedSeats.set(1, { disconnectedAt: 0, timer: null });
    privateHost.perSeatWorkspaceSnapshots = new Map([
      [0, workspace({ host: { zone: "deck", row: 0, column: 0, order: 0 } })],
      [1, workspace({ own: { zone: "deck", row: 0, column: 0, order: 0 } })],
      [2, workspace({ other: { zone: "deck", row: 0, column: 0, order: 0 } })],
    ]);
    privateHost.adapter = {
      setSeatConnected: vi.fn(async () => {}),
      getViewForSeat: vi.fn(async (seat) => {
        lookups.push(seat);
        return view("own");
      }),
    };
    const session: SessionStub = {
      send: vi.fn(async (message) => {
        if (message.type === "draft_reconnect_ack") acknowledgement.resolve(message);
      }),
      onMessage: () => () => {}, onDisconnect: () => () => {},
      close: () => {},
    };

    void privateHost.handleReconnect(session, "seat-1-token");
    const message = await acknowledgement.promise;

    expect(lookups[0]).toBe(1);
    expect(message).toMatchObject({
      type: "draft_reconnect_ack",
      seatIndex: 1,
      workspaceState: privateHost.perSeatWorkspaceSnapshots.get(1),
    });
    expect(JSON.stringify(message)).not.toContain("host");
    expect(JSON.stringify(message)).not.toContain("other");
  });

  it("guest validates before the session guard and sends complete state", async () => {
    const guest = new P2PDraftGuest(
      {} as never, "host", {} as never, { kind: "new", roomCode: "ABCDE", displayName: "Guest" },
    );
    const invalid = workspace({ bad: { zone: "deck", row: 2, column: 0, order: 0 } });
    await expect(guest.updateWorkspace(invalid)).rejects.toThrow("invalid row");
    await expect(guest.updateWorkspace(workspace())).rejects.toThrow("Not connected");

    const send = vi.fn(async () => {});
    (guest as unknown as { session: { send: typeof send } }).session = { send };
    await guest.updateWorkspace(workspace());
    expect(send).toHaveBeenCalledWith({
      type: "draft_workspace_update",
      workspaceState: workspace(),
    });
    send.mockRejectedValueOnce(new Error("send failed"));
    await expect(guest.updateWorkspace(workspace())).rejects.toThrow("send failed");
  });

  it.each(["new", "reconnect"] as const)("guest resolves persistence before restoration, lifecycle, view, and outbox events (%s)", async (kind) => {
    const conn = new FakeDraftDataConnection();
    const guest = new P2PDraftGuest(
      {} as never, "host", conn as never,
      kind === "new"
        ? { kind, roomCode: "ABCDE", displayName: "Guest" }
        : { kind, roomCode: "ABCDE", displayName: "Guest", draftToken: "token" },
    );
    const events: string[] = [];
    guest.onEvent((event) => events.push(`${event.type}:${"workspaceState" in event ? String(event.workspaceState === null) : ""}`));
    const saved = deferred<void>();
    persistenceMocks.saveDraftGuestSession.mockReturnValueOnce(saved.promise);
    let eventsAtReplay: string[] = [];
    persistenceMocks.loadDraftDeckSubmission.mockImplementationOnce(async () => {
      eventsAtReplay = [...events];
      return null;
    });
    const restored = workspace({ two: { zone: "sideboard", row: 1, column: 2, order: 0 } });
    const fields = {
      draftProtocolVersion: DRAFT_PROTOCOL_VERSION,
      seatIndex: 1,
      view: view(kind === "new" ? "one" : "two"),
      draftCode: "code",
      workspaceState: kind === "new" ? null : restored,
    };
    const message: DraftP2PMessage = kind === "new"
      ? { type: "draft_welcome", draftToken: "token", ...fields }
      : { type: "draft_reconnect_ack", ...fields };
    const initialized = guest.initialize();
    const received = conn.receiveRaw(await encodeDraftWireMessage(message));
    await vi.waitFor(() => expect(persistenceMocks.saveDraftGuestSession).toHaveBeenCalledOnce());
    expect(events).toEqual([]);
    expect(persistenceMocks.loadDraftDeckSubmission).not.toHaveBeenCalled();

    saved.resolve();
    await Promise.all([received, initialized]);

    expect(events).toEqual(kind === "new"
      ? ["workspaceRestored:true", "joined:", "viewUpdated:"]
      : ["workspaceRestored:false", "reconnected:", "viewUpdated:"]);
    expect(eventsAtReplay).toEqual(events);
    guest.dispose();
  });

  it("restores an exact host-owned update through reconnect to the guest", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost;
    const state = workspace({
      card: { zone: "sideboard", row: 1, column: 5, order: 0 },
    });
    privateHost.adapter = {
      setSeatConnected: vi.fn(async () => {}),
      getViewForSeat: vi.fn(async () => view("card")),
    };
    privateHost.persistSessionStrict = vi.fn(async () => {});
    await privateHost.handleGuestMessage(1, {
      type: "draft_workspace_update",
      workspaceState: state,
    });
    await privateHost.mutationQueue;
    privateHost.draftStarted = true;
    privateHost.seatTokens.set(1, "seat-1-token");
    privateHost.disconnectedSeats.set(1, { disconnectedAt: 0, timer: null });
    const acknowledgement = deferred<DraftP2PMessage>();
    const session: SessionStub = {
      send: vi.fn(async (message) => {
        if (message.type === "draft_reconnect_ack") acknowledgement.resolve(message);
      }),
      onMessage: () => () => {}, onDisconnect: () => () => {},
      close: () => {},
    };
    void privateHost.handleReconnect(session, "seat-1-token");

    const message = await acknowledgement.promise;
    expect(message).toMatchObject({ workspaceState: state });
    const conn = new FakeDraftDataConnection();
    const guest = new P2PDraftGuest(
      {} as never,
      "host",
      conn as never,
      { kind: "reconnect", roomCode: "ABCDE", displayName: "Guest", draftToken: "seat-1-token" },
    );
    const restored = deferred<DraftWorkspaceState | null>();
    guest.onEvent((event) => {
      if (event.type === "workspaceRestored") restored.resolve(event.workspaceState);
    });
    const initialized = guest.initialize();
    await conn.receiveRaw(await encodeDraftWireMessage(message));
    await initialized;
    await expect(restored.promise).resolves.toEqual(state);
    guest.dispose();
  });

  it("reconnect defensively drops only a corrupt bound entry", async () => {
    const host = makeHost();
    const privateHost = host as unknown as PrivateHost;
    const acknowledgement = deferred<DraftP2PMessage>();
    privateHost.draftStarted = true;
    privateHost.seatTokens.set(1, "seat-1-token");
    privateHost.disconnectedSeats.set(1, { disconnectedAt: 0, timer: null });
    privateHost.perSeatWorkspaceSnapshots = new Map([
      [1, workspace({ bad: { zone: "deck", row: 2, column: 0, order: 0 } })],
      [2, workspace()],
    ]);
    privateHost.adapter = {
      setSeatConnected: vi.fn(async () => {}),
      getViewForSeat: vi.fn(async () => view()),
    };
    privateHost.persistSessionStrict = vi.fn(async () => {});
    const session: SessionStub = {
      send: vi.fn(async (message) => {
        if (message.type === "draft_reconnect_ack") acknowledgement.resolve(message);
      }),
      onMessage: () => () => {}, onDisconnect: () => () => {},
      close: () => {},
    };

    void privateHost.handleReconnect(session, "seat-1-token");
    await expect(acknowledgement.promise).resolves.toMatchObject({ workspaceState: null });
    expect(privateHost.perSeatWorkspaceSnapshots.has(1)).toBe(false);
    expect(privateHost.perSeatWorkspaceSnapshots.has(2)).toBe(true);
    // The reconnect durably records the handoff before persisting removal of
    // the corrupt workspace entry.
    expect(privateHost.persistSessionStrict).toHaveBeenCalledTimes(3);
  });
});
