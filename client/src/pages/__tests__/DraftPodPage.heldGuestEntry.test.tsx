import { act, cleanup, render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { MemoryRouter } from "react-router";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { draftProcedureFixture } from "../../adapter/__tests__/draftProcedureFixture";

/**
 * Drives the real `DraftPodPage` entry effect against the real
 * `multiplayerDraftStore` and `draftPodStore`.
 */
const net = vi.hoisted(() => ({
  joinRoom: vi.fn(async () => ({
    conn: { peer: "phase2-ABCDE" },
    peer: { id: "guest-peer", destroy: vi.fn() },
    closeConn: vi.fn(),
    destroyPeer: vi.fn(),
  })),
}));
vi.mock("../../network/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../network/connection")>()),
  joinRoom: net.joinRoom,
}));

const guest = vi.hoisted(() => ({ ctor: vi.fn(), dispose: vi.fn() }));
vi.mock("../../adapter/p2p-draft-guest", () => ({
  P2PDraftGuest: vi.fn().mockImplementation(function (...args: unknown[]) {
    guest.ctor(...args);
    return {
      onEvent: () => () => {},
      initialize: vi.fn(async () => {}),
      leave: vi.fn(async () => {}),
      dispose: guest.dispose,
      get isRecoveryRevoked() {
        return false;
      },
      view: null,
      seat: null,
      token: null,
    };
  }),
}));

const hostAd = vi.hoisted(() => ({ ctor: vi.fn(), dispose: vi.fn(async () => {}) }));
vi.mock("../../adapter/draftPodHostAdapter", () => ({
  DraftPodHostAdapter: vi.fn().mockImplementation(function () {
    hostAd.ctor();
    return {
      onEvent: () => () => {},
      initialize: vi.fn(async () => {}),
      dispose: hostAd.dispose,
      status: "lobby",
      roomCode: "HOSTD",
    };
  }),
}));

const persist = vi.hoisted(() => ({
  inspectActiveDraftGuest: vi.fn(),
  loadDraftGuestSession: vi.fn(),
  inspectActiveDraftPod: vi.fn((): unknown => ({ type: "absent" })),
  loadDraftHostSession: vi.fn(),
  persistedDraftHostSessionState: vi.fn(() => "live"),
  clearActiveDraftPodIfCurrent: vi.fn(),
}));
vi.mock("../../services/draftPersistence", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/draftPersistence")>()),
  inspectActiveDraftGuest: persist.inspectActiveDraftGuest,
  loadDraftGuestSession: persist.loadDraftGuestSession,
  inspectActiveDraftPod: persist.inspectActiveDraftPod,
  loadDraftHostSession: persist.loadDraftHostSession,
  persistedDraftHostSessionState: persist.persistedDraftHostSessionState,
  clearActiveDraftPodIfCurrent: persist.clearActiveDraftPodIfCurrent,
}));

vi.mock("../../adapter/draft-adapter", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../adapter/draft-adapter")>()),
  DraftAdapter: class {
    draftProcedure = vi.fn(async () => draftProcedureFixture());
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
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

const guestLocator = { roomCode: "ABCDE", displayName: "Alice", hostPeerId: "phase2-ABCDE", timestamp: Date.now() };
const hostMeta = {
  id: "draft-1",
  roomCode: "HOSTD",
  kind: "Premier",
  podSize: 8,
  hostDisplayName: "Host",
  tournamentFormat: "Swiss",
  podPolicy: "Competitive",
  phase: "lobby",
  pickCount: 0,
  updatedAt: Date.now(),
};
const hostSession = {
  persistenceId: "draft-1",
  roomCode: "HOSTD",
  kind: "Premier",
  podSize: 8,
  hostDisplayName: "Host",
  tournamentFormat: "Swiss",
  podPolicy: "Competitive",
  seatTokens: { 0: "host" },
  seatNames: { 0: "Host" },
  kickedTokens: [],
  draftStarted: false,
  draftCode: "HOSTD",
  draftSessionJson: null,
  poolInput: { type: "Set" as const, data: { pools: [{ code: "TST" }], sequence: ["TST"] } },
};

function presentHostRecord() {
  persist.inspectActiveDraftPod.mockReturnValue({
    type: "present",
    meta: hostMeta,
    capture: { id: hostMeta.id, roomCode: hostMeta.roomCode, updatedAt: hostMeta.updatedAt },
  });
  persist.loadDraftHostSession.mockResolvedValue(hostSession);
}

async function arrive(path: string) {
  render(
    <MemoryRouter initialEntries={[path]}>
      <DraftPodPage />
    </MemoryRouter>,
  );
  await act(async () => {
    await new Promise((r) => setTimeout(r, 400));
  });
}

describe("DraftPodPage entry with a held session", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    persist.inspectActiveDraftGuest.mockReturnValue({ type: "present", meta: guestLocator, capture: guestLocator });
    persist.loadDraftGuestSession.mockResolvedValue({ draftToken: "tok" });
    // `vi.clearAllMocks()` clears call history but not a `mockReturnValue`
    // set by a previous case, so a present host record leaks forward unless
    // reset explicitly here.
    persist.inspectActiveDraftPod.mockReturnValue({ type: "absent" });
  });

  afterEach(async () => {
    cleanup();
    await useMultiplayerDraftStore.getState().leave(true);
  });

  it("a held guest at plain /draft-pod (Back to draft) keeps its seat without a second join", async () => {
    await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    await arrive("/draft-pod");

    await screen.findByText("Waiting for host to start the draft...");
    expect(net.joinRoom).toHaveBeenCalledTimes(1);
    expect(guest.ctor).toHaveBeenCalledTimes(1);
    expect(guest.dispose).not.toHaveBeenCalled();
  });

  it("a held guest in error still reconnects on arrival", async () => {
    await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    useMultiplayerDraftStore.setState({ phase: "error" });
    await arrive("/draft-pod");

    expect(net.joinRoom).toHaveBeenCalledTimes(2);
  });

  it("auto entry with a held guest and a resumable saved host record keeps the guest", async () => {
    presentHostRecord();
    await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    await arrive("/draft-pod");

    const s = useMultiplayerDraftStore.getState();
    expect(hostAd.ctor).not.toHaveBeenCalled();
    expect(guest.dispose).not.toHaveBeenCalled();
    expect(s.role).toBe("guest");
  });

  it("with no held session, auto entry resumes the saved host record", async () => {
    presentHostRecord();
    await arrive("/draft-pod");

    expect(hostAd.ctor).toHaveBeenCalledTimes(1);
  });

  it("guest entry with no held session and a resumable host record still recovers the guest seat", async () => {
    presentHostRecord();
    await arrive("/draft-pod?entry=guest");

    expect(persist.inspectActiveDraftPod).not.toHaveBeenCalled();
    expect(hostAd.ctor).not.toHaveBeenCalled();
    expect(persist.loadDraftGuestSession).toHaveBeenCalled();
    expect(net.joinRoom).toHaveBeenCalledTimes(1);
    const s = useMultiplayerDraftStore.getState();
    expect(s.role).toBe("guest");
    expect(["idle", "error"]).not.toContain(s.phase);
  });

  it("explicit host entry with a held guest switches to the saved hosted pod", async () => {
    presentHostRecord();
    await useMultiplayerDraftStore.getState().joinDraft({ kind: "new", roomCode: "ABCDE", displayName: "Alice" });
    await arrive("/draft-pod?entry=host");

    const s = useMultiplayerDraftStore.getState();
    expect(hostAd.ctor).toHaveBeenCalledTimes(1);
    expect(s.role).toBe("host");
  });
});
