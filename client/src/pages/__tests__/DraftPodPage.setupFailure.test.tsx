import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";
import { refuseRealWebSockets } from "../../test/helpers/refusingWebSocket";

/**
 * The setup-failure reason shown on the error screen, driven through the real
 * draft stores and adapters: a listing the lobby refuses, an unlisted pod
 * whose hosting fails, a guest whose join fails, and the offline sentinel.
 *
 * Real: `draftPodStore`, `multiplayerDraftStore`, the host and guest pod
 * adapters, `multiplayerStore`, `brokerClient`, `connectivityStore`.
 */

const connectionState = vi.hoisted(() => ({ hostRoomShouldFail: false }));

vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  hostRoom: vi.fn(async () => {
    if (connectionState.hostRoomShouldFail) {
      throw new Error("signaling down");
    }
    return {
      roomCode: "PDABC",
      peerId: "phase2-PDABC",
      peer: { destroy: vi.fn(), on: vi.fn() },
      onGuestConnected: () => () => {},
      destroy: vi.fn(),
    };
  }),
  joinRoom: vi.fn(async () => {
    throw new Error("guest dial refused");
  }),
}));

vi.mock("../../services/draftPersistence", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/draftPersistence")>()),
  loadDraftHostSession: vi.fn(async () => null),
  saveDraftHostSession: vi.fn(async () => {}),
  clearDraftHostSession: vi.fn(async () => {}),
}));

const mocks = vi.hoisted(() => ({
  draftProcedure: vi.fn(),
  openPhaseSocket: vi.fn(),
}));

vi.mock("../../adapter/draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter")>()),
  DraftAdapter: class {
    draftProcedure = mocks.draftProcedure;
  },
}));

vi.mock("../../services/openPhaseSocket", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/openPhaseSocket")>()),
  openPhaseSocket: mocks.openPhaseSocket,
}));

vi.mock("../../components/chrome/ScreenChrome", () => ({ ScreenChrome: () => null }));
vi.mock("../../components/menu/MenuShell", () => ({
  MenuShell: ({ children }: { children: ReactNode }) => <>{children}</>,
}));
vi.mock("../../components/draft/HostControls", () => {
  const emptyTopActions: readonly [] = [];
  return {
    HostControls: () => null,
    useHostDraftTopActions: (_options: { enabled: boolean }) => emptyTopActions,
  };
});

import { DraftPodPage } from "../DraftPodPage";
import { useDraftPodStore } from "../../stores/draftPodStore";
import { DRAFT_OFFLINE_ERROR, useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { useConnectivityStore } from "../../stores/connectivityStore";

/** A `LobbyOnly` probe answer for `resolveP2PBroker`'s own resolution step —
 *  bypassing it is what keeps the fake lobby socket below to exactly the one
 *  `openBrokerClient` itself opens (the `draftPodStore.lobbyListing.test.ts`
 *  precedent). Declared bare, since only `serverInfo.mode` is read. */
const ensureSubscriptionSocketMock = vi.fn();

/** A lobby-socket double: records sent frames and lets a test deliver a reply. */
const socketState = vi.hoisted(() => {
  class FakeLobbySocket extends EventTarget {
    readyState = 1;
    onopen: unknown = null;
    onmessage: unknown = null;
    onerror: unknown = null;
    onclose: unknown = null;
    sent: string[] = [];
    send = vi.fn((data: string) => {
      this.sent.push(data);
    });
    close = vi.fn(() => {
      this.readyState = 3;
      this.dispatchEvent(new Event("close"));
    });
    deliver(frame: unknown): void {
      this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(frame) }));
    }
  }
  return {
    FakeLobbySocket,
    sockets: [] as InstanceType<typeof FakeLobbySocket>[],
  };
});

/** Parsed wire frames a fake socket's `send` calls carried, minus keepalive. */
function frames(socket: { sent: string[] }): { type: string; data?: unknown }[] {
  return socket.sent
    .map((raw) => JSON.parse(raw) as { type: string; data?: unknown })
    .filter((frame) => frame.type !== "Ping");
}

