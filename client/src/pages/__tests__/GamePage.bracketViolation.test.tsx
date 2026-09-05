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
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes, useLocation } from "react-router";
import { MotionGlobalConfig } from "framer-motion";

import { GamePage } from "../GamePage";
import type { FormatConfig } from "../../adapter/types";
import type { WsAdapterEvent } from "../../adapter/ws-adapter";
import type { P2PAdapterEvent } from "../../adapter/p2p-adapter";
import { P2PHostAdapter } from "../../adapter/p2p-adapter";
import { WebSocketAdapter } from "../../adapter/ws-adapter";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { useUiStore } from "../../stores/uiStore";
import { gameObjectFactory } from "../../test/factories/gameObjectFactory";
import {
  buildCommanderFormatConfig,
  gameStateFactory,
} from "../../test/factories/gameStateFactory";

// ── Hoisted variables (must be declared before vi.mock hoisting) ─────────────

// Capture `onNoDeck` from GameProvider so tests can fire it.
let capturedOnNoDeck: ((reason?: string, bracketViolation?: boolean) => void) | undefined;
let capturedFormatConfig: FormatConfig | undefined;
let capturedOnWsEvent: ((event: WsAdapterEvent) => void) | undefined;
let capturedOnP2PEvent: ((event: P2PAdapterEvent) => void) | undefined;
// The join/spectate origin the route carried, handed down as a provider prop.
let capturedServerUrl: string | undefined;

const { mockClearPromptOverlayState, mockIsMobile, mockSetGameState, storeOverrides } = vi.hoisted(() => ({
  mockClearPromptOverlayState: vi.fn(),
  mockIsMobile: vi.fn(() => false),
  mockSetGameState: vi.fn(),
  // Mutable slice of the mocked game store. Defaults match the previous
  // hardcoded values, so every pre-existing test is unaffected; tests that
  // need a live adapter assign here and `beforeEach` resets.
  storeOverrides: {
    adapter: null as unknown,
    gameState: null as unknown,
    gameMode: null as unknown,
    waitingFor: null as unknown,
    activationBlockReasons: {} as Record<string, Array<{ ability_index: number; type: string }>>,
  },
}));

// Captures the props GameMenu was rendered with, so tests can assert which
// affordances GamePage decided to offer without rendering the real menu.
let capturedGameMenuProps: Record<string, unknown> | undefined;

const { mockMultiplayerState, mockUseMultiplayerStore } = vi.hoisted(() => {
  const mockMultiplayerState = {
    serverInfo: null,
    activePlayerId: null,
    playerNames: new Map<string, string>(),
    playerAvatars: new Map<string, string>(),
    connectionStatus: "disconnected",
    isSpectator: false,
    // Keyed Map, matching the real store — ConnectionToast reads `.size`.
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
    onWsEvent,
    onP2PEvent,
    formatConfig,
    serverUrl,
  }: {
    children: React.ReactNode;
    onNoDeck?: (reason?: string, bracketViolation?: boolean) => void;
    onWsEvent?: (event: WsAdapterEvent) => void;
    onP2PEvent?: (event: P2PAdapterEvent) => void;
    formatConfig?: FormatConfig;
    serverUrl?: string;
  }) => {
    capturedOnNoDeck = onNoDeck;
    capturedOnWsEvent = onWsEvent;
    capturedOnP2PEvent = onP2PEvent;
    capturedFormatConfig = formatConfig;
    capturedServerUrl = serverUrl;
    return <>{children}</>;
  },
}));

