import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useConnectivityStore } from "../../../stores/connectivityStore";
import { DRAFT_OFFLINE_ERROR } from "../../../stores/multiplayerDraftStore";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";

const mocks = vi.hoisted(() => ({
  startDraft: vi.fn(async () => {}),
  toggleBotFill: vi.fn(),
  kickPlayer: vi.fn(),
  leave: vi.fn(async () => {}),
  copyText: vi.fn(),
  multiplayerState: {
    role: "host",
    seats: [
      {
        seat_index: 0,
        display_name: "Host",
        is_bot: false,
        connected: true,
        has_submitted_deck: false,
        pick_status: "NotDrafting",
      },
    ],
    joined: 1,
    total: 4,
    roomCode: "ABCDE",
    seatIndex: 0,
    error: null as string | null,
  },
  podState: {
    botFillEnabled: true,
    // The engine-published seat counts the lobby's Start gate reads. The base
    // Premier procedure allows every normal pod size from two through eight.
    allowedPodSizes: [2, 3, 4, 5, 6, 7, 8] as number[] | null,
    config: {
      setCode: "dft",
      setName: "Draft Set",
      kind: "Premier",
      podSize: 4,
    },
  },
}));

type MultiplayerMockState = typeof mocks.multiplayerState & {
  kickPlayer: typeof mocks.kickPlayer;
  leave: typeof mocks.leave;
  startDraft: typeof mocks.startDraft;
};

type PodMockState = typeof mocks.podState & {
  toggleBotFill: typeof mocks.toggleBotFill;
  startDraft: () => Promise<void>;
};

vi.mock("../../../stores/multiplayerDraftStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/multiplayerDraftStore")>();
  const state = (): MultiplayerMockState => ({
      ...mocks.multiplayerState,
      kickPlayer: mocks.kickPlayer,
      leave: mocks.leave,
      startDraft: mocks.startDraft,
    });
  return {
    ...actual,
    useMultiplayerDraftStore: Object.assign(
      (selector: (current: MultiplayerMockState) => unknown) => selector(state()),
      { getState: state },
    ),
  };
});

// Only the hook is stubbed. `draftKindLabels` — the single authority for rendering
// a `DraftKind` as prose — lives in the leaf module `components/draft/draftKind`
// and is not mocked: a stub would be a second copy of the map and could not catch
// it drifting.
vi.mock("../../../stores/draftPodStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/draftPodStore")>();
  return {
    ...actual,
    useDraftPodStore: (selector: (state: PodMockState) => unknown) =>
      selector({
      ...mocks.podState,
      toggleBotFill: mocks.toggleBotFill,
      startDraft: actual.useDraftPodStore.getState().startDraft,
      }),
  };
});
vi.mock("../../../services/copyText", () => ({ copyText: mocks.copyText }));

