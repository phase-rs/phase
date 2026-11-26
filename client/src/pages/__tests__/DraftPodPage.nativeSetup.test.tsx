import "fake-indexeddb/auto";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { clear as idbClear } from "idb-keyval";
import type { ReactNode } from "react";
import { MemoryRouter, Route, Routes, useLocation, useNavigate, type InitialEntry, type NavigateFunction } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";
import { Draft27IncompatibilityError } from "../../adapter/draft-compatibility";

/*
 * The pod page's native setup route, on the real page, pod store, lifecycle
 * store, storage (fake-indexeddb) and routes. The doubles, each stated here:
 * - `ServerDraftAdapter` (`NativeDouble`): the constructor records its URL;
 *   `createDraft` holds seat 0 of NAT001, emits `waitingForPlayers` and
 *   resolves; `joinDraft(code)` holds seat 3 of the code and resolves with a
 *   lobby view; `reconnectDraft(credential)` holds the credential, emits a
 *   drafting view and resolves; `dispose()` counts a call and rejects anything
 *   held. One-shot scripts override each.
 * - `DraftAdapter.draftProcedure`: resolves `draftProcedureFixture()`.
 * - `network/connection`'s `hostRoom` and `joinRoom`: never settle.
 * - `fetch`: never settles; its call with "/draft-pools.json" is the P2P
 *   create's reach.
 * - `draftPersistence.loadNativeDraftCapability`: answers `readFailure` while
 *   `storage.failLoad` is set, else the real record.
 * - `SetSelector`: one button, labelled by `startLabel`, that starts with three
 *   TST packs.
 * - The page chrome: `ScreenChrome` as "Chrome Back", `MenuShell` as its
 *   children, `HostControls` as nothing.
 */
type NativeCredential = { draftCode: string; playerToken: string; seatIndex: number };
type Settle = { resolve: (value?: unknown) => void; reject: (error: Error) => void };
type NativeScript = (double: NativeDoubleShape, settle: Settle, args: unknown[]) => void;
interface NativeDoubleShape {
  readonly url: string;
  seat: NativeCredential | null;
  disposeCount: number;
  readonly createCalls: unknown[];
  readonly joinCalls: unknown[][];
  readonly reconnectCalls: NativeCredential[];
  emit: (event: unknown) => void;
}

const native = vi.hoisted(() => {
  const state = {
    instances: [] as NativeDoubleShape[],
    nextCreate: [] as NativeScript[],
    nextJoin: [] as NativeScript[],
    nextReconnect: [] as NativeScript[],
    lobbyView: null as unknown,
    draftingView: null as unknown,
  };

  class NativeDouble implements NativeDoubleShape {
    seat: NativeCredential | null = null;
    disposeCount = 0;
    readonly createCalls: unknown[] = [];
    readonly joinCalls: unknown[][] = [];
    readonly reconnectCalls: NativeCredential[] = [];
    private listeners = new Set<(event: unknown) => void>();
    private pending = new Set<Settle>();

    constructor(readonly url: string) {
      state.instances.push(this);
    }

    get credential(): NativeCredential | null {
      return this.seat;
    }

    onEvent(listener: (event: unknown) => void): () => void {
      this.listeners.add(listener);
      return () => {
        this.listeners.delete(listener);
      };
    }

    emit(event: unknown): void {
      for (const listener of [...this.listeners]) listener(event);
    }

    private run(script: NativeScript, args: unknown[]): Promise<unknown> {
      return new Promise((resolve, reject) => {
        const settle: Settle = {
          resolve: (value) => {
            if (this.pending.delete(settle)) resolve(value);
          },
          reject: (error) => {
            if (this.pending.delete(settle)) reject(error);
          },
        };
        this.pending.add(settle);
        script(this, settle, args);
      });
    }

    createDraft(settings: unknown): Promise<unknown> {
      this.createCalls.push(settings);
      return this.run(state.nextCreate.shift() ?? NativeDouble.answerCreate, [settings]);
    }

    joinDraft(...args: unknown[]): Promise<unknown> {
      this.joinCalls.push(args);
      return this.run(state.nextJoin.shift() ?? NativeDouble.answerJoin, args);
    }

    reconnectDraft(credential: NativeCredential): Promise<unknown> {
      this.reconnectCalls.push(credential);
      return this.run(state.nextReconnect.shift() ?? NativeDouble.answerReconnect, [credential]);
    }

    dispose(): void {
      this.disposeCount += 1;
      const error = new Error("Adapter disposed before draft started");
      for (const settle of [...this.pending]) settle.reject(error);
      this.listeners.clear();
    }

    static answerCreate: NativeScript = (double, settle) => {
      double.seat = { draftCode: "NAT001", playerToken: "tok-host", seatIndex: 0 };
      double.emit({ type: "waitingForPlayers" });
      settle.resolve();
    };

    static answerJoin: NativeScript = (double, settle, args) => {
      double.seat = { draftCode: args[0] as string, playerToken: "tok-guest", seatIndex: 3 };
      settle.resolve(state.lobbyView);
    };

    static answerReconnect: NativeScript = (double, settle, args) => {
      double.seat = args[0] as NativeCredential;
      double.emit({ type: "draftViewUpdated", view: state.draftingView });
      settle.resolve();
    };
  }

  return { state, NativeDouble };
});

