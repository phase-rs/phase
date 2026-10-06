import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { useEffect } from "react";
import { MemoryRouter, Route, Routes, useLocation, useNavigate } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { LobbyGame } from "../../adapter/types";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";
import { refuseRealWebSockets } from "../../test/helpers/refusingWebSocket";

/**
 * Drives the real `MultiplayerPage` and `DraftPodPage` behind a real
 * `MemoryRouter`, with `useNavigate` unmocked, so the assertions below
 * observe the actual route the app lands on after a lobby join — not a
 * navigate-call recorded against a stub. `multiplayerDraftStore` runs for
 * real.
 */
const net = vi.hoisted(() => ({
  plan: [] as Array<(code: string, signal?: AbortSignal) => Promise<unknown>>,
  joinRoom: vi.fn(),
}));
vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  joinRoom: net.joinRoom,
}));

const guest = vi.hoisted(() => ({
  ctor: vi.fn(),
  dispose: vi.fn(),
  instances: [] as Array<{ emit: (event: unknown) => void; dispose: ReturnType<typeof vi.fn> }>,
}));
vi.mock("../../adapter/p2p-draft-guest", () => ({
  P2PDraftGuest: vi.fn().mockImplementation(function (...args: unknown[]) {
    guest.ctor(...args);
    let listener: ((event: unknown) => void) | null = null;
    const dispose = vi.fn(() => guest.dispose());
    guest.instances.push({ emit: (event) => listener?.(event), dispose });
    return {
      onEvent: (l: (event: unknown) => void) => {
        listener = l;
        return () => {
          listener = null;
        };
      },
      initialize: vi.fn(async () => {}),
      leave: vi.fn(async () => {}),
      dispose,
      get isRecoveryRevoked() {
        return false;
      },
      view: null,
      seat: null,
      token: null,
    };
  }),
}));

const persist = vi.hoisted(() => ({
  inspectActiveDraftGuest: vi.fn(),
  loadDraftGuestSession: vi.fn(),
  inspectActiveDraftPod: vi.fn((): unknown => ({ type: "absent" })),
  clearActiveDraftPod: vi.fn(),
}));
vi.mock("../../services/draftPersistence", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/draftPersistence")>()),
  inspectActiveDraftGuest: persist.inspectActiveDraftGuest,
  loadDraftGuestSession: persist.loadDraftGuestSession,
  inspectActiveDraftPod: persist.inspectActiveDraftPod,
  clearActiveDraftPod: persist.clearActiveDraftPod,
}));

vi.mock("../../adapter/draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter")>()),
  DraftAdapter: class {
    draftProcedure = vi.fn(async () => draftProcedureFixture());
  },
}));

const harness = vi.hoisted(() => ({
  lobbyAction: null as null | ((props: Record<string, unknown>) => void),
}));
vi.mock("../../components/lobby/LobbyView", () => ({
  LobbyView: (props: Record<string, unknown>) => {
    useEffect(() => {
      harness.lobbyAction?.(props);
      // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);
    return <div data-testid="lobby" />;
  },
}));
vi.mock("../../components/menu/MyDecks", () => ({ MyDecks: () => null }));
vi.mock("../../components/lobby/HostSetup", () => ({ HostSetup: () => null }));
vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
// A full factory mock of ShellContext drops `useDraftShellChrome`, which
// `DraftPodPage` needs to render at all — spread the real module and
// override only `useInShell`.
vi.mock("../../components/chrome/ShellContext", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../components/chrome/ShellContext")>()),
  useInShell: () => false,
}));
vi.mock("../../components/menu/MenuParticles", () => ({ MenuParticles: () => null }));
vi.mock("../../audio/useAudioContext", () => ({ useAudioContext: () => undefined }));
vi.mock("../../stores/cardDataStore", () => ({
  useCardDataStore: { getState: () => ({ warm: vi.fn() }) },
}));
vi.mock("../../stores/gameStore", () => ({
  useGameStore: { setState: vi.fn() },
  saveActiveGame: vi.fn(),
}));
vi.mock("../../services/multiplayerSession", () => ({
  clearWsSession: vi.fn(),
  loadWsSession: vi.fn(() => null),
  saveWsSession: vi.fn(),
}));
vi.mock("../../components/draft/HostControls", () => {
  const empty: readonly [] = [];
  return { HostControls: () => null, useHostDraftTopActions: () => empty };
});
vi.mock("../../components/draft/SetSelector", () => ({ SetSelector: () => null }));