vi.mock("../../game/sessionCleanup.ts", () => ({
  clearPromptOverlayState: mockClearPromptOverlayState,
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

vi.mock("../../stores/gameStore", async () => ({
  useGameStore: Object.assign(
    vi.fn((selector: (s: Record<string, unknown>) => unknown) =>
      selector({
        gameState: storeOverrides.gameState,
        gameMode: storeOverrides.gameMode,
        waitingFor: storeOverrides.waitingFor,
        legalActions: [],
        endContinuousEffectOffers: [],
        autoPassRecommended: false,
        spellCosts: {},
        legalActionsByObject: {},
        // CR 118.3: the acting-player "can't pay this cost right now" read-out.
        // Mutable so a test can seed it; reset in `beforeEach` alongside the
        // other `storeOverrides` fields.
        activationBlockReasons: storeOverrides.activationBlockReasons,
        events: [],
        eventHistory: [],
        logHistory: [],
        adapter: storeOverrides.adapter,
        lobbyProgress: null,
      }),
    ),
    { setState: mockSetGameState },
  ),
  clearGame: vi.fn(),
  // The real predicate, and `importActual` is what makes that claim true.
  // `hasRemoteHumans` reads `GAME_MODE_TRAITS`, a frozen taxonomy that lives
  // only in this module; re-deriving it here as `mode === "online" || …` would
  // pass while the real predicate classified a mode differently — exactly the
  // failure the `takebackAudience` assertions below exist to catch. Only the
  // predicate is taken: `useGameStore` stays the mock above, so the real
  // zustand store is imported but never rendered against.
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
  // useTurnStatus (reached via the mounted <TurnStatusLine/>) also imports
  // waitingPlayer from this module; the whole module is mocked, so it must be
  // re-declared or the call throws. gameStore is mocked with waitingFor: null,
  // for which the real waitingPlayer returns null — mirror that here.
  waitingPlayer: () => null,
}));

vi.mock("../../hooks/useIsMobile", () => ({
  useIsMobile: () => mockIsMobile(),
  useIsCompactHeight: () => false,
}));

vi.mock("../../audio/useAudioContext", () => ({
  useAudioContext: () => undefined,
}));

vi.mock("../../hooks/useGameplayPreferencesSync", () => ({
  useGameplayPreferencesSync: () => undefined,
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
  GameBoard: (props: Record<string, unknown>) => {
    return (
      <div
        data-layout={String(props.effectiveMultiplayerBoardLayout)}
        data-testid="game-board-layout"
      />
    );
  },
}));

vi.mock("../../components/modal/EngineLostModal", () => ({
  EngineLostModal: () => null,
}));

vi.mock("../../components/modal/CardDataMissingModal", () => ({
  CardDataMissingModal: () => null,
}));

// This is the leaf of the production ability-choice path. Keeping the mock at
// the rendering boundary lets the test exercise GamePage's module-private
// AbilityChoiceModal and observe the actual labels it supplies without pulling
// card-art loading into a label-wiring test.
// `onChoose` is wired through and called UNCONDITIONALLY — deliberately WITHOUT
// re-implementing the real `ChoiceModal`'s `opt.disabled` guard. That guard is
// tested against the real component in
// `components/modal/__tests__/ChoiceModal.test.tsx`; mirroring it here would
// make GamePage's OWN `blocked:` / `!action` guard unreachable, and this mock
// exists precisely so a `blocked:` id can reach it.
vi.mock("../../components/modal/ChoiceModal", () => ({
  ChoiceModal: ({
    options,
    onChoose,
  }: {
    options: Array<{ id: string; label: string; description?: string; disabled?: boolean }>;
    onChoose: (id: string) => void;
  }) => (
    <div data-testid="ability-choice-options">
      {options.map((option) => (
        <button
          key={option.id}
          type="button"
          data-option-id={option.id}
          data-option-description={option.description}
          data-option-disabled={option.disabled ? "true" : undefined}
          onClick={() => onChoose(option.id)}
        >
          {option.label}
        </button>
      ))}
    </div>
  ),
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
  GameMenu: (props: Record<string, unknown>) => {
    capturedGameMenuProps = props;
    return (
      <button
        ref={props.menuTriggerRef as React.Ref<HTMLButtonElement> | undefined}
        type="button"
      >
        Game menu
      </button>
    );
  },
}));

let capturedConcedeDialogProps: Record<string, unknown> | undefined;
vi.mock("../../components/multiplayer/ConcedeDialog", () => ({
  ConcedeDialog: (props: Record<string, unknown>) => {
    capturedConcedeDialogProps = props;
    return null;
  },
}));

vi.mock("../../hooks/useCardDataMeta", () => ({
  useCardDataMeta: () => null,
  formatRelativeDate: () => "",
}));

// ── Helpers ──────────────────────────────────────────────────────────────────

/** Renders whatever router state the deck-rejected re-entry navigated with,
 * so a dropped field is visible rather than inferred. */
function MultiplayerStub() {
  const { state } = useLocation();
  return <div data-testid="multiplayer-stub">{JSON.stringify(state)}</div>;
}

function gamePageTree(
  initialEntry: string | { pathname: string; search: string; state: unknown } =
    "/game/test-game-123?mode=ai",
) {
  return (
    <MemoryRouter initialEntries={[initialEntry as never]}>
      <Routes>
        <Route path="/game/:id" element={<GamePage />} />
        <Route path="/setup" element={<div data-testid="setup-page">Setup</div>} />
        <Route path="/multiplayer" element={<MultiplayerStub />} />
        <Route path="/" element={<div>Home</div>} />
      </Routes>
    </MemoryRouter>
  );
}

function renderGamePage(
  initialEntry: string | { pathname: string; search: string; state: unknown } =
    "/game/test-game-123?mode=ai",
) {
  return render(gamePageTree(initialEntry));
}

async function closePreferencesAndExpectGameMenuFocus(): Promise<void> {
  const dialog = await screen.findByRole("dialog", { name: "Settings" });
  await closeDialogAndExpectGameMenuFocus(dialog);
}

async function closeDialogAndExpectGameMenuFocus(
  dialog: HTMLElement,
): Promise<void> {
  await closeDialogAndExpectFocus(
    dialog,
    screen.getByRole("button", { name: "Game menu" }),
  );
}

async function closeDialogAndExpectFocus(
  dialog: HTMLElement,
  returnTarget: HTMLElement,
): Promise<void> {
  await waitFor(() => expect(dialog).toHaveFocus());
  fireEvent.keyDown(dialog, { key: "Escape" });
  await waitFor(() =>
    expect(dialog).not.toBeInTheDocument(),
  );
  expect(returnTarget).toHaveFocus();
}

// ── Test suite ────────────────────────────────────────────────────────────────

beforeEach(() => {
  capturedOnNoDeck = undefined;
  capturedFormatConfig = undefined;
  capturedOnWsEvent = undefined;
  capturedOnP2PEvent = undefined;
  capturedServerUrl = undefined;
  capturedGameMenuProps = undefined;
  storeOverrides.adapter = null;
  storeOverrides.gameState = null;
  storeOverrides.gameMode = null;
  storeOverrides.waitingFor = null;
  storeOverrides.activationBlockReasons = {};
  useUiStore.setState({ pendingAbilityChoice: null });
  mockIsMobile.mockReturnValue(false);
  usePreferencesStore.setState({
    multiplayerBoardLayout: "focused",
    multiplayerSplitLayoutNudgeDismissed: true,
  });
  capturedConcedeDialogProps = undefined;
  vi.clearAllMocks();
});

afterEach(() => {
  cleanup();
});

describe("GamePage — cEDH bracket-violation blocking modal", () => {
  it("clears prompt overlays before websocket and P2P game-over displays", () => {
    renderGamePage("/game/test-game-123?mode=host");

    act(() => {
      capturedOnWsEvent?.({ type: "gameOver", winner: 0, reason: "conceded" });
    });

    cleanup();
    renderGamePage("/game/test-game-123?mode=p2p-host");

    act(() => {
      capturedOnP2PEvent?.({ type: "gameOver", winner: 0, reason: "conceded" });
    });

    expect(mockClearPromptOverlayState).toHaveBeenCalledTimes(2);
    expect(mockSetGameState).toHaveBeenNthCalledWith(1, {
      waitingFor: { type: "GameOver", data: { winner: 0 } },
    });
    expect(mockSetGameState).toHaveBeenNthCalledWith(2, {
      waitingFor: { type: "GameOver", data: { winner: 0 } },
    });
  });

  it("renders the connection-lost banner when a native engine error arrives before close", () => {
    renderGamePage();

    // NativeEngineSocket emits error before close. GameProvider disposes on the
    // error, so close cannot emit the reconnectFailed event the banner normally
    // consumes. This drives the pre-close adapter event directly.
    act(() => {
      capturedOnWsEvent?.({ type: "error", message: "WebSocket connection failed" });
    });

    expect(screen.getByText("Connection lost")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Return to Menu" })).toBeInTheDocument();
  });

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

  // The setup screen hands its edited config over on the navigation rather than
  // in the URL, which carries the format NAME only. Without this the memo falls
  // back to the format registry and a custom starting life is silently replaced
  // by the format default — including on the Tauri native route, which writes no
  // resume pointer and so has no other copy of the user's choice.
  it("prefers the setup screen's handed-over config over the format default", () => {
    renderGamePage({
      pathname: "/game/test-game-123",
      search: "?mode=ai&format=Commander&players=2",
      state: { formatConfig: { format: "Commander", starting_life: 25 } },
    });

    expect(capturedFormatConfig?.starting_life).toBe(25);
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

describe("GamePage — P2P pause resume control", () => {
  it("wires Resume to a P2P host adapter", async () => {
    const requestResume = vi.fn();
    const host = Object.create(P2PHostAdapter.prototype) as P2PHostAdapter;
    host.requestResume = requestResume;
    storeOverrides.adapter = host;

    renderGamePage("/game/test-game-123?mode=p2p-host");
    act(() => { capturedOnP2PEvent?.({ type: "gamePaused", reason: "Paused by host" }); });

    await userEvent.setup().click(screen.getByRole("button", { name: "Resume game" }));
    expect(requestResume).toHaveBeenCalledOnce();
  });

  it("does not expose Resume to a P2P guest", () => {
    renderGamePage("/game/test-game-123?mode=p2p-join");
    act(() => { capturedOnP2PEvent?.({ type: "gamePaused", reason: "Paused by host" }); });

    expect(screen.queryByRole("button", { name: "Resume game" })).toBeNull();
  });
});

describe("GamePage — Room unlock labels", () => {
  it("passes copied Room half identities into its private ability-choice consumer", () => {
    const roomCopy = gameObjectFactory
      .enchantment()
      .onBattlefield()
      .withId(8294)
      .named("")
      .build();
    const gameState = gameStateFactory
      .withPlayers(0, 1)
      .withObjects(roomCopy)
      .priority(0)
      .build({
        derived: {
          room_half_identities: {
            [String(roomCopy.id)]: {
              left: { name: "Greenhouse", mana_cost: { type: "Cost", generic: 2, shards: ["Green"] } },
              right: { name: "Rickety Gazebo", mana_cost: { type: "Cost", generic: 3, shards: ["Green"] } },
            },
          },
        },
      });
    storeOverrides.gameState = gameState;
    storeOverrides.waitingFor = gameState.waiting_for;
    useUiStore.setState({
      pendingAbilityChoice: {
        objectId: roomCopy.id,
        actions: [
          { type: "UnlockRoomDoor", data: { object_id: roomCopy.id, door: "Left" } },
          { type: "UnlockRoomDoor", data: { object_id: roomCopy.id, door: "Right" } },
        ],
      },
    });

    renderGamePage();

    expect(screen.getByRole("button", { name: "Unlock Greenhouse ({2}{G})" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Unlock Rickety Gazebo ({3}{G})" })).toBeInTheDocument();
  });
});

describe("GamePage — CR 118.3 unaffordable-ability rows in the ability picker", () => {
  const ENGINE_ID = 9301 as const;
  // Index 0 is OFFERED (an action row); index 1 is WITHHELD by the engine
  // because its cost is unpayable right now, and is the row this feature adds.
  const OFFERED_DESC = "{1}: Draw a card.";
  const BLOCKED_DESC = "{7}: Search your library for a Sliver card.";
  // The localized reason lands in the row's `description`, which this file's
  // `ChoiceModal` mock deliberately does not render: appending it inside the
  // <button> would change the accessible names the Room-unlock test above
  // asserts on. The reason text is covered against the REAL component in
  // `components/modal/__tests__/ChoiceModal.test.tsx` (rows 15/16).

  /** Seed a board whose picker is open on `ENGINE_ID` with one offered ability. */
  function seedPicker(activationBlockReasons: Record<string, Array<{ ability_index: number; type: string }>>) {
    const engine = gameObjectFactory
      .creature(2, 2)
      .onBattlefield()
      .withId(ENGINE_ID)
      .ownedBy(0)
      .named("Costly Engine")
      .build({
        abilities: [
          { description: OFFERED_DESC },
          { description: BLOCKED_DESC },
        ] as never,
      });
    const gameState = gameStateFactory
      .withPlayers(0, 1)
      .withObjects(engine)
      .priority(0)
      .build();
    storeOverrides.gameState = gameState;
    storeOverrides.waitingFor = gameState.waiting_for;
    storeOverrides.activationBlockReasons = activationBlockReasons;
    useUiStore.setState({
      pendingAbilityChoice: {
        objectId: ENGINE_ID,
        actions: [
          { type: "ActivateAbility", data: { source_id: ENGINE_ID, ability_index: 0 } },
        ],
      },
    });
    renderGamePage();
  }

  /** Every option button the private `AbilityChoiceModal` supplied, in DOM order. */
  function optionIds(): string[] {
    return Array.from(
      screen.getByTestId("ability-choice-options").querySelectorAll("button"),
    ).map((b) => b.getAttribute("data-option-id") ?? "");
  }

  function optionLabels(): string[] {
    return Array.from(
      screen.getByTestId("ability-choice-options").querySelectorAll("button"),
    ).map((b) => b.textContent ?? "");
  }

  /**
   * Select by the option's ID, not by its accessible name. `abilityChoiceLabel`
   * splits an ActivateAbility description at the colon (the offered row renders
   * as `{1}`, not the whole sentence), and most rows here are not about that
   * formatting — binding to it would make the test fail for an unrelated
   * viewmodel change. The one row that IS about it asserts the split explicitly.
   */
  function optionButton(id: string): HTMLElement {
    const el = screen
      .getByTestId("ability-choice-options")
      .querySelector(`[data-option-id="${id}"]`);
    expect(el, `option ${id} must be rendered`).not.toBeNull();
    return el as HTMLElement;
  }

  // Row 17 (a) — THE USER-VISIBLE HALF OF THE FIX. The reported defect was that
  // an ability the engine withholds for cost simply vanished from the picker.
  // The blocked row must be appended AFTER the action rows, because the offered
  // rows' ids are positional (`String(i)` <-> `pending.actions[Number(id)]`) and
  // prepending would silently re-index every dispatch.
  it("appends a non-selectable row per withheld ability, after the offered rows", () => {
    seedPicker({ [String(ENGINE_ID)]: [{ ability_index: 1, type: "CostNotPayableNow" }] });

    // The load-bearing claim: the blocked row exists, and it is LAST.
    expect(optionIds()).toEqual(["0", "blocked:1"]);
    // The blocked row uses the SAME label/description split as the offered rows
    // (`abilityLabel` + `stripCostPrefix`), so the bold line is the cost pip on
    // both and the two are visually comparable in one list — the comparison the
    // reported defect is about. Asserting the split, not just "some label".
    expect(optionLabels()[1]).toBe("{7}");
    expect(optionButton("blocked:1")).toHaveAttribute(
      "data-option-description",
      "Search your library for a Sliver card. — You can't pay this cost right now",
    );
    // PAIRED POSITIVE for the convention itself: the OFFERED row's label is a
    // bare cost too, so the assertion above pins a shared shape rather than a
    // coincidence of this fixture.
    expect(optionLabels()[0]).toBe("{1}");
    expect(optionButton("blocked:1")).toHaveAttribute("data-option-disabled", "true");
    // PAIRED POSITIVE, mandatory: the offered row is NOT disabled, so a modal
    // that disabled everything cannot satisfy the assertion above.
    expect(optionButton("0")).not.toHaveAttribute("data-option-disabled");
  });

  // Row 17 (b) — the empty-read-out control. With no withheld abilities the
  // picker is byte-for-byte what it was before this feature, so the change is
  // additive rather than a rewrite of the option list.
  it("adds no rows when the engine withholds nothing", () => {
    seedPicker({});

    expect(optionIds()).toEqual(["0"]);
  });

  // Row 17 (c) — GamePage's OWN dispatch guard, reached through the real
  // `onChoose`. `setPending(null)` is the observable: a chosen action clears
  // `pendingAbilityChoice`, so "the picker stays open" is the signal that the
  // guard refused the id. `useUiStore` is the real store here, not a mock.
  it("refuses to dispatch a blocked row's id while still dispatching a real one", () => {
    seedPicker({ [String(ENGINE_ID)]: [{ ability_index: 1, type: "CostNotPayableNow" }] });

    fireEvent.click(optionButton("blocked:1"));
    expect(
      useUiStore.getState().pendingAbilityChoice,
      "clicking a blocked row must not resolve the choice",
    ).not.toBeNull();

    // PAIRED POSITIVE, mandatory: the SAME handler in the SAME render does
    // resolve the choice for a real action row, so the refusal above is a
    // refusal and not a dead modal.
    fireEvent.click(optionButton("0"));
    expect(
      useUiStore.getState().pendingAbilityChoice,
      "clicking an offered row must resolve the choice",
    ).toBeNull();
  });
});

describe("GamePage — multiplayer board layout during board choices", () => {
  it("forces split visibility for an authorized untap choice at a three-player table", () => {
    mockIsMobile.mockReturnValue(true);
    const untapCandidate = gameObjectFactory
      .creature(2, 2)
      .onBattlefield()
      .tapped()
      .withId(10)
      .ownedBy(0)
      .build();
    const gameState = gameStateFactory
      .withPlayers(0, 1, 2)
      .withObjects(untapCandidate)
      .untapChoice({ player: 0, candidates: [untapCandidate.id] })
      .build();
    storeOverrides.gameState = gameState;
    storeOverrides.waitingFor = gameState.waiting_for;

    renderGamePage();

    expect(screen.getByTestId("game-board-layout")).toHaveAttribute("data-layout", "split");
    expect(capturedGameMenuProps?.multiplayerBoardLayout).toBeUndefined();
    expect(capturedGameMenuProps?.showMultiplayerSplitLayoutNudge).toBe(false);
  });

  it("retains the persisted focused layout for a non-untap waiting state", () => {
    const permanent = gameObjectFactory
      .creature(2, 2)
      .onBattlefield()
      .withId(10)
      .ownedBy(0)
      .build();
    const gameState = gameStateFactory
      .withPlayers(0, 1, 2)
      .withObjects(permanent)
      .priority(0)
      .build();
    storeOverrides.gameState = gameState;
    storeOverrides.waitingFor = gameState.waiting_for;

    renderGamePage();

    expect(screen.getByTestId("game-board-layout")).toHaveAttribute("data-layout", "focused");
  });

  it("honors an explicit split preference on mobile outside an untap choice", () => {
    mockIsMobile.mockReturnValue(true);
    usePreferencesStore.setState({ multiplayerBoardLayout: "split" });
    const permanent = gameObjectFactory
      .creature(2, 2)
      .onBattlefield()
      .withId(10)
      .ownedBy(0)
      .build();
    const gameState = gameStateFactory
      .withPlayers(0, 1, 2)
      .withObjects(permanent)
      .priority(0)
      .build();
    storeOverrides.gameState = gameState;
    storeOverrides.waitingFor = gameState.waiting_for;

    renderGamePage();

    expect(screen.getByTestId("game-board-layout")).toHaveAttribute("data-layout", "split");
  });

  it.each([
    ["mobile", true, "focused"],
    ["desktop", false, "split"],
  ] as const)("resolves the auto layout for a %s viewport", (_viewport, isMobile, layout) => {
    mockIsMobile.mockReturnValue(isMobile);
    usePreferencesStore.setState({ multiplayerBoardLayout: "auto" });
    const gameState = gameStateFactory.withPlayers(0, 1, 2).priority(0).build();
    storeOverrides.gameState = gameState;
    storeOverrides.waitingFor = gameState.waiting_for;

    renderGamePage();

    expect(screen.getByTestId("game-board-layout")).toHaveAttribute("data-layout", layout);
    expect(capturedGameMenuProps?.multiplayerBoardLayout).toBe(layout);
  });

  it.each([
    ["mobile", true, "focused", "split"],
    ["desktop", false, "split", "focused"],
  ] as const)(
    "writes the opposite explicit choice when toggling auto on %s",
    (_viewport, isMobile, displayedLayout, expectedPreference) => {
      mockIsMobile.mockReturnValue(isMobile);
      usePreferencesStore.setState({ multiplayerBoardLayout: "auto" });
      const gameState = gameStateFactory.withPlayers(0, 1, 2).priority(0).build();
      storeOverrides.gameState = gameState;
      storeOverrides.waitingFor = gameState.waiting_for;

      renderGamePage();

      expect(capturedGameMenuProps?.multiplayerBoardLayout).toBe(displayedLayout);
      act(() => (capturedGameMenuProps?.onToggleMultiplayerBoardLayout as () => void)());
      expect(usePreferencesStore.getState().multiplayerBoardLayout).toBe(expectedPreference);
    },
  );

  it("offers the wide focused-layout nudge without writing preferences on render", () => {
    const permanent = gameObjectFactory
      .creature(2, 2)
      .onBattlefield()
      .withId(10)
      .ownedBy(0)
      .build();
    const gameState = gameStateFactory
      .withPlayers(0, 1, 2)
      .withObjects(permanent)
      .priority(0)
      .build();
    storeOverrides.gameState = gameState;
    storeOverrides.waitingFor = gameState.waiting_for;
    usePreferencesStore.setState({
      multiplayerBoardLayout: "focused",
      multiplayerSplitLayoutNudgeDismissed: false,
    });

    renderGamePage();

    expect(capturedGameMenuProps?.showMultiplayerSplitLayoutNudge).toBe(true);
    expect(usePreferencesStore.getState().multiplayerBoardLayout).toBe("focused");
    expect(usePreferencesStore.getState().multiplayerSplitLayoutNudgeDismissed).toBe(false);

    act(() => (capturedGameMenuProps?.onTryMultiplayerSplitLayout as () => void)());
    expect(usePreferencesStore.getState().multiplayerBoardLayout).toBe("split");
    expect(usePreferencesStore.getState().multiplayerSplitLayoutNudgeDismissed).toBe(false);
  });

  it("dismisses the nudge without changing the raw layout", () => {
    const gameState = gameStateFactory.withPlayers(0, 1, 2).priority(0).build();
    storeOverrides.gameState = gameState;
    storeOverrides.waitingFor = gameState.waiting_for;
    usePreferencesStore.setState({
      multiplayerBoardLayout: "focused",
      multiplayerSplitLayoutNudgeDismissed: false,
    });

    renderGamePage();

    act(() => (capturedGameMenuProps?.onDismissMultiplayerSplitLayoutNudge as () => void)());

    expect(usePreferencesStore.getState().multiplayerBoardLayout).toBe("focused");
    expect(usePreferencesStore.getState().multiplayerSplitLayoutNudgeDismissed).toBe(true);
  });

  it.each([
    ["mobile", true, "focused", false],
    ["raw split", false, "split", false],
    ["dismissed focused", false, "focused", true],
  ] as const)(
    "withholds the nudge for a %s cohort",
    (_cohort, isMobile, multiplayerBoardLayout, multiplayerSplitLayoutNudgeDismissed) => {
      const gameState = gameStateFactory.withPlayers(0, 1, 2).priority(0).build();
      storeOverrides.gameState = gameState;
      storeOverrides.waitingFor = gameState.waiting_for;
      mockIsMobile.mockReturnValue(isMobile);
      usePreferencesStore.setState({
        multiplayerBoardLayout,
        multiplayerSplitLayoutNudgeDismissed,
      });

      renderGamePage();

      expect(capturedGameMenuProps?.showMultiplayerSplitLayoutNudge).toBe(false);
      expect(capturedGameMenuProps?.onTryMultiplayerSplitLayout).toBeUndefined();
    },
  );
});

describe("GamePage — toast surface", () => {
  const FALLBACK_NOTICE = "Native engine unavailable — this game is running in-browser.";

  function seedToast(): void {
    mockMultiplayerState.toasts = new Map([
      ["generic", { message: FALLBACK_NOTICE, expiresAt: Date.now() + 5_000, showCountdown: false }],
    ]);
  }

  afterEach(() => {
    mockMultiplayerState.toasts = new Map();
  });

  it("shows a solo game's toast", () => {
    // The native-engine fallback notice is raised in `ai` mode. This surface
    // used to be gated on online mode, so the notice was written to the store
    // and then rendered by nothing at all.
    seedToast();

    renderGamePage("/game/test-game-123?mode=ai");

    expect(screen.getByText(FALLBACK_NOTICE)).toBeInTheDocument();
  });

  it("offers a solo game no Retry, since there is no server to re-dial", () => {
    seedToast();

    renderGamePage("/game/test-game-123?mode=ai");

    // Settings is the reach guard: it proves the toast's button row rendered,
    // so Retry's absence is the omitted prop rather than an unmounted toast.
    expect(screen.getByRole("button", { name: "Settings" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
  });

  it("returns Settings from a transient toast to the persistent game menu", async () => {
    seedToast();
    renderGamePage("/game/test-game-123?mode=ai");
    const gameMenu = screen.getByRole("button", { name: "Game menu" });
    const toastSettings = screen.getByRole("button", { name: "Settings" });

    expect(document.activeElement).toBe(document.body);
    fireEvent.click(toastSettings);

    await closePreferencesAndExpectGameMenuFocus();
    expect(gameMenu).toHaveFocus();
  });
});

describe("GamePage — board settings focus handoff", () => {
  it("returns Change Background from its transient menu to the game menu", async () => {
    renderGamePage();
    fireEvent.contextMenu(screen.getByTestId("game-board-layout"), {
      clientX: 40,
      clientY: 60,
    });

    fireEvent.click(screen.getByRole("menuitem", { name: /Change background/ }));

    await closePreferencesAndExpectGameMenuFocus();
  });
});

describe("GamePage — shared modal return targets", () => {
  it("returns a manually opened zone viewer to its persistent pile", async () => {
    const card = gameObjectFactory
      .withId(71)
      .named("Graveyard Card")
      .ownedBy(0)
      .inGraveyard()
      .build();
    storeOverrides.gameState = gameStateFactory
      .withPlayers({ id: 0, graveyard: [card.id] }, 1)
      .withObjects(card)
      .build();
    renderGamePage();
    const pile = document.querySelector<HTMLButtonElement>(
      '[data-graveyard-pile="0"]',
    );
    expect(pile).not.toBeNull();
    screen.getByRole("button", { name: "Game menu" }).focus();
    fireEvent.click(pile!);

    await closeDialogAndExpectFocus(
      await screen.findByRole("dialog", { name: /Graveyard/ }),
      pile!,
    );
  });

  it("falls back to the game menu when the final card leaves an open zone", async () => {
    const card = gameObjectFactory
      .withId(72)
      .named("Last Graveyard Card")
      .ownedBy(0)
      .inGraveyard()
      .build();
    storeOverrides.gameState = gameStateFactory
      .withPlayers({ id: 0, graveyard: [card.id] }, 1)
      .withObjects(card)
      .build();
    const view = renderGamePage();
    const pile = document.querySelector<HTMLButtonElement>(
      '[data-graveyard-pile="0"]',
    );
    expect(pile).not.toBeNull();
    fireEvent.click(pile!);
    const dialog = await screen.findByRole("dialog", { name: /Graveyard/ });
    await waitFor(() => expect(dialog).toHaveFocus());

    storeOverrides.gameState = gameStateFactory.withPlayers(0, 1).build();
    view.rerender(gamePageTree());
    expect(pile).not.toBeInTheDocument();

    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
    expect(screen.getByRole("button", { name: "Game menu" })).toHaveFocus();
  });

  it("falls back when a visible library launcher remains mounted but disables", async () => {
    const visibleTop = gameObjectFactory
      .withId(73)
      .named("Visible Library Top")
      .ownedBy(0)
      .params({
        zone: "Library",
        display_visible_to_viewer: true,
        entered_battlefield_turn: null,
      })
      .build();
    storeOverrides.gameState = gameStateFactory
      .withPlayers({ id: 0, library: [visibleTop.id] }, 1)
      .withObjects(visibleTop)
      .build();
    const view = renderGamePage();
    const pile = document.querySelector<HTMLButtonElement>(
      '[data-library-pile="0"] > button',
    );
    expect(pile).toBeEnabled();
    fireEvent.click(pile!);
    const dialog = await screen.findByRole("dialog", { name: /Library/ });
    await waitFor(() => expect(dialog).toHaveFocus());

    const hiddenTop = { ...visibleTop, display_visible_to_viewer: false };
    storeOverrides.gameState = gameStateFactory
      .withPlayers({ id: 0, library: [hiddenTop.id] }, 1)
      .withObjects(hiddenTop)
      .build();
    view.rerender(gamePageTree());
    expect(pile).toBeInTheDocument();
    expect(pile).toBeDisabled();

    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
    expect(screen.getByRole("button", { name: "Game menu" })).toHaveFocus();
  });

  it("returns a board-context card report to the persistent game menu", async () => {
    storeOverrides.gameState = gameStateFactory.build();
    renderGamePage();
    fireEvent.contextMenu(screen.getByTestId("game-board-layout"), {
      clientX: 40,
      clientY: 60,
    });

    fireEvent.click(screen.getByRole("menuitem", { name: /Report a card/ }));

    await closeDialogAndExpectGameMenuFocus(await screen.findByRole("dialog"));
  });

  it("closes the higher debug panel before opening its library viewer", async () => {
    renderGamePage();
    act(() => useUiStore.getState().openSandboxTools());
    expect(useUiStore.getState().debugPanelOpen).toBe(true);
    expect(useUiStore.getState().debugPanelTab).toBe("actions");
    expect(await screen.findByText("Debug Panel")).toBeInTheDocument();
    expect(await screen.findByText("Debug Actions")).toBeInTheDocument();
    const accordionToggle = await screen.findByRole("button", {
      name: /Browse Library/,
    });
    fireEvent.click(accordionToggle);
    const browseButtons = screen.getAllByRole("button", {
      name: /Browse Library/,
    });
    fireEvent.click(browseButtons[browseButtons.length - 1]);

    expect(screen.queryByText("Debug Panel")).not.toBeInTheDocument();

    await closeDialogAndExpectGameMenuFocus(await screen.findByRole("dialog"));
  });
});

/**
 * A refused takeback must not destroy a desktop-solo session.
 *
 * Desktop solo-vs-AI is served by the `phase-server` sidecar over a
 * WebSocket and arrives here as `mode=ai`, so `isOnlineMode` is false and
 * `case "error"` sets a TERMINAL `reconnectState`. The server used to answer
 * every refused takeback with `ServerMessage::error`, so a second takeback
 * click — reachable because an approved takeback clears the history — tore
 * down the adapter and stranded the game behind a "Connection lost" banner.
 * The refusal now travels `ServerMessage::ActionRejected`, which the adapter
 * surfaces as `requestRejected`.
 */
describe("GamePage — a refused request is survivable", () => {
  it("toasts a refused takeback without entering the terminal reconnect state", () => {
    renderGamePage("/game/test-game-123?mode=ai");

    act(() => {
      capturedOnWsEvent?.({
        type: "requestRejected",
        reason: "There is no previous action of yours to take back",
      });
    });

    // Delivery witness. Without this the two negatives below would be
    // satisfied by an event that was never delivered at all.
    expect(mockMultiplayerState.showToast).toHaveBeenCalledWith(
      "There is no previous action of yours to take back",
    );
    // The session is intact: no terminal banner, no Return-to-Menu escape
    // hatch. These are the assertions that fail if the server reverts to
    // `ServerMessage::error`, or if GameProvider forwards this event into
    // the teardown branch instead of its own.
    expect(screen.queryByText("Connection lost")).toBeNull();
    expect(screen.queryByRole("button", { name: "Return to Menu" })).toBeNull();
  });

  it("still tears down on a genuine transport error from the same fixture", () => {
    // Reach guard for the negatives above: the SAME fixture and the SAME
    // event channel can and does produce the terminal state, so their
    // absence in the test above is the classification change and not an
    // inert harness.
    renderGamePage("/game/test-game-123?mode=ai");

    act(() => {
      capturedOnWsEvent?.({ type: "error", message: "WebSocket connection failed" });
    });

    expect(screen.getByText("Connection lost")).toBeInTheDocument();
  });

  it("survives a second refusal, the click that used to be reachable", () => {
    // An approved takeback clears `takeback_history`, so takeback-twice
    // lands on "there is no previous action of yours to take back". This is
    // the exact reachability that made the destructive path a routine
    // second click rather than an edge case.
    renderGamePage("/game/test-game-123?mode=ai");

    act(() => {
      capturedOnWsEvent?.({ type: "requestRejected", reason: "first refusal" });
      capturedOnWsEvent?.({
        type: "requestRejected",
        reason: "There is no previous action of yours to take back",
      });
    });

    expect(mockMultiplayerState.showToast).toHaveBeenCalledTimes(2);
    expect(screen.queryByText("Connection lost")).toBeNull();
  });

  it("routes an adjacent refusal reason down the same non-terminal path", () => {
    // The fix is per-CHANNEL, not per-message: "only human players may
    // request a takeback" and "a takeback request is already pending" travel
    // the same wire message and must behave identically.
    renderGamePage("/game/test-game-123?mode=ai");

    act(() => {
      capturedOnWsEvent?.({
        type: "requestRejected",
        reason: "Only human players may request a takeback",
      });
    });

    expect(mockMultiplayerState.showToast).toHaveBeenCalledWith(
      "Only human players may request a takeback",
    );
    expect(screen.queryByText("Connection lost")).toBeNull();
  });

  it("does not set the terminal state for an online refusal either", () => {
    // Behaviour-preserving for `online`: it toasted before (via `case
    // "error"`, where `isOnlineMode` suppressed the terminal branch) and
    // toasts now via `case "requestRejected"`.
    //
    // `?mode=host` — the URL spelling. `?mode=online` is NOT an inhabitant of
    // the raw-mode set and falls through to `local`, for which GamePage passes
    // no `onWsEvent` at all, so the whole test would go silently inert.
    renderGamePage("/game/test-game-123?mode=host");

    act(() => {
      capturedOnWsEvent?.({ type: "requestRejected", reason: "refused" });
    });

    // Reach guard: `capturedOnWsEvent` must actually exist for this mode.
    expect(capturedOnWsEvent).toBeDefined();
    expect(mockMultiplayerState.showToast).toHaveBeenCalledWith("refused");
    expect(screen.queryByText("Connection lost")).toBeNull();
  });
});

/**
 * Takeback is offered by TRANSPORT, not by mode.
 *
 * `onRequestTakeback` used to be gated on `isOnlineMode`, which is URL-derived
 * and can never see `native-ai` — desktop solo arrives as `mode=ai`. So the one
 * mode with a server-authoritative takeback and no client-side undo was the one
 * mode that never got the button, while spectators (whom `request_takeback`
 * rejects server-side) did.
 */
describe("GamePage — takeback is a transport capability", () => {
  class FakeWebSocketAdapter extends WebSocketAdapter {
    // Real superclass construction, not a stub — `super(...)` runs. It is safe
    // to call here because the constructor only assigns fields and
    // `maxReconnectAttempts`; it opens no socket. The subclass exists solely
    // to supply throwaway arguments, since all this fixture needs is
    // `instanceof` to hold — the exact predicate GamePage and
    // `handleRequestTakeback` both use.
    constructor() {
      super("ws://test/ws", "host", { main_deck: [], sideboard: [] });
    }
  }

  it("offers takeback for a WebSocketAdapter in desktop-solo mode", () => {
    storeOverrides.adapter = new FakeWebSocketAdapter();

    renderGamePage("/game/test-game-123?mode=ai");

    // `mode=ai` is exactly how desktop solo-vs-AI reaches this page, and it is
    // the case the old `isOnlineMode` gate got wrong.
    expect(capturedGameMenuProps?.isOnlineMode).toBe(false);
    expect(capturedGameMenuProps?.onRequestTakeback).toBeTypeOf("function");
  });

  it("withholds takeback when the adapter cannot send it", () => {
    // Paired negative: `null` stands for any non-WebSocket adapter (the WASM
    // engine of browser solo, which has real local undo instead). Proves the
    // gate is a transport check and not "always on".
    storeOverrides.adapter = null;

    renderGamePage("/game/test-game-123?mode=ai");

    // Reach guard: GameMenu really was rendered, so the undefined prop is the
    // gate rather than an unmounted menu.
    expect(capturedGameMenuProps).toBeDefined();
    expect(capturedGameMenuProps?.onRequestTakeback).toBeUndefined();
  });

  it("withholds takeback from spectators even on a WebSocketAdapter", () => {
    // This removes a currently-VISIBLE control from a live mode, so it gets
    // its own case rather than riding on the justification that
    // `request_takeback` rejects spectators server-side anyway.
    storeOverrides.adapter = new FakeWebSocketAdapter();

    renderGamePage("/game/test-game-123?mode=spectate");

    // Reach guard: the transport half of the gate is satisfied, so the
    // undefined prop can only come from the spectate half.
    expect(capturedGameMenuProps?.isOnlineMode).toBe(true);
    expect(capturedGameMenuProps?.onRequestTakeback).toBeUndefined();
  });

  it("keeps offering takeback to online play", () => {
    storeOverrides.adapter = new FakeWebSocketAdapter();

    renderGamePage("/game/test-game-123?mode=host");

    expect(capturedGameMenuProps?.onRequestTakeback).toBeTypeOf("function");
  });

  // F5 (M11 half). The label axis must come from the AUTHORITATIVE store mode,
  // not the URL-derived one: desktop solo arrives as `?mode=ai` and the store
  // says `native-ai`, so a URL-derived answer cannot tell it apart from a
  // browser AI game — and both must read as a solo undo, while an online table
  // keeps the "request" wording.
  it("addresses the rollback to the player alone in desktop solo", () => {
    storeOverrides.adapter = new FakeWebSocketAdapter();
    storeOverrides.gameMode = "native-ai";

    renderGamePage("/game/test-game-123?mode=ai");

    expect(capturedGameMenuProps?.takebackAudience).toBe("solo");
  });

  it("addresses the rollback to the table in online play", () => {
    // Paired positive. `?mode=host` with the store reporting `online` is the
    // shape that must NOT change wording.
    storeOverrides.adapter = new FakeWebSocketAdapter();
    storeOverrides.gameMode = "online";

    renderGamePage("/game/test-game-123?mode=host");

    expect(capturedGameMenuProps?.takebackAudience).toBe("table");
  });
});

/**
 * F5 — desktop solo-vs-AI sandbox characterization.
 *
 * HONESTLY LABELLED: this PASSES at BASE_SHA. The user's "sandbox mode, no
 * banner" ask is already satisfied, and this exists so a later change cannot
 * silently undo it. `mode` is URL-derived and structurally cannot be
 * `native-ai`; desktop solo arrives as `rawMode === "ai"`, which already
 * satisfies `showSandboxTools`. The SANDBOX badge is gated separately on
 * `format_config.allow_debug_actions`, which the server's `SingleUser` branch
 * deliberately leaves false.
 */
describe("GamePage — desktop solo sandbox tools without the banner", () => {
  it("enables sandbox tools while the SANDBOX badge stays hidden", () => {
    storeOverrides.gameMode = "native-ai";
    storeOverrides.gameState = gameStateFactory.build({
      format_config: { allow_debug_actions: false } as unknown as FormatConfig,
    });

    renderGamePage("/game/test-game-123?mode=ai");

    expect(capturedGameMenuProps?.showSandboxTools).toBe(true);
    expect(screen.queryByRole("status", { name: "Sandbox mode banner" })).toBeNull();
  });

  it("shows the SANDBOX badge once the game really is sandbox-flagged", () => {
    // Non-vacuity guard for the negative above: flipping the one flag the
    // badge is gated on must make it appear, proving the assertion measures
    // the gate rather than an unrendered subtree.
    storeOverrides.gameMode = "ai";
    storeOverrides.gameState = gameStateFactory.build({
      format_config: { allow_debug_actions: true } as unknown as FormatConfig,
    });

    renderGamePage("/game/test-game-123?mode=ai");

    expect(screen.getByRole("status", { name: "Sandbox mode banner" })).toBeInTheDocument();
  });
});

describe("GamePage — bound whole-match concession", () => {
  class FakeWebSocketAdapter extends WebSocketAdapter {
    sendMatchConcede = vi.fn();

    constructor() {
      super("ws://test/ws", "host", { main_deck: [], sideboard: [] });
    }
  }

  it("offers and invokes the WebSocket whole-match capability for a Bo3", () => {
    const sendMatchConcede = vi.fn();
    const adapter = new FakeWebSocketAdapter();
    adapter.sendMatchConcede = sendMatchConcede;
    storeOverrides.adapter = adapter;
    storeOverrides.gameState = {
      match_config: { match_type: "Bo3" },
      waiting_for: {
        type: "BetweenGamesChoosePlayDraw",
        data: {
          player: 0,
          game_number: 2,
          score: { p0_wins: 1, p1_wins: 0, draws: 0 },
        },
      },
      players: [],
      objects: {},
      battlefield: [],
      stack: [],
      exile: [],
    };
    storeOverrides.waitingFor = {
      type: "BetweenGamesChoosePlayDraw",
      data: {
        player: 0,
        game_number: 2,
        score: { p0_wins: 1, p1_wins: 0, draws: 0 },
      },
    };

    renderGamePage("/game/test-game-123?mode=host");
    act(() => (capturedGameMenuProps?.onConcede as () => void)());

    const matchAction = capturedConcedeDialogProps?.matchAction as
      | { onConfirm: () => void }
      | undefined;
    expect(matchAction?.onConfirm).toBeTypeOf("function");
    act(() => matchAction?.onConfirm());
    expect(sendMatchConcede).toHaveBeenCalledOnce();
  });
});

/**
 * A rematch starts a NEW game id from the game-over screen. The URL it carries
 * over names the format but holds none of its edited knobs, and the saved
 * active-game record is keyed to the id being left — so the config has to be
 * handed over explicitly or a custom starting life silently reverts to the
 * format default.
 */
describe("GamePage — rematch preserves the format the game was played with", () => {
  // The rematch button is gated on `onAnimationComplete` of the game-over
  // title's spring, which never settles under happy-dom's rAF. `skipAnimations`
  // is framer-motion's own switch for exactly this — animations jump to their
  // end state and fire their completion callbacks — so the gate is satisfied
  // the way the library intends rather than by mocking `motion` away. Scoped
  // to this block so the suite's other renders keep real motion behaviour.
  beforeEach(() => {
    MotionGlobalConfig.skipAnimations = true;
  });
  afterEach(() => {
    MotionGlobalConfig.skipAnimations = false;
  });

  it("hands the engine's own format config to the new game", async () => {
    const user = userEvent.setup();
    // A Commander game played at 25 life rather than the format's 40.
    storeOverrides.gameState = gameStateFactory.withPlayers(0, 1).build({
      format_config: buildCommanderFormatConfig({ starting_life: 25 }),
    });
    storeOverrides.waitingFor = { type: "GameOver", data: { winner: 0 } };

    renderGamePage("/game/old-game-id?mode=ai&format=Commander");

    await user.click(await screen.findByRole("button", { name: "Rematch" }));

    // Asserted at the engine boundary: this is the config GameProvider would
    // build the rematch with, not merely what was stashed on the navigation.
    // `FORMAT_DEFAULTS` is a Proxy in this suite, so a lost hand-over surfaces
    // as `undefined` here rather than as the real 40.
    expect(capturedFormatConfig?.starting_life).toBe(25);
  });
});

describe("GamePage — join origin", () => {
  const ORIGIN = "wss://play.example.com/ws";

  it("passes the route's server to GameProvider and carries it through deck rejection", async () => {
    renderGamePage(
      `/game/g1?mode=join&code=ABC123&server=${encodeURIComponent(ORIGIN)}`,
    );

    expect(capturedServerUrl).toBe(ORIGIN);

    act(() => {
      capturedOnWsEvent?.({ type: "deckRejected", reason: "bad deck" });
    });

    const stub = await screen.findByTestId("multiplayer-stub");
    expect(JSON.parse(stub.textContent ?? "null")).toEqual({
      deckRejected: true,
      reason: "bad deck",
      joinCode: "ABC123",
      server: ORIGIN,
    });
  });

  it("carries no server when the route had none", async () => {
    renderGamePage("/game/g1?mode=join&code=ABC123");

    expect(capturedServerUrl).toBeUndefined();

    act(() => {
      capturedOnWsEvent?.({ type: "deckRejected", reason: "bad deck" });
    });

    const stub = await screen.findByTestId("multiplayer-stub");
    // Paired with the case above: the field is absent, not stale.
    expect(JSON.parse(stub.textContent ?? "null")).toEqual({
      deckRejected: true,
      reason: "bad deck",
      joinCode: "ABC123",
    });
  });
});
