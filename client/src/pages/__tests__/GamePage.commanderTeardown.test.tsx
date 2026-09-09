/**
 * Step 4 — "Back to Draft" on a `mode=draft-match` game-over screen.
 *
 * WHAT IS UNDER TEST, AND WHY IT NEEDS TWO ROWS.
 * `?mode=draft-match` is navigated to from exactly three places today —
 * `DraftPodPage`'s pairwise `startMatch`, and the store's `launchCommanderGame`
 * / `joinCommanderGame` — so ONE button in `GameOverScreen` serves TWO flows
 * with opposite teardown obligations:
 *
 *   - a pairwise pod match is MID-TOURNAMENT; the pod must survive it so the
 *     next round can be paired. This button has always been a bare `navigate`.
 *   - a Commander launch is the pod's LAST act; its transport and the pod
 *     session end together, or the host returns to a `CompleteView` holding a
 *     live adapter and a stale `commanderLaunch` and spins on the waiting state
 *     forever.
 *
 * So the row that matters most here is the SECOND one. Step 4's teardown was
 * first specified as an unconditional `leave()`, which would tear down a live
 * pod tournament every time any player finished a round. Nothing else in the
 * tree guards against that condition being deleted as redundant, and it looks
 * redundant precisely because the two flows are indistinguishable from this
 * component's props.
 *
 * NEITHER ROW USES A STAND-IN FOR THE STORE. `multiplayerDraftStore` is REAL:
 * `leave` is wrapped in a spy that still calls it, so each row asserts both the
 * call AND its consequence on real state — `commanderLaunch` cleared on one
 * side, `matchPairing` still standing on the other. A spy that merely recorded
 * the call would pass against a `leave` that did nothing.
 *
 * BOTH FIELDS ARE DRIVEN NON-NULL BEFORE THE CLICK. `null` is the initial value
 * of both, so a row that asserted "still null" or "now null" without seeding it
 * first would pass against an implementation that never ran.
 *
 * The mock set is `GamePage.bracketViolation.test.tsx`'s, which already renders
 * this same game-over screen; the `MotionGlobalConfig.skipAnimations` switch is
 * that suite's recipe too — the buttons are gated on `onAnimationComplete` of
 * the title's spring, which never settles under happy-dom's rAF.
 */
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";
import { MotionGlobalConfig } from "framer-motion";

import { GamePage } from "../GamePage";
import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";
import { gameStateFactory } from "../../test/factories/gameStateFactory";
import type { DraftCommanderLaunch, DraftMatchLaunch } from "../../network/draftProtocol";

// ── Hoisted mock state ───────────────────────────────────────────────────────

const { mockSetGameState, storeOverrides } = vi.hoisted(() => ({
  mockSetGameState: vi.fn(),
  storeOverrides: {
    gameState: null as unknown,
    waitingFor: null as unknown,
  },
}));

const { mockMultiplayerState, mockUseMultiplayerStore } = vi.hoisted(() => {
  const mockMultiplayerState = {
    serverInfo: null,
    activePlayerId: 0,
    playerNames: new Map<string, string>(),
    playerAvatars: new Map<string, string>(),
    connectionStatus: "disconnected",
    isSpectator: false,
    toasts: new Map<string, { message: string; expiresAt: number; showCountdown: boolean }>(),
    hostGameCode: null,
    hostingStatus: "idle",
    playerSlots: [] as unknown[],
    displayName: "",
    setConnectionStatus: vi.fn(),
    setActionPending: vi.fn(),
    setLatency: vi.fn(),
    clearToast: vi.fn(),
    showToast: vi.fn(),
  };
  const mockUseMultiplayerStore = Object.assign(
    vi.fn((selector?: (s: typeof mockMultiplayerState) => unknown) =>
      selector ? selector(mockMultiplayerState) : mockMultiplayerState,
    ),
    { getState: () => mockMultiplayerState, setState: vi.fn() },
  );
  return { mockMultiplayerState, mockUseMultiplayerStore };
});