const mocks = vi.hoisted(() => ({
  draftProcedure: vi.fn(),
  hostRoom: vi.fn(() => new Promise(() => {})),
  joinRoom: vi.fn(() => new Promise(() => {})),
  failLoad: false,
}));

vi.mock("../../adapter/server-draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/server-draft-adapter")>()),
  ServerDraftAdapter: native.NativeDouble,
}));

vi.mock("../../adapter/draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter")>()),
  DraftAdapter: class {
    draftProcedure = mocks.draftProcedure;
  },
}));

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  hostRoom: mocks.hostRoom,
  joinRoom: mocks.joinRoom,
}));

vi.mock("../../services/draftPersistence", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../services/draftPersistence")>();
  return {
    ...actual,
    loadNativeDraftCapability: async (...args: Parameters<typeof actual.loadNativeDraftCapability>) => {
      if (mocks.failLoad) return { type: "readFailure", error: new Error("IndexedDB read failed") } as const;
      return actual.loadNativeDraftCapability(...args);
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
    HostControls: () => null,
    useHostDraftTopActions: () => empty,
  };
});
vi.mock("../../components/draft/SetSelector", () => ({
  SetSelector: ({ startLabel, onStartDraft }: { startLabel: string; onStartDraft: (packs: Array<{ code: string; name: string }>) => void }) => (
    <button onClick={() => onStartDraft([{ code: "TST", name: "Test" }, { code: "TST", name: "Test" }, { code: "TST", name: "Test" }])}>
      {startLabel}
    </button>
  ),
}));

import { DraftPodPage } from "../DraftPodPage";
import { useDraftPodStore } from "../../stores/draftPodStore";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { useConnectivityStore } from "../../stores/connectivityStore";
import { getDraftStore, publishActiveNativeDraft, saveNativeDraftCapability } from "../../services/draftPersistence";

const ENDPOINT = "ws://native.example:9374/ws";
const POINTER_KEY = "phase-active-draft-guest";
const SAVED_SEAT: NativeCredential = { draftCode: "NAT001", playerToken: "tok-guest", seatIndex: 3 };
const LOBBY_TEXT = "Waiting for host to start the draft...";
const INCOMPATIBLE_TEXT = "This draft is incompatible with your protocol version.";
const CONNECTION_TEXT = "Connection Error";

function view(fields: Record<string, unknown>): unknown {
  return {
    status: "Lobby",
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
    ...fields,
  };
}
native.state.lobbyView = view({ status: "Lobby" });
native.state.draftingView = view({ status: "Drafting", pick_number: 7 });