let socketUrls: string[] = [];

function stubFetch(): void {
  vi.stubGlobal("__DRAFT_POOLS_URL__", "/draft-pools.json");
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({
      ok: true,
      status: 200,
      json: async () => ({ tst: { code: "TST" } }),
    })),
  );
}

function configureHost(overrides: { podSize?: number } = {}): void {
  useDraftPodStore.setState((prev) => ({
    config: {
      ...prev.config,
      packs: [{ code: "TST", name: "Test Set" }],
      setCode: "TST",
      podSize: overrides.podSize ?? 6,
    },
  }));
}

function renderPage() {
  return render(
    <MemoryRouter initialEntries={["/draft-pod"]}>
      <DraftPodPage />
    </MemoryRouter>,
  );
}

async function enterHostMode(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.click(screen.getByRole("button", { name: /Host a Pod/ }));
  await user.type(screen.getByPlaceholderText(/name/i), "Host");
}

function expectErrorScreenMounted(): void {
  expect(screen.getByRole("button", { name: "Return to Menu" })).toBeInTheDocument();
}

describe("DraftPodPage setup-failure reason", () => {
  beforeEach(async () => {
    vi.clearAllMocks();
    socketUrls = refuseRealWebSockets();
    socketState.sockets = [];
    connectionState.hostRoomShouldFail = false;
    stubFetch();
    mocks.draftProcedure.mockResolvedValue(draftProcedureFixture({
      pod_size: 6,
      human_seats: 1,
      min_pod_size: 2,
      max_pod_size: 8,
      allowed_pod_sizes: [2, 3, 4, 5, 6, 7, 8],
      packs_per_player: 3,
      cards_per_pick: 1,
      distribution: "PickAndPass",
      min_deck_size: 40,
      match_config: { match_type: "Bo1" },
    }));
    mocks.openPhaseSocket.mockReset().mockImplementation(async () => {
      const ws = new socketState.FakeLobbySocket();
      socketState.sockets.push(ws);
      return {
        ws,
        serverInfo: {
          version: "test",
          buildCommit: "test",
          mode: "LobbyOnly",
          protocolVersion: 0,
          lobbyProtocolVersion: 0,
        },
        close: () => ws.close(),
      };
    });
    ensureSubscriptionSocketMock.mockReset().mockImplementation(async () => ({
      serverInfo: { mode: "LobbyOnly" },
    }));
    useMultiplayerStore.setState({ ensureSubscriptionSocket: ensureSubscriptionSocketMock });
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
    useDraftPodStore.getState().reset();
    await useMultiplayerDraftStore.getState().leave();
    useMultiplayerDraftStore.getState().reset();
  });

  afterEach(async () => {
    await useMultiplayerDraftStore.getState().leave();
    cleanup();
    vi.unstubAllGlobals();
    const opened = [...socketUrls];
    expect(opened).toEqual([]);
  });

  it("shows the lobby's reason when it refuses a pod's listing", async () => {
    renderPage();
    const user = userEvent.setup();
    await enterHostMode(user);
    configureHost({ podSize: 6 });
    useDraftPodStore.getState().setListing({ isPublic: true });

    const creating = useDraftPodStore.getState().createPod();
    await waitFor(() => expect(socketState.sockets).toHaveLength(1));
    const socket = socketState.sockets[0]!;
    await waitFor(() =>
      expect(frames(socket).some((f) => f.type === "CreateGameWithSettings")).toBe(true),
    );
    socket.deliver({ type: "Error", data: { message: "Room name is not allowed on the public lobby." } });
    await creating;

    await waitFor(() =>
      expect(screen.getByText("Room name is not allowed on the public lobby.")).toBeInTheDocument(),
    );
    expect(screen.queryByText("Connection Error")).toBeNull();
    expectErrorScreenMounted();
  });

  it("shows the reason when an unlisted pod fails to start hosting", async () => {
    renderPage();
    const user = userEvent.setup();
    await enterHostMode(user);
    configureHost({ podSize: 6 });
    useDraftPodStore.getState().setListing({ isPublic: false });
    connectionState.hostRoomShouldFail = true;

    await useDraftPodStore.getState().createPod();

    await waitFor(() => expect(screen.getByText("signaling down")).toBeInTheDocument());
    expect(screen.queryByText("Connection Error")).toBeNull();
    expectErrorScreenMounted();
  });

  it("shows the reason when a guest's join fails", async () => {
    renderPage();
    useDraftPodStore.setState({ joinCode: "ABCDE", guestDisplayName: "Guest" });

    await useDraftPodStore.getState().joinPod();
    await waitFor(() => expect(useMultiplayerDraftStore.getState().phase).not.toBe("connecting"));

    await waitFor(() => expect(screen.getByText("guest dial refused")).toBeInTheDocument());
    expect(screen.queryByText("Connection Error")).toBeNull();
    expectErrorScreenMounted();
  });

  it("shows the offline notice translated, never its key", async () => {
    useDraftPodStore.setState({ joinCode: "ABCDE", guestDisplayName: "Guest" });
    await useDraftPodStore.getState().joinPod();
    await waitFor(() => expect(useMultiplayerDraftStore.getState().phase).not.toBe("connecting"));

    useConnectivityStore.setState({ forcedOffline: true });
    const outcome = await useMultiplayerDraftStore.getState().resumeDraft();
    expect(outcome).toBe("offline");
    expect(useMultiplayerDraftStore.getState().error).toBe(DRAFT_OFFLINE_ERROR);
    expect(useMultiplayerDraftStore.getState().phase).toBe("error");

    useConnectivityStore.setState({ forcedOffline: false });
    renderPage();

    expect(screen.queryByText(DRAFT_OFFLINE_ERROR)).toBeNull();
    expect(
      screen.getByText(
        "Starting a multiplayer draft is unavailable while offline. Reconnect or turn off Offline Mode to continue.",
      ),
    ).toBeInTheDocument();
    expectErrorScreenMounted();
  });

  it("keeps a guest recovery failure's message and retry over a later offline error", async () => {
    useDraftPodStore.setState({ joinCode: "ABCDE", guestDisplayName: "Guest" });
    await useDraftPodStore.getState().joinPod();
    await waitFor(() => expect(useMultiplayerDraftStore.getState().phase).not.toBe("connecting"));

    // Injected directly: this suite does not drive a real `reconnectFailed`.
    // `resumeDraft` and the offline write below are real.
    useMultiplayerDraftStore.setState({
      guestRecoveryFailure: { kind: "retryable", message: "Host is restarting" },
    });

    useConnectivityStore.setState({ forcedOffline: true });
    await useMultiplayerDraftStore.getState().resumeDraft();
    useConnectivityStore.setState({ forcedOffline: false });
    renderPage();

    expect(screen.getByText("Host is restarting")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Try Reconnecting" })).toBeInTheDocument();
    expect(
      screen.queryByText(
        "Starting a multiplayer draft is unavailable while offline. Reconnect or turn off Offline Mode to continue.",
      ),
    ).toBeNull();
    expectErrorScreenMounted();
  });

  it("offers no retry for a recovery failure that is not retryable", async () => {
    useDraftPodStore.setState({ joinCode: "ABCDE", guestDisplayName: "Guest" });
    await useDraftPodStore.getState().joinPod();
    await waitFor(() => expect(useMultiplayerDraftStore.getState().phase).not.toBe("connecting"));

    useMultiplayerDraftStore.setState({
      guestRecoveryFailure: { kind: "incompatible", message: "Refresh both windows" },
    });
    useConnectivityStore.setState({ forcedOffline: true });
    await useMultiplayerDraftStore.getState().resumeDraft();
    useConnectivityStore.setState({ forcedOffline: false });
    renderPage();

    expect(screen.getByText("Refresh both windows")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Try Reconnecting" })).toBeNull();
    expectErrorScreenMounted();
  });
});