// ── Mock heavy dependencies (mirrors GamePage.bracketViolation.test.tsx) ──────

vi.mock("../../providers/GameProvider", () => ({
  GameProvider: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}));

vi.mock("../../game/sessionCleanup.ts", () => ({ clearPromptOverlayState: vi.fn() }));

vi.mock("../../hooks/useGameDispatch.ts", () => ({ useGameDispatch: () => vi.fn() }));

// `game/dispatch.ts` runs a module-level `captureSnapshot()` that touches
// `document` at import, before happy-dom is ready. Same mock, same reason, as
// the sibling suite.
vi.mock("../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchResolveAll: vi.fn(),
  processRemoteUpdate: vi.fn(),
  restoreGameState: vi.fn(),
  currentSnapshot: new Map(),
}));

vi.mock("../../stores/gameStore", async () => ({
  useGameStore: Object.assign(
    vi.fn((selector: (s: Record<string, unknown>) => unknown) =>
      selector({
        gameState: storeOverrides.gameState,
        gameMode: null,
        waitingFor: storeOverrides.waitingFor,
        gameId: "pod-game-1",
        legalActions: [],
        endContinuousEffectOffers: [],
        autoPassRecommended: false,
        spellCosts: {},
        legalActionsByObject: {},
        activationBlockReasons: {},
        events: [],
        eventHistory: [],
        logHistory: [],
        adapter: null,
        lobbyProgress: null,
      }),
    ),
    { setState: mockSetGameState },
  ),
  clearGame: vi.fn(),
  hasRemoteHumans: (
    await vi.importActual<typeof import("../../stores/gameStore")>("../../stores/gameStore")
  ).hasRemoteHumans,
  canExportAuthoritativeState: (
    await vi.importActual<typeof import("../../stores/gameStore")>("../../stores/gameStore")
  ).canExportAuthoritativeState,
  loadActiveGame: vi.fn(() => null),
  saveActiveGame: vi.fn(),
  clearActiveGame: vi.fn(),
  loadGame: vi.fn(() => Promise.resolve(null)),
  loadCheckpoints: vi.fn(() => Promise.resolve([])),
}));

// SYNCHRONOUS factory, as in the sibling suite: an async one reorders module
// evaluation and the real `dispatch.ts` top-level runs before happy-dom exists.
// `multiplayerDraftStore` — which this suite keeps REAL — reads
// `FORMAT_DEFAULTS.Limited` at module top level, so the Proxy must answer.
vi.mock("../../stores/multiplayerStore", () => ({
  useMultiplayerStore: mockUseMultiplayerStore,
  FORMAT_DEFAULTS: new Proxy({}, { get: (_target, key) => ({ format: String(key) }) }),
}));

vi.mock("../../hooks/usePlayerId", () => ({
  usePlayerId: () => 0,
  usePerspectivePlayerId: () => 0,
  useCanActForWaitingState: () => true,
  waitingPlayer: () => null,
}));

vi.mock("../../hooks/useIsMobile", () => ({
  useIsMobile: () => false,
  useIsCompactHeight: () => false,
}));

vi.mock("../../audio/useAudioContext", () => ({ useAudioContext: () => undefined }));
vi.mock("../../hooks/useGameplayPreferencesSync", () => ({
  useGameplayPreferencesSync: () => undefined,
}));
vi.mock("../../hooks/useCardDataMeta", () => ({
  useCardDataMeta: () => null,
  formatRelativeDate: () => "",
}));

vi.mock("../../components/board/BattlefieldBackground", () => ({
  BattlefieldBackground: () => null,
}));
vi.mock("../../components/stack/StackDisplay", () => ({ StackDisplay: () => null }));
vi.mock("../../components/debug/DebugPanel", () => ({ DebugPanel: () => null }));
vi.mock("../../components/hud/HUD", () => ({ HUD: () => null }));
vi.mock("../../components/board/GameBoard", () => ({ GameBoard: () => null }));
vi.mock("../../components/modal/EngineLostModal", () => ({ EngineLostModal: () => null }));
vi.mock("../../components/modal/CardDataMissingModal", () => ({
  CardDataMissingModal: () => null,
}));
vi.mock("../../components/modal/ChoiceModal", () => ({ ChoiceModal: () => null }));
vi.mock("../../components/multiplayer/ConcedeDialog", () => ({ ConcedeDialog: () => null }));
vi.mock("../../components/chrome/GameMenu", () => ({
  GameMenu: () => <button type="button">Game menu</button>,
}));

