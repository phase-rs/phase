import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineAdapter, GameAction, GameEvent, GameState, SubmitResult } from "../../../adapter/types.ts";
import { AdapterError, AdapterErrorCode, nextSnapshotSeq } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { useKeyboardShortcuts } from "../../../hooks/useKeyboardShortcuts.ts";
import { buildEngineAdapterMock } from "../../../test/factories/engineAdapterFactory.ts";
import {
  buildManaPaymentWaitingFor,
  buildFormatConfig,
  buildLegalActionsResult,
  gameStateFactory,
} from "../../../test/factories/gameStateFactory.ts";
import { abandonPendingDispatches } from "../../../game/dispatch.ts";
import { SandboxLifeCorrection } from "../SandboxLifeCorrection.tsx";

interface Harness {
  adapter: EngineAdapter;
  initialState: GameState;
  applySetLife: (action: GameAction, actor: number) => SubmitResult;
  setEngineLife: (targetPlayerId: number, life: number) => void;
  currentEngineState: () => GameState;
}

function makeSandboxGameState(): GameState {
  const state = gameStateFactory
    .withPlayers({ id: 0, life: 20 }, { id: 1, life: 18 })
    .build();
  return {
    ...state,
    active_player: 1,
    turn_decision_controller: 0,
    format_config: buildFormatConfig({ ...state.format_config, allow_debug_actions: false }),
    debug_permitted: [0],
    debug_mode: true,
  };
}

function makeHarness(gameId = "sandbox-life-correction-test"): Harness {
  // Match the real session-boundary cleanup: an in-flight old dispatch must
  // not commit its snapshot after a replacement game is installed.
  abandonPendingDispatches();
  const initialState = makeSandboxGameState();
  let engineState = initialState;

  const applySetLife = (action: GameAction, actor: number): SubmitResult => {
    if (action.type !== "Debug" || action.data.type !== "SetLife") {
      return { events: [] };
    }

    const { player_id: targetPlayerId, life } = action.data.data;
    if (!engineState.players.some((player) => player.id === targetPlayerId)) return { events: [] };
    engineState = {
      ...engineState,
      players: engineState.players.map((player) =>
        player.id === targetPlayerId ? { ...player, life } : player,
      ),
    };

    const events: GameEvent[] = [
      {
        type: "DebugActionUsed",
        data: { player_id: actor, description: `SetLife (Player ${targetPlayerId + 1} → ${life})` },
      },
    ];
    return { events };
  };

  const adapter = buildEngineAdapterMock(initialState, {
    submitAction: vi.fn(async (action: GameAction, actor: number) => applySetLife(action, actor)),
    getState: vi.fn(async () => engineState),
    getLegalActions: vi.fn(async () => buildLegalActionsResult()),
  });

  const nextGeneration = useGameStore.getState().gameSessionGeneration + 1;
  act(() => {
    useMultiplayerStore.setState({
      activePlayerId: 0,
      isSpectator: false,
      playerNames: new Map([[1, "Ada"]]),
    });
    useGameStore.setState({
      gameId,
      gameMode: "ai",
      gameState: initialState,
      adapter,
      gameSessionGeneration: nextGeneration,
      engineCommitEpoch: 0,
      lastCommittedSeq: 0,
      waitingFor: initialState.waiting_for,
      events: [],
      eventHistory: [],
      legalActions: [],
    });
  });

  return {
    adapter,
    initialState,
    applySetLife,
    setEngineLife: (targetPlayerId, life) => {
      engineState = {
        ...engineState,
        players: engineState.players.map((player) =>
          player.id === targetPlayerId ? { ...player, life } : player,
        ),
      };
    },
    currentEngineState: () => engineState,
  };
}

function GameKeyboardShortcutsHarness() {
  useKeyboardShortcuts();
  return null;
}

function openPanel() {
  fireEvent.click(screen.getByRole("button", { name: "Life correction" }));
  return screen.getByRole("dialog", { name: "Sandbox life correction" });
}

function setNewLife(value: string) {
  fireEvent.change(screen.getByRole("spinbutton", { name: "New life total" }), {
    target: { value },
  });
}

function positionedRect(left: number, top: number, width = 120, height = 44): DOMRect {
  return {
    x: left,
    y: top,
    left,
    top,
    right: left + width,
    bottom: top + height,
    width,
    height,
    toJSON: () => ({}),
  } as DOMRect;
}