const fetchDouble = vi.fn((_input: unknown) => new Promise<Response>(() => {}));

let pathname = "";
let navigateTo: NavigateFunction | null = null;
function Where() {
  const location = useLocation();
  pathname = location.pathname + location.search;
  navigateTo = useNavigate();
  return null;
}

function renderApp(entries: InitialEntry[]) {
  return render(
    <MemoryRouter initialEntries={entries} initialIndex={entries.length - 1}>
      <Where />
      <Routes>
        <Route path="/draft-pod" element={<DraftPodPage />} />
        <Route path="/" element={<div data-testid="home" />} />
      </Routes>
    </MemoryRouter>,
  );
}

const settle = (ms = 60) => act(async () => { await new Promise((done) => setTimeout(done, ms)); });

/** Admits `endpoint` and answers the navigation the admission names. */
function admittedEntry(endpoint = ENDPOINT): { pathname: string; search: string; state: { nativeDraftSetup: number } } {
  const admission = useDraftPodStore.getState().admitNativeDraftSetup(endpoint);
  if (admission.type !== "admitted") throw new Error(`refused ${endpoint}`);
  const route = new URL(admission.to, "http://page.test");
  return { pathname: route.pathname, search: route.search, state: admission.state };
}

/** A committed native seat record and its published pointer, as a committed native join leaves them. */
async function storeNativeSeat(): Promise<string> {
  const pointer = await saveNativeDraftCapability({
    capabilityId: "cap-N",
    endpoint: ENDPOINT,
    draftCode: SAVED_SEAT.draftCode,
    playerToken: SAVED_SEAT.playerToken,
    seatIndex: SAVED_SEAT.seatIndex,
    participant: "guest",
  });
  publishActiveNativeDraft(pointer);
  return localStorage.getItem(POINTER_KEY)!;
}

const lastCreate = () => native.state.instances[0]!.createCalls[0] as Record<string, unknown>;