import { MultiplayerPage } from "../MultiplayerPage";
import { DraftPodPage } from "../DraftPodPage";
import { adHocLobbySource, useMultiplayerStore, type LobbySource } from "../../stores/multiplayerStore";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { useConnectivityStore } from "../../stores/connectivityStore";
import { DraftPodGuestAdapter } from "../../adapter/draftPodGuestAdapter";

const ORIGIN = adHocLobbySource("wss://play.example.com/ws") as LobbySource;
const p2pDraftRow: LobbyGame = {
  game_code: "ABC123",
  host_name: "Alice",
  created_at: 1,
  has_password: false,
  is_p2p: true,
  draft_metadata: { setCode: "MKM", draftKind: "Premier" },
};
const locator = { roomCode: "ABCDE", displayName: "Tester", hostPeerId: "phase2-ABCDE", timestamp: Date.now() };

function LocationProbe() {
  const location = useLocation();
  return <div data-testid="loc">{location.pathname + location.search}</div>;
}

/** Captures the router's `navigate`, so a test can leave `/multiplayer` on its own. */
let navigateTo: ((to: string) => void) | null = null;
function NavigationProbe() {
  const navigate = useNavigate();
  useEffect(() => {
    navigateTo = navigate;
    return () => {
      navigateTo = null;
    };
  }, [navigate]);
  return null;
}

function renderApp(initialPath = "/multiplayer") {
  return render(
    <MemoryRouter initialEntries={[initialPath]}>
      <NavigationProbe />
      <Routes>
        <Route path="/multiplayer" element={<MultiplayerPage />} />
        <Route path="/draft-pod" element={<DraftPodPage />} />
        <Route path="/elsewhere" element={<div data-testid="elsewhere" />} />
      </Routes>
      <LocationProbe />
    </MemoryRouter>,
  );
}

function joinResult(code: string) {
  return {
    conn: { peer: `phase2-${code}` },
    peer: { id: `peer-${code}`, destroy: vi.fn() },
    closeConn: vi.fn(),
    destroyPeer: vi.fn(),
  };
}

/** A room join that settles only when released, and only then looks at its abort signal. */
function gateNextJoin(): { release: () => void } {
  const gate = { release: () => {} };
  net.plan.push((code, signal) => new Promise((resolve, reject) => {
    gate.release = () => {
      if (signal?.aborted) reject(new DOMException("Aborted", "AbortError"));
      else resolve(joinResult(code));
    };
  }));
  return gate;
}

type ResolveGuest = NonNullable<ReturnType<typeof useMultiplayerStore.getState>["resolveGuest"]>;

const SETTLE_MS = 250;
// Negative assertions wait this long, so a branch that runs late still shows up.
async function settle() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, SETTLE_MS));
  });
}

const realJoinDraft = useMultiplayerDraftStore.getState().joinDraft;
/** Every `joinDraft` the page makes, so a test can wait for the page's own attempt to settle. */
let pageJoins: Array<ReturnType<typeof realJoinDraft>> = [];

function toastMessages(): string[] {
  return [...useMultiplayerStore.getState().toasts.values()].map((t) => t.message);
}

