/**
 * F1 — DebugPanel's server-published turn-rewind list.
 *
 * The Turn Checkpoints section has two independent sources now:
 *
 *   1. Server-published boundaries (`rewindTargets`), offered only when the
 *      installed adapter declares `ServerRewindCapability`. This is the desktop
 *      solo-vs-AI path, where the sidecar owns the state and a local restore
 *      would desync.
 *   2. The pre-existing browser-local `turnCheckpoints`, restored through
 *      `restoreGameState`, which only the in-browser engine supports.
 *
 * These tests pin that the two never cross: a wire session must not call
 * `restoreGameState`, and a local session must not send a wire frame.
 */
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DebugPanel } from "../DebugPanel";
import type { GameMode } from "../../../stores/gameStore";
import { restoreGameState } from "../../../game/dispatch";

const sendRequestTakeback = vi.fn();

/** A stub shaped exactly like `WebSocketAdapter`'s capability surface. */
function rewindCapableAdapter() {
  return { supportsServerRewind: true, sendRequestTakeback };
}

/** A P2P-shaped stub: wire-authoritative, but no rollback capability. */
function rewindIncapableAdapter() {
  return { supportsMatchConcede: true, sendMatchConcede: vi.fn() };
}

const storeState = {
  gameMode: "native-ai" as GameMode | null,
  turnCheckpoints: [] as unknown[],
  rewindTargets: [] as unknown[],
  gameState: null as unknown,
  adapter: null as unknown,
};

vi.mock("../../../stores/gameStore", () => ({
  canExportAuthoritativeState: (mode: GameMode | null) =>
    mode === "ai" || mode === "local" || mode === "native-ai",
  useGameStore: Object.assign(
    vi.fn((selector: (s: typeof storeState) => unknown) => selector(storeState)),
    { getState: () => storeState, setState: vi.fn() },
  ),
}));

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

vi.mock("../../../hooks/usePlayerId", () => ({
  usePlayerId: () => 0,
  usePerspectivePlayerId: () => 0,
}));
vi.mock("../../../hooks/useGameDispatch", () => ({ useGameDispatch: () => vi.fn() }));
vi.mock("../../../game/dispatch", () => ({ restoreGameState: vi.fn(async () => null) }));
vi.mock("../../../audio/AudioManager", () => ({
  audioManager: { play: vi.fn(), diagnostics: () => "" },
}));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (k: string) => k }) }));
vi.mock("../../../services/cardNames", () => ({ getCardNames: async () => [] }));

beforeEach(() => {
  storeState.gameMode = "native-ai";
  storeState.turnCheckpoints = [];
  storeState.rewindTargets = [];
  storeState.gameState = { players: [{}, {}], seat_order: [0, 1] };
  storeState.adapter = null;
  uiState.debugPanelTab = "console";
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("DebugPanel — server-published turn rewind", () => {
  it("renders the server's boundaries and asks the server to roll back to one", () => {
    storeState.adapter = rewindCapableAdapter();
    storeState.rewindTargets = [{ turn_number: 3, active_player: 1 }];

    render(<DebugPanel />);

    // At BASE_SHA this section rendered the "Restore needs the in-browser
    // engine" notice and no button existed at all.
    const button = screen.getByRole("button", { name: /Turn 3/ });
    fireEvent.click(button);

    // The exact payload is the contract with `server-core`'s `RewindTarget`.
    expect(sendRequestTakeback).toHaveBeenCalledWith({
      kind: "turn_start",
      turn_number: 3,
    });
    // A wire session must NEVER take the local restore path — that adapter's
    // `restoreState` throws.
    expect(restoreGameState).not.toHaveBeenCalled();
  });

  it("still uses the local checkpoint list for a browser solo game", () => {
    // Paired positive. Without a capability the local chain must be reached
    // exactly as before, proving the new branch is additive.
    storeState.gameMode = "ai";
    storeState.adapter = rewindIncapableAdapter();
    storeState.turnCheckpoints = [
      { turn_number: 4, active_player: 0, seat_order: [0, 1] },
    ];

    render(<DebugPanel />);

    fireEvent.click(screen.getByRole("button", { name: /Turn 4/ }));

    expect(restoreGameState).toHaveBeenCalledTimes(1);
    expect(sendRequestTakeback).not.toHaveBeenCalled();
  });

  it("renders neither list for a wire adapter without the capability", () => {
    // Hostile: a P2P-shaped adapter is wire-authoritative but cannot bind a
    // rollback request. It must fall through to the existing notice, not to a
    // button that no-ops.
    storeState.adapter = rewindIncapableAdapter();
    storeState.rewindTargets = [{ turn_number: 3, active_player: 1 }];

    render(<DebugPanel />);

    expect(screen.queryByRole("button", { name: /Turn 3/ })).toBeNull();
    expect(
      screen.getByText(
        "Restore needs the in-browser engine. Use Takeback to undo your last action.",
      ),
    ).toBeInTheDocument();
  });

  it("falls through to the existing chain on an honestly empty list", () => {
    // Hostile: an online table HAS the capability but a permanently empty list
    // (the server scopes turn rewind to a SingleUser sidecar). It must look
    // byte-identical to before rather than showing a dead empty list.
    storeState.gameMode = "online";
    storeState.adapter = rewindCapableAdapter();
    storeState.rewindTargets = [];

    render(<DebugPanel />);

    expect(
      screen.getByText(
        "Restore needs the in-browser engine. Use Takeback to undo your last action.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Turn / })).toBeNull();
  });
});
