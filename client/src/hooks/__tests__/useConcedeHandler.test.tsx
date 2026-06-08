/**
 * Runtime tests: these drive the hook's returned callback through the actual
 * branching logic, asserting that the correct stores/adapters/navigation are
 * invoked in the correct ORDER. The "ai/local" case asserts dispatch is
 * awaited before navigation — that ordering is the bug fix (concede must
 * reach the engine before local state is cleared, otherwise the WasmAdapter
 * singleton retains the conceded game).
 */
import { act, renderHook } from "@testing-library/react";
import { MemoryRouter } from "react-router";
import type { ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useConcedeHandler } from "../useConcedeHandler";

// ---- Mocks -----------------------------------------------------------------

const dispatchMock = vi.fn();
const clearGameMock = vi.fn().mockResolvedValue(undefined);
const clearPromptOverlayStateMock = vi.fn();
const recordMatchResultMock = vi.fn();
const reportActiveMatchConcessionMock = vi.fn();
const sendConcedeMock = vi.fn();
const navigateMock = vi.fn();

vi.mock("../../game/sessionCleanup", () => ({
  clearPromptOverlayState: () => clearPromptOverlayStateMock(),
}));

vi.mock("../../stores/gameStore", () => ({
  useGameStore: {
    getState: () => ({
      dispatch: dispatchMock,
      adapter: { sendConcede: sendConcedeMock },
    }),
  },
  clearGame: (...args: unknown[]) => clearGameMock(...args),
}));

vi.mock("../../stores/draftStore", () => ({
  useDraftStore: {
    getState: () => ({
      recordMatchResult: recordMatchResultMock,
    }),
  },
}));

vi.mock("../../stores/multiplayerDraftStore", () => ({
  useMultiplayerDraftStore: {
    getState: () => ({
      reportActiveMatchConcession: reportActiveMatchConcessionMock,
    }),
  },
}));

vi.mock("react-router", async () => {
  const actual = await vi.importActual<typeof import("react-router")>("react-router");
  return {
    ...actual,
    useNavigate: () => navigateMock,
  };
});

// ---- Helpers ---------------------------------------------------------------

function wrapper({ children }: { children: ReactNode }) {
  return <MemoryRouter>{children}</MemoryRouter>;
}

beforeEach(() => {
  dispatchMock.mockReset();
  clearGameMock.mockReset();
  clearGameMock.mockResolvedValue(undefined);
  clearPromptOverlayStateMock.mockReset();
  recordMatchResultMock.mockReset();
  reportActiveMatchConcessionMock.mockReset();
  sendConcedeMock.mockReset();
  navigateMock.mockReset();

  dispatchMock.mockResolvedValue([]);
  recordMatchResultMock.mockResolvedValue(undefined);
  reportActiveMatchConcessionMock.mockResolvedValue(undefined);
});

afterEach(() => {
  vi.restoreAllMocks();
});

// ---- Tests -----------------------------------------------------------------