describe("MultiplayerPage draft pod handoff", () => {
  let socketUrls: string[] = [];

  beforeEach(() => {
    vi.clearAllMocks();
    socketUrls = refuseRealWebSockets();
    harness.lobbyAction = null;
    net.plan = [];
    guest.instances = [];
    net.joinRoom.mockImplementation((code: string, signal?: AbortSignal) => {
      const next = net.plan.shift();
      return next ? next(code, signal) : Promise.resolve(joinResult(code));
    });
    pageJoins = [];
    useMultiplayerDraftStore.setState({
      joinDraft: (config, options) => {
        const joining = realJoinDraft(config, options);
        pageJoins.push(joining);
        return joining;
      },
    });
    persist.inspectActiveDraftGuest.mockReturnValue({ type: "present", meta: locator, capture: locator });
    persist.loadDraftGuestSession.mockResolvedValue({ draftToken: "tok" });
    persist.inspectActiveDraftPod.mockReturnValue({ type: "absent" });
    useMultiplayerStore.setState({
      displayName: "Tester",
      toasts: new Map(),
      resolveGuest: vi.fn(async () => ({
        ok: true as const,
        peerInfo: {
          game_code: "ABC123",
          host_peer_id: "phase2-ABCDE",
          match_config: { match_type: "Bo1" as const },
          player_count: 8,
          filled_seats: 1,
        },
      })),
      lookupJoinTarget: vi.fn(),
    });
  });

  afterEach(async () => {
    const opened = [...socketUrls];
    useMultiplayerDraftStore.setState({ joinDraft: realJoinDraft });
    await useMultiplayerDraftStore.getState().leave(true);
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    expect(opened).toEqual([]);
  });

  it("hands a successful lobby join off to the draft pod as a guest", async () => {
    harness.lobbyAction = (props) => {
      (props.onJoinGame as (...a: unknown[]) => void)("ABC123", ORIGIN, undefined, undefined, p2pDraftRow);
    };
    renderApp();

    await screen.findByText("Waiting for host to start the draft...", undefined, { timeout: 2000 });
    expect(screen.getByTestId("loc").textContent).toBe("/draft-pod?entry=guest");
    expect(screen.queryByTestId("lobby")).toBeNull();
    expect(net.joinRoom).toHaveBeenCalledTimes(1);
    expect(guest.ctor).toHaveBeenCalledTimes(1);
    expect(guest.dispose).not.toHaveBeenCalled();
    expect(useMultiplayerDraftStore.getState().role).toBe("guest");
  });

  it("toasts a failed join's own reason, stays on the lobby, and keeps the saved hosted pod", async () => {
    net.plan.push(async () => {
      throw new Error("test stub: no live PeerJS connection");
    });
    harness.lobbyAction = (props) => {
      (props.onJoinGame as (...a: unknown[]) => void)("ABC123", ORIGIN, undefined, undefined, p2pDraftRow);
    };
    renderApp();

    await waitFor(() => expect(toastMessages()).toContain("test stub: no live PeerJS connection"));
    expect(screen.getByTestId("lobby")).toBeTruthy();
    expect(screen.getByTestId("loc").textContent).toBe("/multiplayer");
    const s = useMultiplayerDraftStore.getState();
    expect(s.role).toBeNull();
    expect(s.phase).toBe("idle");
    expect(persist.clearActiveDraftPod).not.toHaveBeenCalled();
  });

  it("falls back to a generic toast when the failure is the offline sentinel", async () => {
    useMultiplayerStore.setState({
      resolveGuest: vi.fn(async () => {
        useConnectivityStore.setState({ forcedOffline: true });
        return {
          ok: true as const,
          peerInfo: {
            game_code: "ABC123",
            host_peer_id: "phase2-ABCDE",
            match_config: { match_type: "Bo1" as const },
            player_count: 8,
            filled_seats: 1,
          },
        };
      }),
    });
    harness.lobbyAction = (props) => {
      (props.onJoinGame as (...a: unknown[]) => void)("ABC123", ORIGIN, undefined, undefined, p2pDraftRow);
    };
    renderApp();

    await waitFor(() => expect(toastMessages()).toContain("Failed to join draft pod."));
    expect(toastMessages()).not.toContain("offline.startUnavailable");
  });

  function joinFromLobbyRow() {
    harness.lobbyAction = (props) => {
      (props.onJoinGame as (...a: unknown[]) => void)("ABC123", ORIGIN, undefined, undefined, p2pDraftRow);
    };
    renderApp();
  }

  it.each([
    ["failed", "error", "Failed to connect: Could not connect to peer NEWER"],
    ["was kicked", "kicked", "Kicked by host"],
    ["is live", "lobby", null],
  ] as const)(
    "leaves a newer session that %s alone, without a toast, when the lobby join settles after it",
    async (shape, phase, error) => {
      const stale = gateNextJoin();
      joinFromLobbyRow();
      try {
        await waitFor(() => expect(pageJoins).toHaveLength(1));

        if (shape === "failed") {
          net.plan.push(async () => {
            throw new Error("Failed to connect: Could not connect to peer NEWER");
          });
        }
        await realJoinDraft({ kind: "new", roomCode: "NEWER", displayName: "Other" });
        if (shape === "was kicked") guest.instances[0]!.emit({ type: "kicked", reason: "Kicked by host" });
        expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase, error });

        stale.release();
        await act(async () => {
          await pageJoins[0];
        });
        await settle();

        expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase, error });
        for (const g of guest.instances) expect(g.dispose).not.toHaveBeenCalled();
        expect(toastMessages()).toEqual([]);
        expect(screen.getByTestId("loc").textContent).toBe("/multiplayer");
      } finally {
        stale.release();
      }
    },
  );

  it("stays silent and leaves a newer session alone when it starts while the failed lobby join is disposing its adapter", async () => {
    const realDispose = DraftPodGuestAdapter.prototype.dispose;
    let resumeDispose!: () => void;
    const paused = new Promise<void>((resolve) => {
      resumeDispose = resolve;
    });
    const disposeSpy = vi
      .spyOn(DraftPodGuestAdapter.prototype, "dispose")
      .mockImplementationOnce(async function (this: DraftPodGuestAdapter, options) {
        await paused;
        return realDispose.call(this, options);
      });
    net.plan.push(async () => {
      throw new Error("test stub: no live PeerJS connection");
    });
    try {
      joinFromLobbyRow();
      await waitFor(() => expect(disposeSpy).toHaveBeenCalledTimes(1));

      await realJoinDraft({ kind: "new", roomCode: "NEWER", displayName: "Other" });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby", error: null });

      resumeDispose();
      await act(async () => {
        await pageJoins[0];
      });
      await settle();

      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby", error: null });
      expect(guest.instances).toHaveLength(1);
      expect(guest.instances[0]!.dispose).not.toHaveBeenCalled();
      expect(toastMessages()).toEqual([]);
      expect(screen.getByTestId("loc").textContent).toBe("/multiplayer");
    } finally {
      resumeDispose();
      disposeSpy.mockRestore();
    }
  });

  it("leaves a session opened while the failed lobby join's toast is shown alone", async () => {
    net.plan.push(async () => {
      throw new Error("test stub: no live PeerJS connection");
    });
    let newer: ReturnType<typeof realJoinDraft> | null = null;
    const unsubscribe = useMultiplayerStore.subscribe((state) => {
      if (newer === null && state.toasts.size > 0) {
        newer = realJoinDraft({ kind: "new", roomCode: "NEWER", displayName: "Other" });
      }
    });
    try {
      joinFromLobbyRow();
      await waitFor(() => expect(newer).not.toBeNull());
      await act(async () => {
        await expect(newer).resolves.toEqual({ status: "opened" });
      });
      await settle();
    } finally {
      unsubscribe();
    }

    expect(toastMessages()).toEqual(["test stub: no live PeerJS connection"]);
    expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby", error: null });
    expect(guest.instances).toHaveLength(1);
    expect(guest.instances[0]!.dispose).not.toHaveBeenCalled();
  });

  it("does not join a lobby row whose broker lookup settles after the player left the page", async () => {
    let releaseBroker: () => void = () => {};
    useMultiplayerStore.setState({
      resolveGuest: vi.fn<ResolveGuest>(
        () =>
          new Promise((resolve) => {
            releaseBroker = () =>
              resolve({
                ok: true as const,
                peerInfo: {
                  game_code: "ABC123",
                  host_peer_id: "phase2-ABCDE",
                  match_config: { match_type: "Bo1" as const },
                  player_count: 8,
                  filled_seats: 1,
                },
              });
          }),
      ),
    });
    try {
      joinFromLobbyRow();
      await waitFor(() => expect(useMultiplayerStore.getState().resolveGuest).toHaveBeenCalledTimes(1));

      act(() => navigateTo?.("/elsewhere"));
      // Positive control: the page really did leave the lobby before the broker settles.
      expect(screen.queryByTestId("lobby")).toBeNull();

      releaseBroker();
      await settle();

      expect(pageJoins).toEqual([]);
      expect(net.joinRoom).not.toHaveBeenCalled();
      expect(screen.getByTestId("loc").textContent).toBe("/elsewhere");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(toastMessages()).toEqual([]);
    } finally {
      releaseBroker();
    }
  });

  it("withdraws a lobby join the player left while it was connecting", async () => {
    const stale = gateNextJoin();
    joinFromLobbyRow();
    try {
      await waitFor(() => expect(pageJoins).toHaveLength(1));
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "connecting" });

      act(() => navigateTo?.("/elsewhere"));

      stale.release();
      await act(async () => {
        await expect(pageJoins[0]).resolves.toEqual({ status: "superseded" });
      });
      await settle();

      expect(screen.getByTestId("loc").textContent).toBe("/elsewhere");
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: null, phase: "idle" });
      expect(toastMessages()).toEqual([]);
    } finally {
      stale.release();
    }
  });

  it("does not report a lobby join the player left while it was pending", async () => {
    const stale = gateNextJoin();
    joinFromLobbyRow();
    try {
      await waitFor(() => expect(pageJoins).toHaveLength(1));
      await act(async () => {
        await useMultiplayerDraftStore.getState().leave();
      });

      stale.release();
      await act(async () => {
        await pageJoins[0];
      });
      await settle();

      expect(toastMessages()).toEqual([]);
      expect(screen.getByTestId("loc").textContent).toBe("/multiplayer");
    } finally {
      stale.release();
    }
  });

  it("does not join over a pod session started while the broker was resolving the row", async () => {
    let releaseBroker: () => void = () => {};
    useMultiplayerStore.setState({
      resolveGuest: vi.fn<ResolveGuest>(
        () =>
          new Promise((resolve) => {
            releaseBroker = () =>
              resolve({
                ok: true as const,
                peerInfo: {
                  game_code: "ABC123",
                  host_peer_id: "phase2-ABCDE",
                  match_config: { match_type: "Bo1" as const },
                  player_count: 8,
                  filled_seats: 1,
                },
              });
          }),
      ),
    });
    try {
      joinFromLobbyRow();
      await waitFor(() => expect(useMultiplayerStore.getState().resolveGuest).toHaveBeenCalledTimes(1));

      await realJoinDraft({ kind: "new", roomCode: "NEWER", displayName: "Other" });
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby" });

      releaseBroker();
      await settle();

      expect(pageJoins).toEqual([]);
      expect(net.joinRoom).toHaveBeenCalledTimes(1);
      expect(guest.instances[0]!.dispose).not.toHaveBeenCalled();
      expect(useMultiplayerDraftStore.getState()).toMatchObject({ role: "guest", phase: "lobby" });
      expect(screen.getByTestId("loc").textContent).toBe("/multiplayer");
    } finally {
      releaseBroker();
    }
  });

  it("renders the lobby, not a leave-draft screen, for a stale ?view=draft-lobby link", async () => {
    renderApp("/multiplayer?view=draft-lobby");

    expect(await screen.findByTestId("lobby")).toBeTruthy();
    expect(screen.queryByText("Leave Draft")).toBeNull();
  });
});
