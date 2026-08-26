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
const sendMatchConcedeMock = vi.fn();
const navigateMock = vi.fn();
let adapterForTest: unknown = { supportsMatchConcede: true, sendMatchConcede: sendMatchConcedeMock };

vi.mock("../../game/sessionCleanup", () => ({
  clearPromptOverlayState: () => clearPromptOverlayStateMock(),
}));

// Only the store handle and `clearGame` are stubbed. The module's pure
// helpers — `seatSource` and the `GAME_MODE_TRAITS` census behind it, which
// `getPlayerId()` consults for the conceding seat — come through for real, so
// this test concedes as the seat the census actually resolves rather than as a
// seat the mock asserts.
vi.mock("../../stores/gameStore", async () => ({
  ...(await vi.importActual<typeof import("../../stores/gameStore")>("../../stores/gameStore")),
  useGameStore: {
    getState: () => ({
      dispatch: dispatchMock,
      adapter: adapterForTest,
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
  sendMatchConcedeMock.mockReset();
  adapterForTest = { supportsMatchConcede: true, sendMatchConcede: sendMatchConcedeMock };
  navigateMock.mockReset();

  dispatchMock.mockResolvedValue([]);
  recordMatchResultMock.mockResolvedValue(undefined);
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

  it("isDraftPodMatch branch uses only the bound whole-match capability", async () => {
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
    });

    expect(sendMatchConcedeMock).toHaveBeenCalledTimes(1);
    expect(clearGameMock).not.toHaveBeenCalled();
    expect(navigateMock).not.toHaveBeenCalled();
    expect(dispatchMock).not.toHaveBeenCalled();
  });

  it("refuses an unbound draft pod concession without falling through to the game engine", async () => {
    const consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    adapterForTest = { sendConcede: vi.fn() };
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
    });

    expect(consoleErrorSpy).toHaveBeenCalledWith(
      "[useConcedeHandler] refused unbound draft pod match concession",
    );
    expect(clearGameMock).not.toHaveBeenCalled();
    expect(navigateMock).not.toHaveBeenCalled();
    consoleErrorSpy.mockRestore();
  });
});
