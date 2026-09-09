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
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

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

afterEach(async () => {
  vi.restoreAllMocks();
  // This suite's seat row moves state on the REAL stores (see its comment), so
  // it has to put both back or every later row concedes as seat 2.
  const { useGameStore: actualGameStore } =
    await vi.importActual<typeof import("../../stores/gameStore")>("../../stores/gameStore");
  actualGameStore.setState({ gameMode: null });
  useMultiplayerStore.setState({ activePlayerId: 0 });
  useMultiplayerDraftStore.setState({
    commanderLaunch: null,
    commanderSeat: null,
    matchAdapter: null,
  });
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

  // INVERTED (was: "refuses an unbound draft pod concession..."). #7920's
  // refusal was correct while every `draft-match` game was a bound 1v1; the
  // Commander pod launch binds nothing, so refusing left the in-game Concede
  // button silently inert for all four seats. Now pinned as a FALLTHROUGH.
  //
  // Three wrong implementations must red this row: the old refusal (no
  // dispatch); a fallthrough that reaches the transport instead of the engine
  // (the decoy below); and a fallthrough that skips the engine and merely
  // clears + navigates (the ordering assertions).
  //
  // The decoy carries the REAL method name and omits only the capability flag,
  // because `supportsMatchConcede` (adapter/types.ts) requires BOTH
  // `supportsMatchConcede === true` AND a callable `sendMatchConcede`. An
  // earlier version spied on a `sendConcede` the hook never calls under any
  // implementation, so it could not fail. This one reds if the guard is ever
  // weakened to a bare method check — the plausible edit.
  it("falls through to the game engine for an unbound draft pod concession", async () => {
    const sendMatchConcede = vi.fn();
    adapterForTest = { sendMatchConcede };
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
      // Flush the promise chain.
      await Promise.resolve();
      await Promise.resolve();
    });

    // CR 104.3a: the conceding player leaves the game and loses it.
    expect(dispatchMock).toHaveBeenCalledTimes(1);
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "Concede",
      data: { player_id: 0 },
    });
    // The transport path must NOT be taken: the adapter is unbound (no
    // capability flag), so the engine dispatch above is the only correct route.
    expect(sendMatchConcede).not.toHaveBeenCalled();
    expect(clearGameMock).toHaveBeenCalledWith("g1");
    expect(navigateMock).toHaveBeenCalledWith("/");

    // Carried over from the ai/local row: a fallthrough that clears and
    // navigates without reaching the engine passes every assertion above only
    // if the ordering is unchecked.
    const dispatchOrder = dispatchMock.mock.invocationCallOrder[0];
    const overlayOrder = clearPromptOverlayStateMock.mock.invocationCallOrder[0];
    const clearOrder = clearGameMock.mock.invocationCallOrder[0];
    expect(dispatchOrder).toBeLessThan(overlayOrder);
    expect(overlayOrder).toBeLessThan(clearOrder);
  });

  /**
   * RECONSTRUCTED. The team lead wrote this row; I destroyed it by restoring
   * this file from a backup taken before their edit, and rebuilt it from their
   * description. It is verified to discriminate (see below), but it is their
   * intent expressed in my words — worth a read from them.
   *
   * CR 104.3a: the conceding player leaves the game and loses it. CR 800.4a:
   * the remaining players play on. That second half is the whole point of this
   * row. `releaseCommanderPodState` clears the POD's record of the launch and
   * deliberately does NOT dispose the adapter, because in this
   * host-authoritative topology the host's adapter IS the game for everyone
   * else — tearing it down because one player conceded would end three other
   * players' game. The surviving adapter is load-bearing, not a leak, which is
   * why this is asymmetric with `endCommanderSession` on purpose.
   *
   * Both fields are seeded NON-NULL first: `null` is also their initial value,
   * so without the seeding this row would pass against a function that does
   * nothing at all.
   */
  it("drops the pod's launch record on concede without tearing down the game", async () => {
    adapterForTest = { sendMatchConcede: sendMatchConcedeMock };
    const survivingAdapter = { marker: "still serving the other seats" };
    useMultiplayerDraftStore.setState({
      commanderLaunch: {
        gameId: "commander-1",
        roomCode: "POD-commander-abc",
        localDeck: { main: ["Sol Ring"], commanders: ["Ur-Dragon"] },
        playerCount: 4,
        draftSetCodes: null,
      } as never,
      commanderSeat: 2,
      matchAdapter: survivingAdapter as never,
    });

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

    // The pod's record of a game that is over for this player.
    expect(useMultiplayerDraftStore.getState().commanderLaunch).toBeNull();
    expect(useMultiplayerDraftStore.getState().commanderSeat).toBeNull();
    // CR 800.4a: the transport the others are still playing on is untouched.
    expect(useMultiplayerDraftStore.getState().matchAdapter).toBe(survivingAdapter);
    // Ordering: the concession must reach the engine BEFORE this client stops
    // caring about the game. Clearing ahead of the dispatch would leave three
    // players watching a seat that neither acts nor leaves.
    expect(dispatchMock).toHaveBeenCalledTimes(1);
  });

  /**
   * The seat a Commander pod concession is dispatched FOR, which every other
   * row in this file leaves unpinned.
   *
   * Why it matters: `draft-match` is declared `seat: "wire-assigned"`
   * (`GAME_MODE_TRAITS`), so `getPlayerId()` must answer the seat the HOST
   * assigned this client, not seat 0. Replacing `getPlayerId()` with a literal
   * `0` in `useConcedeHandler` keeps every other row in this file green while
   * each guest concedes AS THE HOST — in a four-seat pod, one guest quitting
   * eliminates the host instead. That is the reported bug's own failure mode
   * (everything resolving to seat 0) reappearing on the path this change
   * created.
   *
   * WHY THIS ROW SEEDS THE REAL STORES INSTEAD OF THE MOCK ABOVE, which is
   * surprising enough to be worth stating: the suite's `useGameStore` mock does
   * NOT reach `usePlayerId`. `stores/gameStore.ts` itself imports `getPlayerId`
   * from `hooks/usePlayerId`, so the mock factory's own
   * `vi.importActual("../../stores/gameStore")` instantiates `usePlayerId`
   * inside the ACTUAL module graph, bound to the real `useGameStore`, and that
   * cached instance is the one `useConcedeHandler` later receives. Measured,
   * not assumed: with `gameMode: "draft-match"` added to the mock's `getState`,
   * `getPlayerId()` still answered `0`; seeding the store returned by
   * `importActual` made it answer `2`. `multiplayerStore` is not mocked at all,
   * so its `activePlayerId` was always shared.
   */
  it("concedes as this client's own wire-assigned seat, not as seat 0", async () => {
    // Unbound, so the pod branch falls through to the engine — the only path
    // on which the conceding seat is chosen at all.
    adapterForTest = { sendMatchConcede: sendMatchConcedeMock };
    const { useGameStore: actualGameStore } =
      await vi.importActual<typeof import("../../stores/gameStore")>("../../stores/gameStore");
    actualGameStore.setState({ gameMode: "draft-match" });
    // A guest seated third by the host. Deliberately NOT 0 and NOT 1: 0 is the
    // value a broken resolver returns anyway, and 1 is `DRAFT_BOT_AI_SEAT`.
    useMultiplayerStore.setState({ activePlayerId: 2 });

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

    // CR 104.3a: the player who concedes is the one who leaves and loses.
    expect(dispatchMock).toHaveBeenCalledWith({
      type: "Concede",
      data: { player_id: 2 },
    });
    expect(sendMatchConcedeMock).not.toHaveBeenCalled();
  });
});
