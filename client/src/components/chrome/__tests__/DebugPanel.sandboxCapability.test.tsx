/**
 * DebugPanel under desktop solo-vs-AI (`native-ai`).
 *
 * Unlike GamePage — whose `mode` is URL-derived and can never be `native-ai` —
 * DebugPanel subscribes to the store's `gameMode`, so it genuinely observes
 * the value. These tests pin the two things the server-side capability change
 * must NOT alter on the client: checkpoint restore stays off (a transport
 * limit, not a mode policy), and the host grant/revoke console stays hidden.
 */
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DebugPanel } from "../DebugPanel";
import { WebSocketAdapter } from "../../../adapter/ws-adapter.ts";
import type { GameMode } from "../../../stores/gameStore";

const storeState = {
  gameMode: "native-ai" as GameMode | null,
  turnCheckpoints: [] as unknown[],
  gameState: null as unknown,
  // The desktop sidecar is reached through a `WebSocketAdapter`, same as
  // online play. `DebugCreateActions` gates card spawning on the adapter
  // type, so this has to be a real instance — an `instanceof` check cannot
  // be satisfied by a duck-typed object.
  adapter: null as unknown,
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

// `usePerspectivePlayerId` is only needed once `CreateCardForm` actually
// renders — its `PlayerSelect`/`ObjectSelect` reach for it. The gated branch
// never does, which is itself evidence the two branches render different
// subtrees rather than the same one with different copy.
vi.mock("../../../hooks/usePlayerId", () => ({
  usePlayerId: () => 0,
  usePerspectivePlayerId: () => 0,
}));
vi.mock("../../../hooks/useGameDispatch", () => ({ useGameDispatch: () => vi.fn() }));
vi.mock("../../../game/dispatch", () => ({ restoreGameState: vi.fn() }));
vi.mock("../../../audio/AudioManager", () => ({
  audioManager: { play: vi.fn(), diagnostics: () => "" },
}));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (k: string) => k }) }));
// `CardNameAutocomplete` fetches the full name list on mount via a build-time
// define that vitest does not provide. Only the ungated branch mounts it, and
// the suggestion list is not what these tests measure — the input's presence
// is.
vi.mock("../../../services/cardNames", () => ({ getCardNames: async () => [] }));

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

  /**
   * `Debug::CreateCard` is the sandbox panel's headline capability and it
   * cannot work on the sidecar: only `engine-wasm` intercepts it, `apply()`
   * returns `InvalidAction`, and `server-core`'s `handle_action` has no
   * `CardDatabase` to resolve a name against. Granting desktop solo the panel
   * without gating this form would ship a control whose only possible outcome
   * is "CreateCard failed". Pre-existing for shared-server sandbox, newly
   * surfaced here.
   */
  class SidecarAdapter extends WebSocketAdapter {
    // Real superclass construction — the `instanceof` check the component uses
    // cannot be satisfied by a duck-typed object. The constructor only assigns
    // fields; it opens no socket, so no transport is stubbed here.
    constructor() {
      super("ws://test/ws", "host", { main_deck: [], sideboard: [] });
    }
  }

  /** Create tab, then the "Create Card" accordion — both start collapsed. */
  function openCreateCardAccordion() {
    fireEvent.click(screen.getByRole("button", { name: "Create" }));
    fireEvent.click(screen.getByRole("button", { name: /Create Card/ }));
  }

  it("explains why card spawning is unavailable on the sidecar transport", () => {
    uiState.debugPanelTab = "actions";
    storeState.adapter = new SidecarAdapter();

    render(<DebugPanel />);
    openCreateCardAccordion();

    // Reach guard: the Create tab really rendered its body, so the missing
    // form below is the transport gate and not an unrendered subtree. The
    // token accordions are the siblings that DO work on this transport.
    expect(screen.getByText("Create Token (Catalog)")).toBeInTheDocument();
    expect(
      screen.getByText(/Spawning a card by name needs the in-browser engine/),
    ).toBeInTheDocument();
    // The revert-failing assertion: the card-name input's placeholder is
    // unique to `CreateCardForm`. Remove the gate and the form renders and
    // this is found. (Its submit button is NOT usable here — it shares the
    // accordion header's accessible name.)
    expect(screen.queryByPlaceholderText("Lightning Bolt")).toBeNull();
  });

  it("still offers card spawning when the in-browser engine is behind the adapter", () => {
    // Paired positive, and the proof the gate is a transport check rather
    // than "always off": `null` stands for any non-WebSocket adapter, i.e.
    // browser solo's `WasmAdapter`, where `CreateCard` is intercepted and
    // genuinely works.
    uiState.debugPanelTab = "actions";
    storeState.adapter = null;

    render(<DebugPanel />);
    openCreateCardAccordion();

    expect(screen.getByPlaceholderText("Lightning Bolt")).toBeInTheDocument();
    expect(
      screen.queryByText(/Spawning a card by name needs the in-browser engine/),
    ).toBeNull();
  });
});