vi.mock("../../stores/draftStore", () => ({
  useDraftStore: vi.fn(() => ({
    phase: "idle",
    pool: [],
    picks: [],
    packs: [],
    currentPack: null,
    currentPickIndex: 0,
    draftComplete: false,
  })),
}));
vi.mock("../../services/quickDraftPersistence", () => ({
  loadActiveQuickDraft: vi.fn(() => null),
  saveQuickDraftRun: vi.fn(),
  deleteQuickDraftRun: vi.fn(),
}));
// Type-only in `multiplayerDraftStore`, so this cannot disturb the real store.
vi.mock("../../adapter/draft-adapter", () => ({ createDraftAdapter: vi.fn() }));

// ── Fixtures ─────────────────────────────────────────────────────────────────

/** What both roles of a Commander game hold while that game is running. */
function commanderLaunchFixture(): DraftCommanderLaunch {
  return {
    gameId: "pod-game-1",
    roomCode: "ABCDE-commander-pod1",
    localDeck: { main_deck: ["Sol Ring"], sideboard: [], commander: ["Kenrith"] },
    playerCount: 4,
    draftSetCodes: ["TST"],
  };
}

/**
 * A pairwise pod match, mid-tournament.
 *
 * `matchAuthoritySeat` deliberately differs from `localSeat`: `reportMatchResult`
 * returns early on that mismatch, so the game-over screen's result-reporting
 * effect resolves without touching the settlement outbox or an adapter — which
 * is what enables the button. The row is about teardown, not settlement.
 */
function pairwiseLaunchFixture(): DraftMatchLaunch {
  return {
    type: "HumanHost",
    matchId: "match-r1-s0",
    matchRoomCode: "PODAB",
    round: 1,
    localSeat: 0,
    opponentSeat: 1,
    opponentName: "Player 1",
    matchHostPeerId: "peer-host",
    deckPayload: {
      player: { main_deck: ["Forest"], sideboard: [], commander: [] },
      opponent: { main_deck: ["Island"], sideboard: [], commander: [] },
    },
    matchConfig: { match_type: "Bo1" },
    binding: {
      podId: "pod-1",
      matchId: "match-r1-s0",
      round: 1,
      sessionKey: "session-key",
      lease: "lease",
      nonce: "nonce",
      revision: 1,
      matchAuthoritySeat: 1,
    },
  } as unknown as DraftMatchLaunch;
}

function renderGameOver() {
  return render(
    <MemoryRouter initialEntries={["/game/pod-game-1?mode=draft-match"]}>
      <Routes>
        <Route path="/game/:id" element={<GamePage />} />
        <Route path="/draft-pod" element={<div data-testid="draft-pod-page">Pod</div>} />
        <Route path="/" element={<div>Home</div>} />
      </Routes>
    </MemoryRouter>,
  );
}

/**
 * The real `leave`, wrapped so a row can assert the call AND its effect.
 *
 * Typed to the store's OWN signature rather than a bare `vi.fn()`: `setState`
 * checks the field it replaces, so a loose `Mock` is a `tsc` error — and that
 * check is the thing keeping the wrapper honest about what it stands in for.
 */
type LeaveFn = (preserveRecovery?: boolean) => Promise<void>;
let leaveSpy: Mock<LeaveFn>;
const realLeave: LeaveFn = useMultiplayerDraftStore.getState().leave;

async function clickBackToDraft() {
  await userEvent.click(await screen.findByRole("button", { name: "Back to Draft" }));
}