describe("SandboxLifeCorrection", () => {
  beforeEach(() => {
    vi.stubEnv("DEV", true);
    vi.stubEnv("VITE_PHASE_SANDBOX", "1");
    usePreferencesStore.getState().setAnimationSpeedMultiplier(0);
    abandonPendingDispatches();
  });

  afterEach(() => {
    cleanup();
    abandonPendingDispatches();
    useMultiplayerStore.setState({ activePlayerId: 0, isSpectator: false, playerNames: new Map() });
    useGameStore.setState({
      gameId: null,
      gameMode: null,
      gameState: null,
      adapter: null,
      waitingFor: null,
      events: [],
      eventHistory: [],
      engineCommitEpoch: 0,
      lastCommittedSeq: 0,
    });
    usePreferencesStore.getState().setAnimationSpeedMultiplier(1);
    vi.unstubAllEnvs();
  });

  it("requires both a development build and the explicit sandbox flag", () => {
    makeHarness();
    vi.stubEnv("VITE_PHASE_SANDBOX", "");
    const first = render(<SandboxLifeCorrection />);
    expect(screen.queryByRole("button", { name: "Life correction" })).toBeNull();
    first.unmount();

    vi.stubEnv("VITE_PHASE_SANDBOX", "1");
    vi.stubEnv("DEV", false);
    render(<SandboxLifeCorrection />);
    expect(screen.queryByRole("button", { name: "Life correction" })).toBeNull();
  });

  it("registers no overlay observer or geometry measurement while ineligible", () => {
    makeHarness();
    vi.stubEnv("VITE_PHASE_SANDBOX", "");
    const observeSpy = vi.spyOn(MutationObserver.prototype, "observe");
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect");

    try {
      render(<SandboxLifeCorrection />);

      expect(observeSpy).not.toHaveBeenCalled();
      expect(rectSpy).not.toHaveBeenCalled();
      expect(screen.queryByRole("button", { name: "Life correction" })).toBeNull();
    } finally {
      observeSpy.mockRestore();
      rectSpy.mockRestore();
    }
  });

  it("starts and cleans up overlay and measurement listeners with eligibility", async () => {
    const harness = makeHarness();
    act(() => useGameStore.setState({ gameState: null }));
    const observerSpy = vi.spyOn(MutationObserver.prototype, "observe");
    const disconnectSpy = vi.spyOn(MutationObserver.prototype, "disconnect");
    const addListenerSpy = vi.spyOn(window, "addEventListener");
    const removeListenerSpy = vi.spyOn(window, "removeEventListener");

    try {
      render(<SandboxLifeCorrection />);
      expect(observerSpy).not.toHaveBeenCalled();
      expect(addListenerSpy.mock.calls.some(([type]) => type === "resize" || type === "scroll")).toBe(false);

      act(() => useGameStore.setState({ gameState: harness.initialState }));
      await waitFor(() => {
        expect(observerSpy).toHaveBeenCalled();
        expect(addListenerSpy.mock.calls.some(([type]) => type === "resize")).toBe(true);
        expect(addListenerSpy.mock.calls.some(([type]) => type === "scroll")).toBe(true);
      });

      const addedMeasurements = addListenerSpy.mock.calls.filter(([type]) => type === "resize" || type === "scroll");
      act(() => {
        useGameStore.setState({ gameState: { ...harness.initialState, debug_mode: false } });
      });
      await waitFor(() => {
        expect(disconnectSpy).toHaveBeenCalled();
        expect(removeListenerSpy.mock.calls.filter(([type]) => type === "resize" || type === "scroll")).toEqual(
          expect.arrayContaining(addedMeasurements),
        );
      });
    } finally {
      observerSpy.mockRestore();
      disconnectSpy.mockRestore();
      addListenerSpy.mockRestore();
      removeListenerSpy.mockRestore();
    }
  });

  it("hides in a remote game even when this local sandbox flag is enabled", () => {
    makeHarness();
    act(() => useGameStore.setState({ gameMode: "online" }));

    render(<SandboxLifeCorrection />);

    expect(screen.queryByRole("button", { name: "Life correction" })).toBeNull();
  });

  it("requires engine debug mode even when the format flag is off", () => {
    const harness = makeHarness();
    expect(harness.initialState.format_config?.allow_debug_actions).toBe(false);
    act(() => {
      useGameStore.setState({ gameState: { ...harness.initialState, debug_mode: false } });
    });

    render(<SandboxLifeCorrection />);

    expect(screen.queryByRole("button", { name: "Life correction" })).toBeNull();
  });

  it("hides when the local seat is not engine-permitted to submit debug actions", () => {
    const harness = makeHarness();
    act(() => {
      useGameStore.setState({ gameState: { ...harness.initialState, debug_permitted: [1] } });
    });

    render(<SandboxLifeCorrection />);

    expect(screen.queryByRole("button", { name: "Life correction" })).toBeNull();
  });

  it("measures the trigger when sandbox eligibility becomes true after mount", async () => {
    const harness = makeHarness();
    act(() => useGameStore.setState({ gameState: null }));
    const originalGetBoundingClientRect = HTMLElement.prototype.getBoundingClientRect;
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      return this.hasAttribute("data-sandbox-life-correction-anchor")
        ? positionedRect(72, 118)
        : originalGetBoundingClientRect.call(this);
    });

    try {
      render(<SandboxLifeCorrection />);
      expect(screen.queryByRole("button", { name: "Life correction" })).toBeNull();

      act(() => useGameStore.setState({ gameState: harness.initialState }));

      const trigger = await screen.findByRole("button", { name: "Life correction" });
      expect(trigger.style.left).toBe("72px");
      expect(trigger.style.top).toBe("118px");
    } finally {
      rectSpy.mockRestore();
    }
  });

  it("keeps the body portal aligned while the draggable HUD transform moves", async () => {
    makeHarness();
    const { container } = render(
      <div data-flex-zone="playerHud">
        <SandboxLifeCorrection />
      </div>,
    );
    const anchor = container.querySelector<HTMLElement>("[data-sandbox-life-correction-anchor]");
    const flexWidget = container.querySelector<HTMLElement>('[data-flex-zone="playerHud"]');
    expect(anchor).not.toBeNull();
    expect(flexWidget).not.toBeNull();

    let left = 32;
    vi.spyOn(anchor!, "getBoundingClientRect").mockImplementation(() => positionedRect(left, 64));
    const trigger = screen.getByRole("button", { name: "Life correction" });
    left = 172;
    await act(async () => {
      flexWidget!.setAttribute("style", "transform: translate(140px, 0px)");
      await new Promise((resolve) => setTimeout(resolve, 0));
    });

    expect(trigger.style.left).toBe("172px");
    expect(trigger.style.top).toBe("64px");
  });

  it("suppresses itself for blocking overlays and engine recovery, but ignores coachmarks", async () => {
    makeHarness();
    render(
      <>
        <SandboxLifeCorrection />
        <div data-coachmark="sandbox-walkthrough" className="fixed z-[120]">
          Coachmark
        </div>
      </>,
    );

    expect(screen.getByRole("button", { name: "Life correction" })).toBeInTheDocument();
    expect(screen.getByText("Coachmark")).toBeInTheDocument();
    openPanel();

    const blockingDialog = document.createElement("div");
    blockingDialog.setAttribute("role", "dialog");
    blockingDialog.setAttribute("aria-modal", "true");
    const blockingAction = document.createElement("button");
    blockingAction.textContent = "Continue in modal";
    blockingDialog.appendChild(blockingAction);
    document.body.appendChild(blockingDialog);
    blockingAction.focus();
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Life correction" })).toBeNull();
      expect(screen.queryByRole("dialog", { name: "Sandbox life correction" })).toBeNull();
    });
    expect(blockingAction).toHaveFocus();
    blockingDialog.remove();
    await waitFor(() => expect(screen.getByRole("button", { name: "Life correction" })).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: "Apply correction" })).toBeNull();

    const recoveryModal = document.createElement("div");
    recoveryModal.setAttribute("data-engine-lost-reason", "STATE_LOST");
    document.body.appendChild(recoveryModal);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Life correction" })).toBeNull());
    recoveryModal.remove();
    await waitFor(() => expect(screen.getByRole("button", { name: "Life correction" })).toBeInTheDocument());
  });

  it("suppresses itself under a full-screen blocking layer without modal ARIA", async () => {
    makeHarness();
    render(<SandboxLifeCorrection />);
    expect(screen.getByRole("button", { name: "Life correction" })).toBeInTheDocument();

    const overlay = document.createElement("div");
    overlay.className = "fixed inset-0 z-50";
    overlay.style.position = "fixed";
    overlay.style.inset = "0";
    overlay.style.zIndex = "50";
    overlay.getBoundingClientRect = () => positionedRect(0, 0, window.innerWidth, window.innerHeight);
    document.body.appendChild(overlay);

    await waitFor(() => expect(screen.queryByRole("button", { name: "Life correction" })).toBeNull());
    overlay.remove();
    await waitFor(() => expect(screen.getByRole("button", { name: "Life correction" })).toBeInTheDocument());
  });

  it("submits SetLife through useGameDispatch and confirms the committed engine snapshot", async () => {
    const harness = makeHarness();
    expect(harness.initialState.format_config?.allow_debug_actions).toBe(false);
    expect(harness.initialState.debug_mode).toBe(true);
    render(<SandboxLifeCorrection />);
    const trigger = screen.getByRole("button", { name: "Life correction" });
    expect(trigger).toHaveClass("min-h-11", "z-[130]");
    expect(trigger.parentElement).toBe(document.body);
    const dialog = openPanel();
    expect(dialog.parentElement).toBe(document.body);
    const targetSelect = screen.getByRole("combobox", { name: "Player" });
    expect(targetSelect).toHaveClass("min-h-11");
    expect(screen.getByRole("option", { name: "You (Player 1)" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Ada (Player 2)" })).toBeInTheDocument();
    fireEvent.change(targetSelect, { target: { value: "1" } });
    expect(screen.getByRole("spinbutton", { name: "New life total" })).toHaveValue(18);
    expect(screen.getByRole("spinbutton", { name: "New life total" })).toHaveClass("min-h-11");
    setNewLife("17");

    fireEvent.click(screen.getByRole("button", { name: "Apply correction" }));

    expect(await screen.findByText("Current committed life for Ada (Player 2): 17.")).toBeInTheDocument();
    expect(harness.adapter.submitAction).toHaveBeenCalledWith(
      { type: "Debug", data: { type: "SetLife", data: { player_id: 1, life: 17 } } },
      0,
    );
    expect(useGameStore.getState().engineCommitEpoch).toBe(1);
    expect(useGameStore.getState().gameState?.players[1]?.life).toBe(17);
    expect(useGameStore.getState().events).toEqual([
      {
        type: "DebugActionUsed",
        data: { player_id: 0, description: "SetLife (Player 2 → 17)" },
      },
    ]);
    expect(useGameStore.getState().events.some((event) => event.type === "LifeChanged")).toBe(false);
    expect(harness.currentEngineState().players[1]?.life).toBe(17);
    harness.setEngineLife(1, 16);
    const previousSnapshot = useGameStore.getState().gameState!;
    const laterSnapshot: GameState = {
      ...previousSnapshot,
      players: previousSnapshot.players.map((player) =>
        player.id === 1 ? { ...player, life: 16 } : player,
      ),
    };
    act(() => {
      useGameStore.getState().commitEngineSnapshot(
        { state: laterSnapshot, legalResult: buildLegalActionsResult(), seq: nextSnapshotSeq() },
        { events: [], logEntries: [] },
      );
    });
    expect(screen.getByText("Current committed life for Ada (Player 2): 16.")).toBeInTheDocument();
    expect(screen.queryByText("Current committed life for Ada (Player 2): 17.")).toBeNull();
    const closeButton = screen.getByRole("button", { name: "Close" });
    expect(closeButton).toHaveClass("min-h-11", "min-w-11");
    expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull();
    fireEvent.click(closeButton);
    expect(screen.queryByRole("dialog", { name: "Sandbox life correction" })).toBeNull();
  });

  it("sends nothing when cancelled before submission and starts fresh after reopening", () => {
    const harness = makeHarness();
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("23");

    const cancelButton = screen.getByRole("button", { name: "Cancel" });
    expect(cancelButton).toHaveClass("min-h-11", "min-w-11");
    fireEvent.click(cancelButton);
    expect(harness.adapter.submitAction).not.toHaveBeenCalled();

    openPanel();
    expect(screen.getByRole("spinbutton", { name: "New life total" })).toHaveValue(20);
  });

  it("returns focus to the trigger after cancel and reopens from the keyboard", async () => {
    makeHarness();
    const user = userEvent.setup();
    render(<SandboxLifeCorrection />);
    const trigger = screen.getByRole("button", { name: "Life correction" });

    await user.click(trigger);
    expect(screen.getByRole("dialog", { name: "Sandbox life correction" })).toHaveFocus();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(trigger).toHaveFocus();

    await user.keyboard("{Enter}");
    expect(screen.getByRole("dialog", { name: "Sandbox life correction" })).toHaveFocus();
  });

  it("moves focus to the result status after Apply", async () => {
    const user = userEvent.setup();
    const harness = makeHarness();
    render(<SandboxLifeCorrection />);
    await user.click(screen.getByRole("button", { name: "Life correction" }));
    const lifeInput = screen.getByRole("spinbutton", { name: "New life total" });
    await user.clear(lifeInput);
    await user.type(lifeInput, "21");
    await user.click(screen.getByRole("button", { name: "Apply correction" }));

    const status = await screen.findByRole("status");
    expect(status).toHaveTextContent("Current committed life for You (Player 1): 21.");
    expect(status).toHaveFocus();
    expect(useGameStore.getState().engineCommitEpoch).toBe(1);
    expect(harness.adapter.submitAction).toHaveBeenCalledTimes(1);
  });

  it("moves focus to failure feedback after Apply is rejected", async () => {
    const user = userEvent.setup();
    const harness = makeHarness();
    vi.mocked(harness.adapter.submitAction).mockRejectedValueOnce(
      new AdapterError(AdapterErrorCode.ACTION_REJECTED, "Debug permission denied", false),
    );
    render(<SandboxLifeCorrection />);
    await user.click(screen.getByRole("button", { name: "Life correction" }));
    const lifeInput = screen.getByRole("spinbutton", { name: "New life total" });
    await user.clear(lifeInput);
    await user.type(lifeInput, "21");
    await user.click(screen.getByRole("button", { name: "Apply correction" }));

    expect(await screen.findByText("The correction could not be submitted.")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveFocus();
    expect(useGameStore.getState().gameState?.players[0]?.life).toBe(20);
    expect(harness.adapter.submitAction).toHaveBeenCalledTimes(1);
  });

  it("keeps all trigger and panel keydowns local while preserving native activation and focus navigation", async () => {
    const harness = makeHarness();
    expect(useGameStore.getState().waitingFor?.type).toBe("Priority");
    const user = userEvent.setup();
    render(
      <>
        <GameKeyboardShortcutsHarness />
        <SandboxLifeCorrection />
      </>,
    );

    const trigger = screen.getByRole("button", { name: "Life correction" });
    trigger.focus();
    await user.keyboard("{Enter}");
    const firstDialog = await screen.findByRole("dialog", { name: "Sandbox life correction" });
    expect(firstDialog).toHaveFocus();
    expect(harness.adapter.submitAction).not.toHaveBeenCalled();

    act(() => useUiStore.setState({ selectedCardIds: [10] }));
    const stateBeforeEscape = useGameStore.getState().gameState;
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "Sandbox life correction" })).toBeNull();
    expect(trigger).toHaveFocus();
    expect(useUiStore.getState().selectedCardIds).toEqual([10]);
    expect(useGameStore.getState().gameState).toBe(stateBeforeEscape);
    expect(harness.adapter.submitAction).not.toHaveBeenCalled();

    await user.keyboard("{Enter}");
    expect(await screen.findByRole("dialog", { name: "Sandbox life correction" })).toHaveFocus();
    await user.tab();
    const cancel = screen.getByRole("button", { name: "Cancel" });
    expect(cancel).toHaveFocus();
    await user.keyboard(" ");
    expect(screen.queryByRole("dialog", { name: "Sandbox life correction" })).toBeNull();
    expect(trigger).toHaveFocus();
    expect(harness.adapter.submitAction).not.toHaveBeenCalled();

    await user.keyboard("{Enter}");
    expect(await screen.findByRole("dialog", { name: "Sandbox life correction" })).toHaveFocus();
    await user.tab();
    const secondCancel = screen.getByRole("button", { name: "Cancel" });
    expect(secondCancel).toHaveFocus();
    await user.tab();
    expect(screen.getByRole("combobox", { name: "Player" })).toHaveFocus();
    await user.tab();
    const lifeInput = screen.getByRole("spinbutton", { name: "New life total" });
    expect(lifeInput).toHaveFocus();
    await user.clear(lifeInput);
    await user.type(lifeInput, "21");
    await user.tab();
    const apply = screen.getByRole("button", { name: "Apply correction" });
    expect(apply).toHaveFocus();
    await user.keyboard("{Enter}");

    const firstStatus = await screen.findByRole("status");
    expect(firstStatus).toHaveTextContent("Current committed life for You (Player 1): 21.");
    expect(firstStatus).toHaveFocus();
    await user.tab({ shift: true });
    const close = screen.getByRole("button", { name: "Close" });
    expect(close).toHaveFocus();
    await user.keyboard(" ");
    expect(screen.queryByRole("dialog", { name: "Sandbox life correction" })).toBeNull();
    expect(trigger).toHaveFocus();

    await user.keyboard(" ");
    expect(await screen.findByRole("dialog", { name: "Sandbox life correction" })).toHaveFocus();
    await user.tab();
    const thirdCancel = screen.getByRole("button", { name: "Cancel" });
    expect(thirdCancel).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(screen.queryByRole("dialog", { name: "Sandbox life correction" })).toBeNull();
    expect(trigger).toHaveFocus();

    await user.keyboard(" ");
    expect(await screen.findByRole("dialog", { name: "Sandbox life correction" })).toHaveFocus();
    await user.tab();
    await user.tab();
    await user.tab();
    const secondLifeInput = screen.getByRole("spinbutton", { name: "New life total" });
    expect(secondLifeInput).toHaveFocus();
    await user.clear(secondLifeInput);
    await user.type(secondLifeInput, "22");
    await user.tab();
    const secondApply = screen.getByRole("button", { name: "Apply correction" });
    expect(secondApply).toHaveFocus();
    await user.keyboard(" ");

    const secondStatus = await screen.findByText("Current committed life for You (Player 1): 22.");
    expect(secondStatus).toHaveFocus();
    await user.tab({ shift: true });
    const secondClose = screen.getByRole("button", { name: "Close" });
    expect(secondClose).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(screen.queryByRole("dialog", { name: "Sandbox life correction" })).toBeNull();
    expect(trigger).toHaveFocus();

    expect(harness.adapter.submitAction).toHaveBeenCalledTimes(2);
    expect(vi.mocked(harness.adapter.submitAction).mock.calls.map(([action]) => action)).toEqual([
      { type: "Debug", data: { type: "SetLife", data: { player_id: 0, life: 21 } } },
      { type: "Debug", data: { type: "SetLife", data: { player_id: 0, life: 22 } } },
    ]);
  });

  it("dismisses locally on Escape while auto-pass is active without changing game or selection", async () => {
    const harness = makeHarness();
    const autoPassState: GameState = {
      ...harness.initialState,
      auto_pass: { 0: { type: "UntilTurnBoundary", until: "EndOfCurrentTurn" } },
    };
    const dispatch = vi.fn().mockResolvedValue([]);
    act(() => {
      useGameStore.setState({ gameState: autoPassState, waitingFor: autoPassState.waiting_for, dispatch });
      useUiStore.setState({ selectedCardIds: [10, 20] });
    });
    const user = userEvent.setup();
    render(
      <>
        <GameKeyboardShortcutsHarness />
        <SandboxLifeCorrection />
      </>,
    );

    const trigger = screen.getByRole("button", { name: "Life correction" });
    await user.click(trigger);
    const dialog = await screen.findByRole("dialog", { name: "Sandbox life correction" });
    dialog.focus();
    const stateBeforeEscape = useGameStore.getState().gameState;
    await user.keyboard("{Escape}");

    expect(screen.queryByRole("dialog", { name: "Sandbox life correction" })).toBeNull();
    expect(trigger).toHaveFocus();
    expect(useGameStore.getState().gameState).toBe(stateBeforeEscape);
    expect(useGameStore.getState().gameState?.auto_pass).toEqual(autoPassState.auto_pass);
    expect(useUiStore.getState().selectedCardIds).toEqual([10, 20]);
    expect(dispatch).not.toHaveBeenCalled();
    expect(harness.adapter.submitAction).not.toHaveBeenCalled();
  });

  it("keeps Escape and T local during ManaPayment while the outside T shortcut still dispatches", async () => {
    const harness = makeHarness();
    const waitingFor = buildManaPaymentWaitingFor();
    const paymentState: GameState = { ...harness.initialState, waiting_for: waitingFor };
    const dispatch = vi.fn().mockResolvedValue([]);
    const tapAction: GameAction = {
      type: "TapLandForMana",
      data: {
        selection: {
          source: { object_id: 17, incarnation: 1 },
          ability_index: null,
          mana_type: "Green",
          output: { type: "Concrete", data: "Green" },
          atomic_combination: null,
          restrictions: [],
          penalty: "None",
          taps_for_mana: [],
        },
      },
    };
    act(() => {
      useGameStore.setState({
        gameState: paymentState,
        waitingFor,
        dispatch,
        manaPaymentShortcutActions: [tapAction],
      });
    });
    const user = userEvent.setup();
    render(
      <>
        <GameKeyboardShortcutsHarness />
        <SandboxLifeCorrection />
        <button type="button">Outside control</button>
      </>,
    );

    const trigger = screen.getByRole("button", { name: "Life correction" });
    await user.click(trigger);
    const dialog = await screen.findByRole("dialog", { name: "Sandbox life correction" });
    dialog.focus();
    const stateBeforeShortcut = useGameStore.getState().gameState;
    await user.keyboard("t");
    expect(dispatch).not.toHaveBeenCalled();
    expect(useGameStore.getState().gameState).toBe(stateBeforeShortcut);

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "Sandbox life correction" })).toBeNull();
    expect(trigger).toHaveFocus();
    expect(useGameStore.getState().gameState).toBe(stateBeforeShortcut);
    expect(useGameStore.getState().waitingFor).toBe(waitingFor);
    expect(dispatch).not.toHaveBeenCalled();
    expect(harness.adapter.submitAction).not.toHaveBeenCalled();

    screen.getByRole("button", { name: "Outside control" }).focus();
    await user.keyboard("t");
    expect(dispatch).toHaveBeenCalledWith(tapAction);
  });

  it("does not run Z undo inside the boundary and preserves the global shortcut outside", async () => {
    const harness = makeHarness();
    const undo = vi.fn().mockResolvedValue(undefined);
    const history = [harness.initialState];
    act(() => useGameStore.setState({ stateHistory: history, undo }));
    const user = userEvent.setup();
    render(
      <>
        <GameKeyboardShortcutsHarness />
        <SandboxLifeCorrection />
        <button type="button">Outside control</button>
      </>,
    );

    const trigger = screen.getByRole("button", { name: "Life correction" });
    trigger.focus();
    await user.keyboard("z");
    expect(undo).not.toHaveBeenCalled();

    await user.click(trigger);
    const dialog = await screen.findByRole("dialog", { name: "Sandbox life correction" });
    dialog.focus();
    const stateBeforeUndoShortcut = useGameStore.getState().gameState;
    await user.keyboard("z");
    expect(undo).not.toHaveBeenCalled();
    expect(useGameStore.getState().stateHistory).toBe(history);
    expect(useGameStore.getState().gameState).toBe(stateBeforeUndoShortcut);

    screen.getByRole("button", { name: "Outside control" }).focus();
    await user.keyboard("z");
    expect(undo).toHaveBeenCalledTimes(1);
  });

  it("does not apply a draft after the engine snapshot changes during editing", () => {
    const harness = makeHarness();
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("23");

    const updatedState: GameState = {
      ...harness.initialState,
      players: harness.initialState.players.map((player) =>
        player.id === 0 ? { ...player, life: 19 } : player,
      ),
    };
    act(() => {
      useGameStore.getState().commitEngineSnapshot(
        { state: updatedState, legalResult: buildLegalActionsResult(), seq: nextSnapshotSeq() },
        { events: [], logEntries: [] },
      );
    });

    expect(screen.getByRole("alert")).toHaveTextContent("The game changed while editing.");
    expect(screen.queryByRole("button", { name: "Apply correction" })).toBeNull();
    expect(harness.adapter.submitAction).not.toHaveBeenCalled();
  });

  it("rechecks the current store snapshot synchronously just before submit", () => {
    const harness = makeHarness();
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("23");
    const form = screen.getByRole("form", { name: "Sandbox life correction" });
    const updatedState: GameState = {
      ...harness.initialState,
      players: harness.initialState.players.map((player) =>
        player.id === 0 ? { ...player, life: 19 } : player,
      ),
    };

    act(() => {
      useGameStore.setState({ gameState: updatedState });
      fireEvent.submit(form);
    });

    expect(harness.adapter.submitAction).not.toHaveBeenCalled();
    expect(screen.getByRole("alert")).toHaveTextContent("The game changed while editing.");
  });

  it("does not apply a draft to a different game session", () => {
    const harness = makeHarness();
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("23");

    act(() => {
      useGameStore.setState((state) => ({ gameSessionGeneration: state.gameSessionGeneration + 1 }));
    });

    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByRole("button", { name: "Life correction" })).toHaveAttribute("aria-expanded", "false");
    expect(screen.getByRole("button", { name: "Life correction" })).not.toHaveFocus();
    expect(screen.queryByRole("button", { name: "Apply correction" })).toBeNull();
    expect(harness.adapter.submitAction).not.toHaveBeenCalled();
  });

  it("does not treat a swallowed stale-action no-op as success", async () => {
    const harness = makeHarness();
    vi.mocked(harness.adapter.submitAction).mockRejectedValueOnce(
      new AdapterError(AdapterErrorCode.STALE_ACTION, "stale action", false),
    );
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("21");

    fireEvent.click(screen.getByRole("button", { name: "Apply correction" }));

    expect(await screen.findByText("The engine did not confirm this correction. Check the current snapshot before trying again.")).toBeInTheDocument();
    expect(useGameStore.getState().engineCommitEpoch).toBe(0);
    expect(useGameStore.getState().gameState?.players[0]?.life).toBe(20);
  });

  it("does not treat an empty event batch as a confirmed SetLife", async () => {
    const harness = makeHarness();
    vi.mocked(harness.adapter.submitAction).mockResolvedValueOnce({ events: [] });
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("21");

    fireEvent.click(screen.getByRole("button", { name: "Apply correction" }));

    expect(await screen.findByText("The engine did not confirm this correction. Check the current snapshot before trying again.")).toBeInTheDocument();
    expect(useGameStore.getState().gameState?.players[0]?.life).toBe(20);
  });

  it("reports an explicit engine permission denial without claiming an edit", async () => {
    const harness = makeHarness();
    vi.mocked(harness.adapter.submitAction).mockRejectedValueOnce(
      new AdapterError(AdapterErrorCode.ACTION_REJECTED, "Debug permission denied", false),
    );
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("21");

    fireEvent.click(screen.getByRole("button", { name: "Apply correction" }));

    expect(await screen.findByText("The correction could not be submitted.")).toBeInTheDocument();
    expect(useGameStore.getState().gameState?.players[0]?.life).toBe(20);
    expect(useGameStore.getState().events).toEqual([]);
  });

  it("rejects unchanged and invalid life totals before dispatch", () => {
    const harness = makeHarness();
    render(<SandboxLifeCorrection />);
    openPanel();
    const submit = screen.getByRole("button", { name: "Apply correction" });
    const form = screen.getByRole("form", { name: "Sandbox life correction" });

    expect(screen.getByRole("spinbutton", { name: "New life total" })).toHaveValue(20);
    expect(submit).toBeDisabled();
    fireEvent.submit(form);
    setNewLife("");
    expect(submit).toBeDisabled();
    fireEvent.submit(form);

    expect(harness.adapter.submitAction).not.toHaveBeenCalled();
  });

  it("does not confirm a matching life total when the engine event has another actor", async () => {
    const harness = makeHarness();
    vi.mocked(harness.adapter.submitAction).mockImplementationOnce(async (action) =>
      harness.applySetLife(action, 1),
    );
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("21");

    fireEvent.click(screen.getByRole("button", { name: "Apply correction" }));

    expect(await screen.findByText("The engine did not confirm this correction. Check the current snapshot before trying again.")).toBeInTheDocument();
    expect(useGameStore.getState().gameState?.players[0]?.life).toBe(21);
  });

  it("requires the exact engine-authored target label and life in the debug event", async () => {
    const harness = makeHarness();
    vi.mocked(harness.adapter.submitAction).mockImplementationOnce(async (action, actor) => {
      const result = harness.applySetLife(action, actor);
      return {
        ...result,
        events: [{
          type: "DebugActionUsed",
          data: { player_id: actor, description: "SetLife (Player 2 → 21)" },
        }],
      };
    });
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("21");

    fireEvent.click(screen.getByRole("button", { name: "Apply correction" }));

    expect(await screen.findByText("The engine did not confirm this correction. Check the current snapshot before trying again.")).toBeInTheDocument();
    expect(useGameStore.getState().gameState?.players[0]?.life).toBe(21);
  });

  it("does not report success when the same-session target changes before the submitted snapshot commits", async () => {
    const harness = makeHarness();
    let releaseSubmit!: () => void;
    let releaseSnapshot!: () => void;
    vi.mocked(harness.adapter.submitAction).mockImplementationOnce((action, actor) =>
      new Promise((resolve) => {
        releaseSubmit = () => resolve(harness.applySetLife(action, actor));
      }),
    );
    vi.mocked(harness.adapter.getSnapshot).mockImplementationOnce(async () => {
      await new Promise<void>((resolve) => {
        releaseSnapshot = resolve;
      });
      return {
        state: harness.currentEngineState(),
        legalResult: buildLegalActionsResult(),
        seq: nextSnapshotSeq(),
      };
    });
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("17");
    fireEvent.click(screen.getByRole("button", { name: "Apply correction" }));

    await waitFor(() => expect(harness.adapter.submitAction).toHaveBeenCalledTimes(1));
    await act(async () => {
      releaseSubmit();
      await Promise.resolve();
    });
    await waitFor(() => expect(harness.adapter.getSnapshot).toHaveBeenCalledTimes(1));
    harness.setEngineLife(0, 16);
    await act(async () => {
      releaseSnapshot();
      await Promise.resolve();
    });

    expect(await screen.findByText("The engine did not confirm this correction. Check the current snapshot before trying again.")).toBeInTheDocument();
    expect(useGameStore.getState().gameState?.players[0]?.life).toBe(16);
  });

  it("rejects duplicate submits while an adapter request is pending", async () => {
    const harness = makeHarness();
    let finish!: () => void;
    vi.mocked(harness.adapter.submitAction).mockImplementationOnce((action, actor) =>
      new Promise((resolve) => {
        finish = () => resolve(harness.applySetLife(action, actor));
      }),
    );
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("21");

    const form = screen.getByRole("form", { name: "Sandbox life correction" });
    fireEvent.submit(form);
    fireEvent.submit(form);
    expect(harness.adapter.submitAction).toHaveBeenCalledTimes(1);

    await act(async () => {
      finish();
      await Promise.resolve();
    });
    expect(await screen.findByText("Current committed life for You (Player 1): 21.")).toBeInTheDocument();
  });

  it("does not leak a submitted result into a panel closed and reopened before completion", async () => {
    const harness = makeHarness();
    let finish!: () => void;
    vi.mocked(harness.adapter.submitAction).mockImplementationOnce((action, actor) =>
      new Promise((resolve) => {
        finish = () => resolve(harness.applySetLife(action, actor));
      }),
    );
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("22");
    fireEvent.click(screen.getByRole("button", { name: "Apply correction" }));
    expect(screen.getByText("Correction submitted; closing this panel will not cancel it.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(harness.adapter.submitAction).toHaveBeenCalledTimes(1);
    openPanel();
    expect(screen.getByText("Correction submitted; closing this panel will not cancel it.")).toBeInTheDocument();

    await act(async () => {
      finish();
      await Promise.resolve();
    });

    expect(useGameStore.getState().gameState?.players[0]?.life).toBe(22);
    expect(screen.queryByText("Current committed life for You (Player 1): 22.")).toBeNull();
    expect(screen.getByRole("alert")).toHaveTextContent("The game changed while editing.");
  });

  it("clears post-result feedback synchronously when the game session is replaced", async () => {
    const harness = makeHarness("sandbox-before-replacement");
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("21");
    fireEvent.click(screen.getByRole("button", { name: "Apply correction" }));
    expect(await screen.findByText("Current committed life for You (Player 1): 21.")).toBeInTheDocument();

    makeHarness("sandbox-after-replacement");

    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByText("Current committed life for You (Player 1): 21.")).toBeNull();
    openPanel();
    expect(screen.getByRole("spinbutton", { name: "New life total" })).toHaveValue(20);
    expect(screen.queryByText("Current committed life for You (Player 1): 21.")).toBeNull();
    expect(harness.adapter.submitAction).toHaveBeenCalledTimes(1);
  });

  it("does not show an in-flight result after the game session is replaced", async () => {
    const harness = makeHarness("sandbox-before-inflight-replacement");
    let finish!: () => void;
    vi.mocked(harness.adapter.submitAction).mockImplementationOnce((action, actor) =>
      new Promise((resolve) => {
        finish = () => resolve(harness.applySetLife(action, actor));
      }),
    );
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("21");
    fireEvent.click(screen.getByRole("button", { name: "Apply correction" }));
    expect(screen.getByText("Correction submitted; closing this panel will not cancel it.")).toBeInTheDocument();

    const replacement = makeHarness("sandbox-after-inflight-replacement");
    expect(screen.queryByRole("dialog")).toBeNull();
    openPanel();
    expect(screen.queryByText("Correction submitted; closing this panel will not cancel it.")).toBeNull();
    expect(screen.queryByText("Current committed life for You (Player 1): 21.")).toBeNull();

    await act(async () => {
      finish();
      await Promise.resolve();
    });

    expect(useGameStore.getState().gameState?.players[0]?.life).toBe(20);
    expect(screen.queryByText("Current committed life for You (Player 1): 21.")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(replacement.adapter.submitAction).not.toHaveBeenCalled();
  });

  it("shows a rejected adapter submission without claiming that life changed", async () => {
    const harness = makeHarness();
    vi.mocked(harness.adapter.submitAction).mockRejectedValueOnce(new Error("sandbox action rejected"));
    render(<SandboxLifeCorrection />);
    openPanel();
    setNewLife("21");

    fireEvent.click(screen.getByRole("button", { name: "Apply correction" }));

    expect(await screen.findByText("The correction could not be submitted.")).toBeInTheDocument();
    expect(useGameStore.getState().gameState?.players[0]?.life).toBe(20);
  });
});