import { DraftPodLobby } from "../DraftPodLobby";

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe("DraftPodLobby", () => {
  beforeEach(() => {
    mocks.startDraft.mockReset();
    mocks.startDraft.mockResolvedValue(undefined);
    mocks.toggleBotFill.mockClear();
    mocks.kickPlayer.mockClear();
    mocks.leave.mockReset();
    mocks.leave.mockResolvedValue(undefined);
    mocks.copyText.mockClear();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  afterEach(() => {
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  it("shows the host in the first seat and allows starting with bot fill", () => {
    render(<DraftPodLobby onLeave={vi.fn()} />);

    expect(screen.getByText("Host")).toBeInTheDocument();
    expect(screen.getByText("HOST")).toBeInTheDocument();
    expect(screen.getByText("1 / 4 seats filled")).toBeInTheDocument();

    const startButton = screen.getByRole("button", { name: "Start Draft" });
    expect(startButton).toBeEnabled();

    fireEvent.click(startButton);

    expect(mocks.startDraft).toHaveBeenCalledTimes(1);
  });

  it("does not reach the lower start seam when connectivity flips before an online-rendered Start click", async () => {
    render(<DraftPodLobby onLeave={vi.fn()} />);
    const startButton = screen.getByRole("button", { name: "Start Draft" });
    expect(startButton).toBeEnabled();

    // The browser can deliver the click already queued from the online render
    // after connectivity changes but before React paints the disabled control.
    // This calls the REAL draft-pod public action, whose only lower seam is the
    // mocked multiplayer start below.
    useConnectivityStore.setState({ forcedOffline: true });
    expect(startButton).toBeEnabled();
    fireEvent.click(startButton);

    await vi.waitFor(() => expect(mocks.startDraft).not.toHaveBeenCalled());
  });

  it("names the draft kind in prose rather than as a raw enum", () => {
    mocks.podState.config.kind = "CommanderDraft";
    render(<DraftPodLobby onLeave={vi.fn()} />);

    // Reach guard: the header rendered, so the string below is a real reading.
    expect(screen.getByText("Draft Pod Lobby")).toBeInTheDocument();
    // REVERT-FAILING: BASE interpolates `config.kind` directly, producing
    // "CommanderDraft Draft" once Commander Draft is selectable.
    expect(screen.getByText(/Commander Draft/)).toBeInTheDocument();
    expect(screen.queryByText(/CommanderDraft/)).toBeNull();
  });

  it("still names the pre-existing kinds from the same map", () => {
    mocks.podState.config.kind = "Premier";
    render(<DraftPodLobby onLeave={vi.fn()} />);

    expect(screen.getByText(/Premier Draft/)).toBeInTheDocument();
  });

  it.each([
    ["forced offline", { forcedOffline: true, browserOnline: true }],
    ["browser offline", { forcedOffline: false, browserOnline: false }],
  ])("disables only Start and preserves host lobby controls while %s", async (_label, connectivity) => {
    const originalSeats = mocks.multiplayerState.seats;
    const originalError = mocks.multiplayerState.error;
    mocks.multiplayerState.seats = [
      ...originalSeats,
      {
        seat_index: 1,
        display_name: "Guest",
        is_bot: false,
        connected: true,
        has_submitted_deck: false,
        pick_status: "NotDrafting",
      },
    ];
    mocks.multiplayerState.error = DRAFT_OFFLINE_ERROR;
    useConnectivityStore.setState(connectivity);
    const onLeave = vi.fn();
    const leaveCompletion = deferred<void>();
    mocks.leave.mockImplementationOnce(() => leaveCompletion.promise);
    render(<DraftPodLobby onLeave={onLeave} />);

    expect(screen.getByText("Reconnect or turn off Offline Mode to host, join, start, or watch a multiplayer draft.")).toBeInTheDocument();
    expect(screen.getByText("Starting a multiplayer draft is unavailable while offline. Reconnect or turn off Offline Mode to continue.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Start Draft" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Kick" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Fill empty seats with bots" }));
    fireEvent.click(screen.getByRole("button", { name: /ABCDE/ }));
    fireEvent.click(screen.getByRole("button", { name: "Leave" }));

    expect(mocks.kickPlayer).toHaveBeenCalledWith(1);
    expect(mocks.toggleBotFill).toHaveBeenCalledTimes(1);
    expect(mocks.copyText).toHaveBeenCalledWith("ABCDE");
    expect(mocks.leave).toHaveBeenCalledTimes(1);
    expect(onLeave).not.toHaveBeenCalled();

    await act(async () => {
      leaveCompletion.resolve();
      await leaveCompletion.promise;
    });

    expect(onLeave).toHaveBeenCalledTimes(1);
    mocks.multiplayerState.seats = originalSeats;
    mocks.multiplayerState.error = originalError;
  });
  /**
   * `canStart` reads the complete engine-published allowed-size set from the
   * store. No kind-blind floor or fallback is reconstructed in the UI.
   */
  describe("the Start gate reads the engine-published allowed seat counts", () => {
    const baseSeats = mocks.multiplayerState.seats;
    const baseJoined = mocks.multiplayerState.joined;
    const baseBotFill = mocks.podState.botFillEnabled;
    const baseAllowedPodSizes = mocks.podState.allowedPodSizes;

    /** `filled` occupied seats out of four. `DraftPodLobby` counts a seat as
     *  filled by its `display_name`, so the empties carry none. */
    function seatsFilled(filled: number) {
      return Array.from({ length: 4 }, (_, i) => ({
        seat_index: i,
        display_name: i < filled ? `P${i}` : "",
        is_bot: false,
        connected: true,
        has_submitted_deck: false,
        pick_status: "NotDrafting",
      }));
    }

    function startButton() {
      return screen.getByRole("button", { name: "Start Draft" });
    }

    beforeEach(() => {
      mocks.multiplayerState.seats = seatsFilled(2);
      mocks.multiplayerState.joined = 2;
      mocks.podState.botFillEnabled = false;
    });

    afterEach(() => {
      mocks.multiplayerState.seats = baseSeats;
      mocks.multiplayerState.joined = baseJoined;
      mocks.podState.botFillEnabled = baseBotFill;
      mocks.podState.allowedPodSizes = baseAllowedPodSizes;
    });

    it("disables Start when two seats are outside Commander Draft's allowed set", () => {
      mocks.podState.allowedPodSizes = [3, 4, 5, 6, 7, 8];
      render(<DraftPodLobby onLeave={vi.fn()} />);

      // Reach guard: the lobby really rendered these two seats, so the
      // disabled state below is a reading of THIS fixture.
      expect(screen.getByText("2 / 4 seats filled")).toBeInTheDocument();
      // REVERT-FAILING: a kind-blind two-seat fallback makes this enabled.
      expect(startButton()).toBeDisabled();
    });

    it("enables Start when two seats are in Premier's allowed set", () => {
      mocks.podState.allowedPodSizes = [2, 3, 4, 5, 6, 7, 8];
      render(<DraftPodLobby onLeave={vi.fn()} />);

      // The paired positive reach-guard: without it the negative above is
      // satisfiable by a button that is never enabled at all.
      expect(startButton()).toBeEnabled();
    });

    it("lets bot-fill enable Start outside the current allowed set", () => {
      mocks.podState.allowedPodSizes = [3, 4, 5, 6, 7, 8];
      mocks.podState.botFillEnabled = true;
      render(<DraftPodLobby onLeave={vi.fn()} />);

      // Multi-authority: bot-fill pads the pod to `procedure.pod_size`, which
      // is above every kind's floor, so its short-circuit is preserved.
      expect(startButton()).toBeEnabled();
    });

    it("disables Start while the engine has not answered", () => {
      mocks.podState.allowedPodSizes = null;
      render(<DraftPodLobby onLeave={vi.fn()} />);

      // Fail closed: no client-side fallback may reinstate a legal count.
      expect(startButton()).toBeDisabled();
    });
  });
});