describe("DraftPodPage native setup route", () => {
  beforeEach(async () => {
    vi.clearAllMocks();
    native.state.instances.length = 0;
    native.state.nextCreate.length = 0;
    native.state.nextJoin.length = 0;
    native.state.nextReconnect.length = 0;
    mocks.failLoad = false;
    mocks.draftProcedure.mockReset().mockResolvedValue(draftProcedureFixture());
    vi.stubGlobal("fetch", fetchDouble);
    localStorage.clear();
    await idbClear(getDraftStore());
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    useMultiplayerStore.setState({ lastPodListingPublic: null });
    await useMultiplayerDraftStore.getState().leave(true);
    useMultiplayerDraftStore.getState().reset();
    useDraftPodStore.getState().reset();
    useDraftPodStore.setState({ hostDisplayName: "Host", guestDisplayName: "Alice" });
  });

  afterEach(async () => {
    cleanup();
    await useMultiplayerDraftStore.getState().leave(true);
    useDraftPodStore.getState().reset();
    vi.unstubAllGlobals();
    localStorage.clear();
    navigateTo = null;
  });

  const openHostForm = async () => {
    await userEvent.setup().click(await screen.findByRole("button", { name: /Host a Pod/ }));
  };
  const openJoinForm = async () => {
    await userEvent.setup().click(await screen.findByRole("button", { name: /Join a Pod/ }));
  };
  const createPod = async () => {
    await userEvent.setup().click(await screen.findByRole("button", { name: "Create Pod" }));
  };

  describe("an admitted setup's host form", () => {
    it("creates a server-hosted pod with the form's settings, listed at eight seats", async () => {
      renderApp(["/", admittedEntry(" WS://Native.Example:9374/ws ")]);
      await openHostForm();
      // Reach: native mode.
      expect(screen.queryByRole("button", { name: "Cube" })).toBeNull();
      expect(screen.queryByRole("checkbox", { name: "Set password" })).toBeNull();
      expect(screen.queryByText(/Lobby listing supports/)).toBeNull();
      expect(screen.getByRole("checkbox", { name: "List in lobby" })).toBeEnabled();

      await createPod();
      await waitFor(() => expect(useMultiplayerDraftStore.getState().phase).toBe("lobby"));

      expect(native.state.instances).toHaveLength(1);
      expect(native.state.instances[0]!.url).toBe(ENDPOINT);
      expect(native.state.instances[0]!.createCalls).toStrictEqual([{
        displayName: "Host",
        source: { type: "Uniform", data: { set_codes: ["TST", "TST", "TST"] } },
        kind: "Premier",
        public: true,
        tournamentFormat: "Swiss",
        podPolicy: "Competitive",
        podSize: 8,
      }]);
      expect(mocks.hostRoom).not.toHaveBeenCalled();
      expect(fetchDouble).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "host", transport: "native", phase: "lobby" });
    });

    it("lists a seven-seat server-hosted pod and sends no password", async () => {
      renderApp(["/", admittedEntry()]);
      await openHostForm();
      act(() => {
        useDraftPodStore.setState({
          config: { ...useDraftPodStore.getState().config, podSize: 7 },
          listing: { isPublic: true, password: "secret", roomName: "Room" },
        });
      });

      await createPod();
      // Reach: the native create was sent.
      await waitFor(() => expect(native.state.instances[0]?.createCalls).toHaveLength(1));

      expect(lastCreate()).toMatchObject({ public: true, podSize: 7 });
      expect(Object.keys(lastCreate())).not.toContain("password");
      expect(Object.keys(lastCreate())).not.toContain("timerSeconds");
    });

    it("shows a native start's failure through the one error hook", async () => {
      renderApp(["/", admittedEntry()]);
      await openHostForm();
      native.state.nextCreate.push((_double, settle) => {
        settle.reject(new Draft27IncompatibilityError({ kind: "incompatible", code: "fifo_flow", message: "raw english" }));
      });

      await createPod();
      // Reach: the native create was sent.
      await waitFor(() => expect(native.state.instances[0]?.createCalls).toHaveLength(1));

      expect(await screen.findByText(INCOMPATIBLE_TEXT)).toBeInTheDocument();
      expect(screen.queryByText("raw english")).toBeNull();

      native.state.nextCreate.push((_double, settle) => {
        settle.reject(new Error("Draft is full"));
      });
      await createPod();

      expect(await screen.findByText("Draft is full")).toBeInTheDocument();
      expect(native.state.instances).toHaveLength(2);
    });
  });

  describe("an admitted setup's join form", () => {
    it("joins on the admitted server and lands in the lobby", async () => {
      useDraftPodStore.setState({ joinCode: "NAT123" });
      renderApp(["/", admittedEntry()]);
      await openJoinForm();

      await userEvent.setup().click(screen.getByRole("button", { name: "Join Pod" }));
      await waitFor(() => expect(useMultiplayerDraftStore.getState().phase).toBe("lobby"));

      expect(native.state.instances).toHaveLength(1);
      expect(native.state.instances[0]!.url).toBe(ENDPOINT);
      expect(native.state.instances[0]!.joinCalls).toEqual([["NAT123", "Alice", undefined]]);
      expect(mocks.joinRoom).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", transport: "native", phase: "lobby" });
      expect(await screen.findByText(LOBBY_TEXT)).toBeInTheDocument();
    });

    // The real adapter never settles a join the server answers with
    // `PasswordRequired`; what the page shows for that refusal is the native
    // commands' typed refusal, which this route does not have yet.
    it("offers no password for a protected pod, and Back leaves its join cleanly", async () => {
      useDraftPodStore.setState({ joinCode: "PROT12" });
      native.state.nextJoin.push(() => {});
      renderApp(["/", admittedEntry()]);
      await openJoinForm();
      act(() => {
        useDraftPodStore.setState({ listing: { ...useDraftPodStore.getState().listing, password: "secret" } });
      });

      await userEvent.setup().click(screen.getByRole("button", { name: "Join Pod" }));
      await waitFor(() => expect(native.state.instances[0]?.joinCalls).toEqual([["PROT12", "Alice", undefined]]));
      // Reach: the join is connecting on the admitted server.
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", transport: "native", phase: "connecting" });

      await userEvent.setup().click(screen.getByRole("button", { name: "Chrome Back" }));
      await settle(60);

      expect(pathname).toBe("/");
      expect(native.state.instances[0]!.disposeCount).toBe(1);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(localStorage.getItem(POINTER_KEY)).toBeNull();
    });

    it("leaves the native seat from its lobby", async () => {
      useDraftPodStore.setState({ joinCode: "NAT123" });
      renderApp(["/", admittedEntry()]);
      await openJoinForm();
      await userEvent.setup().click(screen.getByRole("button", { name: "Join Pod" }));
      await waitFor(() => expect(useMultiplayerDraftStore.getState().phase).toBe("lobby"));
      // Reach: joined natively, with its pointer published.
      await waitFor(() => expect(localStorage.getItem(POINTER_KEY)).not.toBeNull());

      await userEvent.setup().click(await screen.findByRole("button", { name: "Leave" }));
      await settle(80);

      expect(pathname).toBe("/");
      expect(native.state.instances[0]!.disposeCount).toBe(1);
      expect(localStorage.getItem(POINTER_KEY)).toBeNull();
      expect(useDraftPodStore.getState().nativeSetup).toBeNull();
    });
  });

  describe("navigation without an agreeing setup", () => {
    it.each(["no history state", "a token the store no longer holds"] as const)("is the P2P page with %s", async (entry) => {
      const first = admittedEntry("ws://a.example:9374/ws");
      const second = admittedEntry("ws://b.example:9374/ws");
      // Reach: the store holds the second admission, one generation on.
      expect(second.state.nativeDraftSetup).toBe(first.state.nativeDraftSetup + 1);
      renderApp(["/", entry === "no history state" ? "/draft-pod?entry=native" : first]);
      await openHostForm();

      expect(screen.getByRole("button", { name: "Cube" })).toBeInTheDocument();
      await createPod();
      await waitFor(() => expect(fetchDouble).toHaveBeenCalledWith("/draft-pools.json"));
      expect(native.state.instances).toHaveLength(0);
    });

    it("is the P2P page on plain /draft-pod carrying an admitted token", async () => {
      const admitted = admittedEntry();
      // Reach: the store holds the admitted generation.
      expect(useDraftPodStore.getState().nativeSetup?.generation).toBe(admitted.state.nativeDraftSetup);
      renderApp(["/", { pathname: "/draft-pod", state: admitted.state }]);
      await openHostForm();

      expect(screen.getByRole("button", { name: "Cube" })).toBeInTheDocument();
      await createPod();
      await waitFor(() => expect(fetchDouble).toHaveBeenCalledWith("/draft-pools.json"));
      expect(native.state.instances).toHaveLength(0);
    });
  });

  describe("the native route's entry", () => {
    it("recovers the tab's own seat with no admission, starting no pod operation", async () => {
      await storeNativeSeat();
      renderApp(["/", "/draft-pod?entry=native"]);
      await waitFor(() => expect(useMultiplayerDraftStore.getState().phase).toBe("drafting"));

      expect(native.state.instances).toHaveLength(1);
      expect(native.state.instances[0]!.reconnectCalls).toEqual([SAVED_SEAT]);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", transport: "native", phase: "drafting" });
      expect(native.state.instances[0]!.createCalls).toEqual([]);
      expect(native.state.instances[0]!.joinCalls).toEqual([]);
    });

    it("leaves a native seat alone on plain /draft-pod", async () => {
      const pointerText = await storeNativeSeat();
      renderApp(["/", "/draft-pod"]);
      await settle(80);

      expect(native.state.instances).toHaveLength(0);
      expect(localStorage.getItem(POINTER_KEY)).toBe(pointerText);
    });

    it("ends the recovered seat with its route", async () => {
      await storeNativeSeat();
      renderApp(["/", "/draft-pod?entry=native"]);
      // Reach: the entry recovered the native seat.
      await waitFor(() => expect(useMultiplayerDraftStore.getState().transport).toBe("native"));

      await act(async () => { navigateTo!("/"); });
      await settle(60);

      expect(native.state.instances[0]!.disposeCount).toBe(1);
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      // The seat's pointer is kept, as the resume refreshed it.
      expect(JSON.parse(localStorage.getItem(POINTER_KEY)!)).toMatchObject({ transport: "native", capabilityId: "cap-N" });
    });
  });

  describe("a failed native entry recovery", () => {
    const failReconnect = (error: Error) => {
      native.state.nextReconnect.push((_double, settle) => {
        settle.reject(error);
      });
    };

    it.each(["an admitted setup", "no history state", "a token the store does not hold"] as const)(
      "shows on the choice screen with %s, and never on the forms",
      async (entry) => {
        await storeNativeSeat();
        failReconnect(new Error("Seat not found"));
        const admitted = admittedEntry();
        renderApp(["/", entry === "an admitted setup"
          ? admitted
          : entry === "no history state"
            ? "/draft-pod?entry=native"
            : { ...admitted, state: { nativeDraftSetup: admitted.state.nativeDraftSetup + 1 } }]);
        // Reach: the entry's reconnect was sent once.
        await waitFor(() => expect(native.state.instances[0]?.reconnectCalls).toHaveLength(1));

        expect(await screen.findByText("Seat not found")).toBeInTheDocument();
        await openHostForm();
        expect(screen.queryByText("Seat not found")).toBeNull();
        await userEvent.setup().click(screen.getByRole("button", { name: "← Back" }));
        await openJoinForm();
        expect(screen.queryByText("Seat not found")).toBeNull();
      },
    );

    it("translates an incompatible seat's refusal", async () => {
      await storeNativeSeat();
      failReconnect(new Draft27IncompatibilityError({ kind: "incompatible", code: "fifo_flow", message: "raw english" }));
      renderApp(["/", "/draft-pod?entry=native"]);
      await waitFor(() => expect(native.state.instances[0]?.reconnectCalls).toHaveLength(1));

      expect(await screen.findByText(INCOMPATIBLE_TEXT)).toBeInTheDocument();
      expect(screen.queryByText("raw english")).toBeNull();
    });

    it("shows the connection message for a record it could not read", async () => {
      const pointerText = await storeNativeSeat();
      mocks.failLoad = true;
      renderApp(["/", "/draft-pod?entry=native"]);

      expect(await screen.findByText(CONNECTION_TEXT)).toBeInTheDocument();
      expect(native.state.instances).toHaveLength(0);
      expect(localStorage.getItem(POINTER_KEY)).toBe(pointerText);
    });

    it("is not shown on a later plain /draft-pod mount", async () => {
      await storeNativeSeat();
      failReconnect(new Error("Seat not found"));
      renderApp(["/", "/draft-pod?entry=native"]);
      expect(await screen.findByText("Seat not found")).toBeInTheDocument();

      await act(async () => { navigateTo!("/"); });
      await settle(40);
      await act(async () => { navigateTo!("/draft-pod"); });
      await settle(80);
      // Any later render of the route, such as a window resize.
      await act(async () => { window.dispatchEvent(new Event("resize")); });
      await settle(40);

      expect(pathname).toBe("/draft-pod");
      expect(useDraftPodStore.getState().nativeRecoveryFailure).toMatchObject({ error: "Seat not found" });
      expect(screen.queryByText("Seat not found")).toBeNull();
    });
  });
});
