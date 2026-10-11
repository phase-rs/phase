import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameEvent } from "../../../adapter/types";
import { useGameStore } from "../../../stores/gameStore";
import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { useUiStore } from "../../../stores/uiStore";
import { buildGameState, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory";
import { useKeyboardShortcuts } from "../../../hooks/useKeyboardShortcuts";
import { ChromeControls } from "../../chrome/ChromeControls";

const dispatchActionMock = vi.hoisted(() => vi.fn<(action: GameAction) => Promise<void>>());
const storeDispatchMock = vi.fn<(action: GameAction) => Promise<GameEvent[]>>();

vi.mock("../../../game/dispatch", () => ({
  dispatchAction: dispatchActionMock,
}));

vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

vi.mock("../visual-packs/VisualPackManager.tsx", () => ({
  VisualPackManager: () => null,
}));

vi.mock("../../chrome/AccountControl", () => ({ AccountControl: () => null }));
vi.mock("../../chrome/FullscreenButton", () => ({ FullscreenButton: () => null }));
vi.mock("../../chrome/VolumeControl", () => ({ VolumeControl: () => null }));

function KeyboardSettingsHarness() {
  useKeyboardShortcuts();
  return <ChromeControls hideVolume hideLanguage />;
}

function seedFirstPriority(): void {
  const waitingFor = buildPriorityWaitingFor({ data: { player: 0 } });
  act(() => {
    useGameStore.getState().reset();
    useGameStore.setState({
      gameMode: "ai",
      dispatch: storeDispatchMock,
      gameState: buildGameState({
        waiting_for: waitingFor,
        active_player: 0,
        priority_player: 0,
      }),
      waitingFor,
    });
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
    useMultiplayerStore.setState({ displayName: "" });
    useUiStore.setState({ fullControl: false, flexEditMode: false, helpSheetOpen: false });
  });
}

async function openSettingsAndFocusControl() {
  const launcher = screen.getByRole("button", { name: "Settings" });
  fireEvent.click(launcher);
  const dialog = await screen.findByRole("dialog", { name: "Settings" });
  const control = within(dialog).getByRole("button", { name: "Gameplay" });
  control.focus();
  return { launcher, dialog, control };
}

describe("PreferencesModal keyboard boundary", () => {
  beforeEach(() => {
    dispatchActionMock.mockReset();
    dispatchActionMock.mockResolvedValue(undefined);
    storeDispatchMock.mockReset();
    storeDispatchMock.mockResolvedValue([]);
    seedFirstPriority();
  });

  afterEach(() => {
    cleanup();
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
    useUiStore.setState({ fullControl: false, flexEditMode: false, helpSheetOpen: false });
  });

  it("does not pass Space from an open Settings panel to the game shortcut", async () => {
    render(<KeyboardSettingsHarness />);
    const { dialog, control } = await openSettingsAndFocusControl();
    const event = new KeyboardEvent("keydown", { key: " ", bubbles: true, cancelable: true });

    fireEvent(control, event);

    expect(dialog).toBeInTheDocument();
    expect(dispatchActionMock).not.toHaveBeenCalled();
    expect(storeDispatchMock).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(false);
  });

  it("does not toggle Full Control from an open Settings panel", async () => {
    render(<KeyboardSettingsHarness />);
    const { dialog, control } = await openSettingsAndFocusControl();
    const event = new KeyboardEvent("keydown", { key: "f", bubbles: true, cancelable: true });

    fireEvent(control, event);

    expect(dialog).toBeInTheDocument();
    expect(useUiStore.getState().fullControl).toBe(false);
    expect(event.defaultPrevented).toBe(false);
  });

  it("keeps text editing inside Settings working", async () => {
    const user = userEvent.setup();
    render(<KeyboardSettingsHarness />);
    const { dialog } = await openSettingsAndFocusControl();
    const multiplayerTab = within(dialog).getByRole("button", { name: "Multiplayer" });
    multiplayerTab.focus();
    await user.keyboard(" ");
    const nameInput = within(dialog).getByPlaceholderText("Enter your name");

    await user.type(nameInput, "Pilot");

    expect(nameInput).toHaveValue("Pilot");
    expect(useMultiplayerStore.getState().displayName).toBe("Pilot");
    expect(dispatchActionMock).not.toHaveBeenCalled();
    expect(storeDispatchMock).not.toHaveBeenCalled();
  });

  it("restores focus on Escape and resumes game shortcuts after Settings closes", async () => {
    render(<KeyboardSettingsHarness />);
    const { launcher, dialog, control } = await openSettingsAndFocusControl();
    const escape = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });

    fireEvent(control, escape);

    expect(escape.defaultPrevented).toBe(true);
    expect(dialog).not.toBeInTheDocument();
    expect(launcher).toHaveFocus();

    fireEvent.keyDown(launcher, { key: "f" });
    expect(useUiStore.getState().fullControl).toBe(true);

    fireEvent.keyDown(launcher, { key: " " });
    expect(storeDispatchMock).toHaveBeenCalledWith({ type: "PassPriority" });
  });
});
