/**
 * GamePage — cEDH bracket-violation blocking modal tests.
 *
 * The modal renders when GameProvider calls `onNoDeck` with `bracketViolation`
 * set to `true`. GamePage matches by the typed flag — not by string substring
 * on the error message — so a reformatted error message cannot silently break
 * the modal trigger.
 *
 * Heavy sub-components (WASM engine, GameProvider, audio, socket, P2P)
 * are mocked so the suite exercises only the modal render logic and the
 * "Return to setup" navigation.
 */
import { cleanup, render, screen, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import { GamePage } from "../GamePage";
import type { FormatConfig } from "../../adapter/types";

// ── Hoisted variables (must be declared before vi.mock hoisting) ─────────────

// Capture `onNoDeck` from GameProvider so tests can fire it.
let capturedOnNoDeck: ((reason?: string, bracketViolation?: boolean) => void) | undefined;
let capturedFormatConfig: FormatConfig | undefined;

const { mockMultiplayerState: _mockMultiplayerState, mockUseMultiplayerStore } = vi.hoisted(() => {
  const mockMultiplayerState = {
    serverInfo: null,
    activePlayerId: null,
    playerNames: new Map<string, string>(),
    playerAvatars: new Map<string, string>(),
    connectionStatus: "disconnected",
    isSpectator: false,
    toasts: [] as unknown[],
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
    {
      getState: () => mockMultiplayerState,
      setState: vi.fn(),
    },
  );
  return { mockMultiplayerState, mockUseMultiplayerStore };
});

// ── Mock heavy dependencies ──────────────────────────────────────────────────

vi.mock("../../providers/GameProvider", () => ({
  GameProvider: ({
    children,
    onNoDeck,
    formatConfig,
  }: {
    children: React.ReactNode;
    onNoDeck?: (reason?: string, bracketViolation?: boolean) => void;
    formatConfig?: FormatConfig;
  }) => {
    capturedOnNoDeck = onNoDeck;
    capturedFormatConfig = formatConfig;
    return <>{children}</>;
  },
}));

// useGameDispatch moved out of GameProvider into its own hook module; mock it
// at the real location.
vi.mock("../../hooks/useGameDispatch.ts", () => ({
  useGameDispatch: () => vi.fn(),
}));

// game/dispatch.ts runs a module-level `captureSnapshot()` (dispatch.ts:44)
// that touches `document` at import. GamePage's subtree reaches it via
// ActionButton, and collection evaluates that import before the happy-dom
// environment is ready — so mock the whole module (matching the convention in
// ActionButton.test.tsx). All exports are stubbed since this test exercises
// the bracket-violation modal, not action dispatch.
vi.mock("../../game/dispatch.ts", () => ({
  dispatchAction: vi.fn(),
  dispatchResolveAll: vi.fn(),
  processRemoteUpdate: vi.fn(),
  restoreGameState: vi.fn(),
  currentSnapshot: new Map(),
}));

vi.mock("../../stores/gameStore", () => ({
  useGameStore: vi.fn((selector: (s: Record<string, unknown>) => unknown) =>
    selector({
      gameState: null,
      waitingFor: null,
      legalActions: [],
      autoPassRecommended: false,
      spellCosts: {},
      legalActionsByObject: {},
      events: [],
      eventHistory: [],
      logHistory: [],
      adapter: null,
      lobbyProgress: null,
    }),
  ),
  clearGame: vi.fn(),
  loadActiveGame: vi.fn(() => null),
  saveActiveGame: vi.fn(),
  clearActiveGame: vi.fn(),
  loadGame: vi.fn(() => Promise.resolve(null)),
  loadCheckpoints: vi.fn(() => Promise.resolve([])),
}));

// `FORMAT_DEFAULTS` is consumed at module top-level by multiplayerDraftStore
// (and indexed by GamePage). This test mocks the whole store to avoid its
// heavy zustand wiring, so the mock must still expose FORMAT_DEFAULTS. The
// factory stays SYNCHRONOUS: an async factory reorders module evaluation so
// the real dispatch.ts top-level `captureSnapshot()` runs before the happy-dom
// environment is ready (`document is not defined`). A Proxy returning an empty
// config for any format key satisfies every access this test reaches without
// importing the real module.
vi.mock("../../stores/multiplayerStore", () => ({
  useMultiplayerStore: mockUseMultiplayerStore,
  FORMAT_DEFAULTS: new Proxy({}, { get: (_target, key) => ({ format: String(key) }) }),
}));

vi.mock("../../hooks/usePlayerId", () => ({
  usePlayerId: () => 0,
  usePerspectivePlayerId: () => 0,
  useCanActForWaitingState: () => true,
}));

vi.mock("../../hooks/useIsMobile", () => ({
  useIsMobile: () => false,
  useIsCompactHeight: () => false,
}));

vi.mock("../../audio/useAudioContext", () => ({
  useAudioContext: () => undefined,
}));

vi.mock("../../hooks/usePhaseStopsSync", () => ({
  usePhaseStopsSync: () => undefined,
}));

vi.mock("../../components/board/BattlefieldBackground", () => ({
  BattlefieldBackground: () => null,
}));

vi.mock("../../components/stack/StackDisplay", () => ({
  StackDisplay: () => null,
}));

vi.mock("../../components/debug/DebugPanel", () => ({
  DebugPanel: () => null,
}));

vi.mock("../../components/hud/HUD", () => ({
  HUD: () => null,
}));

vi.mock("../../components/board/GameBoard", () => ({
  GameBoard: () => null,
}));

vi.mock("../../components/modal/EngineLostModal", () => ({
  EngineLostModal: () => null,
}));

vi.mock("../../components/modal/CardDataMissingModal", () => ({
  CardDataMissingModal: () => null,
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

vi.mock("../../adapter/draft-adapter", () => ({
  createDraftAdapter: vi.fn(),
}));

vi.mock("../../components/chrome/GameMenu", () => ({
  GameMenu: () => null,
}));

vi.mock("../../hooks/useCardDataMeta", () => ({
  useCardDataMeta: () => null,
  formatRelativeDate: () => "",
}));

// ── Helpers ──────────────────────────────────────────────────────────────────

function renderGamePage(initialEntry = "/game/test-game-123?mode=ai") {
  return render(
    <MemoryRouter initialEntries={[initialEntry]}>
      <Routes>
        <Route path="/game/:id" element={<GamePage />} />
        <Route path="/setup" element={<div data-testid="setup-page">Setup</div>} />
        <Route path="/" element={<div>Home</div>} />
      </Routes>
    </MemoryRouter>,
  );
}

// ── Test suite ────────────────────────────────────────────────────────────────

beforeEach(() => {
  capturedOnNoDeck = undefined;
  capturedFormatConfig = undefined;
  vi.clearAllMocks();
});

afterEach(() => {
  cleanup();
});

describe("GamePage — cEDH bracket-violation blocking modal", () => {
  it("passes Two-Headed Giant to GameProvider for a direct local URL", () => {
    renderGamePage("/game/test-game-123?format=TwoHeadedGiant&players=4");

    expect(capturedFormatConfig?.format).toBe("TwoHeadedGiant");
  });

  it("passes Two-Headed Giant to GameProvider for a direct AI URL", () => {
    renderGamePage("/game/test-game-123?mode=ai&format=TwoHeadedGiant&players=4");

    expect(capturedFormatConfig?.format).toBe("TwoHeadedGiant");
  });

  it("passes Planechase to GameProvider for a direct local URL", () => {
    renderGamePage("/game/test-game-123?format=Planechase&players=4");

    expect(capturedFormatConfig?.format).toBe("Planechase");
  });

  it("renders the blocking modal when bracketViolation flag is true", async () => {
    renderGamePage();

    // Simulate GameProvider calling onNoDeck with bracketViolation=true.
    // The modal must trigger on the typed flag, not on string substring.
    act(() => {
      capturedOnNoDeck?.(
        "Deck validation failed: seat 0 is not declared cEDH (actual tier: core)",
        true,
      );
    });

    const modal = await screen.findByTestId("bracket-violation-modal");
    expect(modal).toBeTruthy();
    expect(modal).toHaveTextContent(/Return to setup/i);
  });

  it("does NOT render the bracket-violation modal when bracketViolation flag is absent", () => {
    renderGamePage();

    // Same message text as above but no bracketViolation flag.
    // The modal must NOT trigger — string substring must not be the gate.
    act(() => {
      capturedOnNoDeck?.(
        "Deck validation failed: seat 0 is not declared cEDH (actual tier: core)",
        // bracketViolation intentionally omitted
      );
    });

    expect(screen.queryByTestId("bracket-violation-modal")).toBeNull();
  });

  it("does NOT render the bracket-violation modal for unrelated engine errors", () => {
    renderGamePage();

    act(() => {
      capturedOnNoDeck?.("Deck validation failed: Forest is not legal in Standard");
    });

    expect(screen.queryByTestId("bracket-violation-modal")).toBeNull();
  });

  it("does NOT render the bracket-violation modal when no error is present", () => {
    renderGamePage();
    expect(screen.queryByTestId("bracket-violation-modal")).toBeNull();
  });

  it("navigates to /setup when the 'Return to setup' button is clicked", async () => {
    const user = userEvent.setup();
    renderGamePage();

    act(() => {
      capturedOnNoDeck?.(
        "Deck validation failed: seat 1 is not declared cEDH (actual tier: optimized)",
        true,
      );
    });

    const button = await screen.findByRole("button", { name: /return to setup/i });
    await user.click(button);

    // After clicking, the modal should be gone and /setup rendered.
    expect(screen.queryByTestId("bracket-violation-modal")).toBeNull();
    expect(await screen.findByTestId("setup-page")).toBeTruthy();
  });

  // ── Regression: bracket-5 human deck vs non-cEDH AI must be allowed ────────

  it("REGRESSION: bracketViolation=false with a bracket-5 message does not show modal", () => {
    renderGamePage();

    // This is the regression case: a bracket-5 user deck playing against
    // Easy/Hard AI should never trigger the bracket-violation modal.
    // GameProvider will pass bracketViolation=false (or omit it), so even
    // if the error message mentions cEDH, the modal must not fire.
    act(() => {
      capturedOnNoDeck?.(
        "Deck validation failed: some other error",
        false,
      );
    });

    expect(screen.queryByTestId("bracket-violation-modal")).toBeNull();
  });
});