// ── Tests ────────────────────────────────────────────────────────────────────

describe("GamePage — draft-match teardown on Back to Draft", () => {
  beforeEach(() => {
    MotionGlobalConfig.skipAnimations = true;
    storeOverrides.gameState = gameStateFactory.withPlayers(0, 1).build();
    storeOverrides.waitingFor = { type: "GameOver", data: { winner: 0 } };
    mockMultiplayerState.activePlayerId = 0;
    usePreferencesStore.setState({
      multiplayerBoardLayout: "focused",
      multiplayerSplitLayoutNudgeDismissed: true,
    });
    useUiStore.setState({ pendingAbilityChoice: null });
    // `leave` is arrow-bound over `set`/`get`, so the detached call is the real
    // one — the wrapper adds a call record and nothing else.
    leaveSpy = vi.fn<LeaveFn>(realLeave);
  });

  afterEach(() => {
    MotionGlobalConfig.skipAnimations = false;
    cleanup();
    useMultiplayerDraftStore.getState().reset();
    // AFTER `reset()`: it spreads `initialState`, which carries state only, so
    // the wrapper would otherwise outlive its test.
    useMultiplayerDraftStore.setState({ leave: realLeave });
    localStorage.clear();
  });

  /**
   * The Commander game is the pod's last act, so the pod session ends with it.
   *
   * REVERT-FAILING: drop the `leave()` from `handleBackToPod` (leaving the bare
   * `navigate("/draft-pod")` this button carried before step 4) and this row
   * reds on both the call and the still-set `commanderLaunch`.
   */
  it("tears the pod down when the finished game was a Commander launch", async () => {
    useMultiplayerDraftStore.setState({
      commanderLaunch: commanderLaunchFixture(),
      matchPairing: null,
      leave: leaveSpy,
    });
    // Driven non-null FIRST: `null` is this field's initial value, so without
    // this the "cleared" assertion below would pass on a click that did nothing.
    expect(useMultiplayerDraftStore.getState().commanderLaunch).not.toBeNull();

    renderGameOver();
    await clickBackToDraft();

    expect(leaveSpy).toHaveBeenCalledTimes(1);
    // The navigation still happens, and it is also the reach guard for the
    // assertions around it.
    expect(await screen.findByTestId("draft-pod-page")).toBeInTheDocument();
    // The CONSEQUENCE, not just the call: `leave` clears the launch through
    // `disposeMatchAdapter`, which is what stops `CompleteView` rendering its
    // waiting state against a game that is already over.
    await waitFor(() =>
      expect(useMultiplayerDraftStore.getState().commanderLaunch).toBeNull(),
    );
  });

  /**
   * THE REGRESSION GUARD. A pairwise pod match is mid-tournament and the pod
   * must survive it.
   *
   * REVERT-FAILING against the edit a future reader would actually make: delete
   * the `commanderLaunch` condition so `leave()` runs unconditionally — the
   * shape step 4's teardown was first specified as. This row then reds on both
   * the unexpected call and the destroyed `matchPairing`.
   */
  it("leaves the pod intact when the finished game was a pairwise pod match", async () => {
    useMultiplayerDraftStore.setState({
      commanderLaunch: null,
      matchPairing: pairwiseLaunchFixture(),
      leave: leaveSpy,
    });
    // Driven non-null FIRST, for the same reason as the row above: this row
    // asserts a SURVIVAL, and `matchPairing` is `null` by default.
    expect(useMultiplayerDraftStore.getState().matchPairing).not.toBeNull();

    renderGameOver();
    await clickBackToDraft();

    // Reach guard for the negative: the route actually changed, so "leave was
    // not called" cannot pass on a click that never dispatched.
    expect(await screen.findByTestId("draft-pod-page")).toBeInTheDocument();
    expect(leaveSpy).not.toHaveBeenCalled();
    // The pod tournament is still standing — the next round can be paired.
    expect(useMultiplayerDraftStore.getState().matchPairing).not.toBeNull();
  });
});
