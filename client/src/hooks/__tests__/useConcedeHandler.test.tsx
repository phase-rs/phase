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
const clearGameMock = vi.fn();
const recordMatchResultMock = vi.fn();
const reportActiveMatchConcessionMock = vi.fn();
const sendConcedeMock = vi.fn();
const navigateMock = vi.fn();

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
    expect(clearGameMock).toHaveBeenCalledWith("g1");
    expect(navigateMock).toHaveBeenCalledWith("/");

    // Regression coverage: dispatch MUST be invoked before clearGame.
    // Without the await, a future refactor would silently regress and the
    // WasmAdapter singleton would retain the conceded game.
    const dispatchOrder = dispatchMock.mock.invocationCallOrder[0];
    const clearOrder = clearGameMock.mock.invocationCallOrder[0];
    expect(dispatchOrder).toBeLessThan(clearOrder);
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
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(sendConcedeMock).toHaveBeenCalledTimes(1);
    expect(reportActiveMatchConcessionMock).toHaveBeenCalledTimes(1);
    expect(clearGameMock).toHaveBeenCalledWith("g1");
    expect(navigateMock).toHaveBeenCalledWith("/draft-pod");
    expect(dispatchMock).not.toHaveBeenCalled();
  });
});
