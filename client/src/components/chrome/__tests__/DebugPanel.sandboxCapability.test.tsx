/**
 * DebugPanel under desktop solo-vs-AI (`native-ai`).
 *
 * Unlike GamePage — whose `mode` is URL-derived and can never be `native-ai` —
 * DebugPanel subscribes to the store's `gameMode`, so it genuinely observes
 * the value. These tests pin the two things the server-side capability change
 * must NOT alter on the client: checkpoint restore stays off (a transport
 * limit, not a mode policy), and the host grant/revoke console stays hidden.
 */
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DebugPanel } from "../DebugPanel";
import type { GameMode } from "../../../stores/gameStore";

const storeState = {
  gameMode: "native-ai" as GameMode | null,
  turnCheckpoints: [] as unknown[],
  gameState: null as unknown,
};

vi.mock("../../../stores/gameStore", () => ({
  useGameStore: Object.assign(
    vi.fn((selector: (s: typeof storeState) => unknown) => selector(storeState)),
    { getState: () => storeState, setState: vi.fn() },
  ),
}));

// `console` shows Turn Checkpoints / Import State; `actions` shows
// DebugActions. Both are exercised below, so the tab is mutable.
const uiState = {
  debugPanelOpen: true,
  debugPanelTab: "console" as "console" | "actions",
  setDebugPanelTab: vi.fn(),
};

vi.mock("../../../stores/uiStore", () => ({
  useUiStore: Object.assign(
    vi.fn((selector: (s: typeof uiState) => unknown) => selector(uiState)),
    { getState: () => uiState, setState: vi.fn() },
  ),
}));

vi.mock("../../../hooks/usePlayerId", () => ({ usePlayerId: () => 0 }));
vi.mock("../../../hooks/useGameDispatch", () => ({ useGameDispatch: () => vi.fn() }));
vi.mock("../../../game/dispatch", () => ({ restoreGameState: vi.fn() }));
vi.mock("../../../audio/AudioManager", () => ({
  audioManager: { play: vi.fn(), diagnostics: () => "" },
}));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (k: string) => k }) }));

/**
 * The m8 fixture. After the server change a `native-ai` game has a POPULATED
 * `debug_permitted` (`[0]`) and `player_count` 2, so
 * `debugPermitted.length > 0 && debugPermitted.length < playerCount` is TRUE.
 * That leaves `allow_debug_actions === false` as the ONLY thing keeping the
 * grant/revoke console hidden — which is exactly the property under test.
 * A fixture with an empty set would pass for the wrong reason and would stop
 * guarding the decision to leave the sandbox format flag off.
 */
function nativeAiSandboxState() {
  return {
    debug_permitted: [0],
    players: [{}, {}],
    format_config: { allow_debug_actions: false },
  };
}

beforeEach(() => {
  storeState.gameMode = "native-ai";
  storeState.turnCheckpoints = [];
  storeState.gameState = nativeAiSandboxState();
  uiState.debugPanelTab = "console";
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("DebugPanel — desktop solo capability", () => {
  it("says why checkpoint restore is unavailable instead of blaming multiplayer", () => {
    render(<DebugPanel />);

    expect(
      screen.getByText(
        "Restore needs the in-browser engine. Use Takeback to undo your last action.",
      ),
    ).toBeInTheDocument();
    // The assertion that fails on revert: the old copy called a solo game
    // multiplayer. Asserting only the new string's presence would pass if
    // both rendered.
    expect(screen.queryByText(/multiplayer/i)).toBeNull();
  });

  it("still shows the checkpoint notice for browser solo, where restore works", () => {
    // Reach guard for the negative above: with a mode whose adapter DOES
    // implement `restoreState`, the notice is replaced by the empty-state,
    // proving the branch is mode-sensitive rather than always rendering.
    storeState.gameMode = "ai";

    render(<DebugPanel />);

    expect(screen.getByText("No checkpoints yet (saved at turn start)")).toBeInTheDocument();
    expect(
      screen.queryByText(
        "Restore needs the in-browser engine. Use Takeback to undo your last action.",
      ),
    ).toBeNull();
  });

  it("keeps the host grant/revoke console hidden on a native-ai game", () => {
    uiState.debugPanelTab = "actions";

    render(<DebugPanel />);

    // Reach guard: the debug actions panel itself rendered and the seat is
    // permitted, so the console's absence is `allow_debug_actions === false`
    // and not an unrendered subtree or a "disabled for this seat" bailout.
    expect(screen.getByText("Debug Actions")).toBeInTheDocument();
    expect(screen.queryByText(/disabled for this seat/i)).toBeNull();
    // This is the test that fails if an implementer takes the rejected
    // `FormatConfig::with_sandbox()` shortcut on the server: flipping
    // `allow_debug_actions` to true makes `hasRevocation` true (1 < 2) and
    // this console appears for the first time.
    expect(screen.queryByText(/grant/i)).toBeNull();
  });
});