describe("useConcedeHandler", () => {
  it("ai/local default branch dispatches Concede then clears + navigates home (bug fix)", async () => {
    const { result } = renderHook(
      () =>
        useConcedeHandler({
          gameId: "g1",
          isOnlineMode: false,
          isDraft: false,
          isDraftPodMatch: false,
        }),
      { wrapper },
    );

    await act(async () => {
      result.current();
      // Flush the promise chain.
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "Concede",
      data: { player_id: 0 },
    });
    expect(clearPromptOverlayStateMock).toHaveBeenCalledTimes(1);
    expect(clearGameMock).toHaveBeenCalledWith("g1");
    expect(navigateMock).toHaveBeenCalledWith("/");

    // Regression coverage: dispatch MUST be invoked before clearGame.
    // Without the await, a future refactor would silently regress and the
    // WasmAdapter singleton would retain the conceded game.
    const dispatchOrder = dispatchMock.mock.invocationCallOrder[0];
    const overlayOrder = clearPromptOverlayStateMock.mock.invocationCallOrder[0];
    const clearOrder = clearGameMock.mock.invocationCallOrder[0];
    expect(dispatchOrder).toBeLessThan(overlayOrder);
    expect(overlayOrder).toBeLessThan(clearOrder);
  });

  it("isDraft branch records match loss then clears + navigates to draft resume", async () => {
    const { result } = renderHook(
      () =>
        useConcedeHandler({
          gameId: "g1",
          isOnlineMode: false,
          isDraft: true,
          isDraftPodMatch: false,
        }),
      { wrapper },
    );

    await act(async () => {
      result.current();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(recordMatchResultMock).toHaveBeenCalledWith("g1", "loss");
    expect(clearGameMock).toHaveBeenCalledWith("g1");
    expect(navigateMock).toHaveBeenCalledWith("/draft/quick?resume=1");
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  it("isDraftPodMatch branch fires adapter sendConcede + concession report + clear + navigate", async () => {
    const { result } = renderHook(
      () =>
        useConcedeHandler({
          gameId: "g1",
          isOnlineMode: false,
          isDraft: false,
          isDraftPodMatch: true,
        }),
      { wrapper },
    );

    await act(async () => {
      result.current();
      // Hook chains: sendPromise -> .catch -> .then(report) -> .then(clear+nav).
      // Four microtask flushes cover the whole chain.
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(sendConcedeMock).toHaveBeenCalledTimes(1);
    expect(reportActiveMatchConcessionMock).toHaveBeenCalledTimes(1);
    expect(clearGameMock).toHaveBeenCalledWith("g1");
    expect(navigateMock).toHaveBeenCalledWith("/draft-pod");
    expect(dispatchMock).not.toHaveBeenCalled();

    // Regression coverage for PR #1252 review: sendConcede must complete
    // BEFORE reportActiveMatchConcession + clearGame + navigate. Without
    // the chained await on the host-side async sendConcede (which fans
    // out player_conceded to every guest's PeerJS data channel), tearing
    // down the adapter mid-fan-out drops peer notifications.
    const sendOrder = sendConcedeMock.mock.invocationCallOrder[0];
    const reportOrder = reportActiveMatchConcessionMock.mock.invocationCallOrder[0];
    const clearOrder = clearGameMock.mock.invocationCallOrder[0];
    expect(sendOrder).toBeLessThan(reportOrder);
    expect(reportOrder).toBeLessThan(clearOrder);
  });

  it("isDraftPodMatch branch awaits async sendConcede before reporting concession (race fix)", async () => {
    // Discriminating regression: the host-side sendConcede returns a
    // Promise (it awaits engine concedePlayer then broadcasts to guests).
    // A fire-and-forget call would invoke reportActiveMatchConcession
    // synchronously after sendConcede returns its pending promise — this
    // test gates on the unresolved promise to catch that regression.
    let releaseSend: () => void = () => {};
    const sendPending = new Promise<void>((resolve) => {
      releaseSend = resolve;
    });
    sendConcedeMock.mockReturnValueOnce(sendPending);

    const { result } = renderHook(
      () =>
        useConcedeHandler({
          gameId: "g1",
          isOnlineMode: false,
          isDraft: false,
          isDraftPodMatch: true,
        }),
      { wrapper },
    );

    await act(async () => {
      result.current();
      await Promise.resolve();
      await Promise.resolve();
    });

    // sendConcede has been called but its promise is still pending —
    // downstream chain must not have run.
    expect(sendConcedeMock).toHaveBeenCalledTimes(1);
    expect(reportActiveMatchConcessionMock).not.toHaveBeenCalled();
    expect(clearGameMock).not.toHaveBeenCalled();
    expect(navigateMock).not.toHaveBeenCalled();

    // Releasing the send promise lets the chain proceed.
    await act(async () => {
      releaseSend();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(reportActiveMatchConcessionMock).toHaveBeenCalledTimes(1);
    expect(clearGameMock).toHaveBeenCalledWith("g1");
    expect(navigateMock).toHaveBeenCalledWith("/draft-pod");
  });

  it("isDraftPodMatch branch still navigates if reportActiveMatchConcession rejects", async () => {
    // User intent on Concede is to leave — a store-mutation failure
    // must not strand them on the conceded screen.
    reportActiveMatchConcessionMock.mockRejectedValueOnce(new Error("store failed"));
    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const { result } = renderHook(
      () =>
        useConcedeHandler({
          gameId: "g1",
          isOnlineMode: false,
          isDraft: false,
          isDraftPodMatch: true,
        }),
      { wrapper },
    );

    await act(async () => {
      result.current();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(clearGameMock).toHaveBeenCalledWith("g1");
    expect(navigateMock).toHaveBeenCalledWith("/draft-pod");
    expect(consoleErrorSpy).toHaveBeenCalled();

    consoleErrorSpy.mockRestore();
  });
});
