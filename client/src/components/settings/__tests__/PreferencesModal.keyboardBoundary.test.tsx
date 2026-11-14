import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameAction, GameEvent } from "../../../adapter/types";
import { useGameStore } from "../../../stores/gameStore";
import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { useUiStore } from "../../../stores/uiStore";
import { gameStateFactory } from "../../../test/factories/gameStateFactory";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers";
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
  act(() => {
    useGameStore.getState().reset();
    useMultiplayerStore.setState({ activePlayerId: null, isSpectator: false });
    useMultiplayerStore.setState({ displayName: "" });
    useUiStore.getState().dismissPreview();
    useUiStore.setState({
      fullControl: false,
      flexEditMode: false,
      helpSheetOpen: false,
      debugPanelOpen: false,
      debugInteractionMode: false,
      shiftHeld: false,
      altHeld: false,
      isDragging: false,
      mobileHandGesture: null,
    });
    usePreferencesStore.setState({
      cardPreviewMode: "follow",
      cardPreviewHoverDelayMs: 0,
      boardBackground: "auto-wubrg",
    });
  });
  setGameStoreForTest({
    gameMode: "ai",
    dispatch: storeDispatchMock,
    gameState: gameStateFactory.withPlayers(0, 1).activePlayer(0).priority(0)
      .build(),
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
  let savedPreferences: ReturnType<typeof usePreferencesStore.getState>;
  let savedMultiplayer: ReturnType<typeof useMultiplayerStore.getState>;

  beforeEach(() => {
    savedPreferences = usePreferencesStore.getState();
    savedMultiplayer = useMultiplayerStore.getState();
    dispatchActionMock.mockReset();
    dispatchActionMock.mockResolvedValue(undefined);
    storeDispatchMock.mockReset();
    storeDispatchMock.mockResolvedValue([]);
    seedFirstPriority();
  });

  afterEach(() => {
    cleanup();
    useGameStore.getState().reset();
    useMultiplayerStore.setState(savedMultiplayer);
    usePreferencesStore.setState(savedPreferences);
    useUiStore.getState().dismissPreview();
    useUiStore.setState({
      fullControl: false,
      flexEditMode: false,
      helpSheetOpen: false,
      debugPanelOpen: false,
      debugInteractionMode: false,
      shiftHeld: false,
    });
  });

  it("reaches authorized Space, F and Enter shortcuts outside Settings", () => {
    render(<KeyboardSettingsHarness />);
    const launcher = screen.getByRole("button", { name: "Settings" });
    launcher.focus();

    fireEvent.keyDown(launcher, { key: " " });
    expect(
      [...storeDispatchMock.mock.calls, ...dispatchActionMock.mock.calls]
        .map(([action]) => action),
    ).toEqual([{ type: "PassPriority" }]);
    storeDispatchMock.mockClear();
    dispatchActionMock.mockClear();

    fireEvent.keyDown(launcher, { key: "f" });
    expect(useUiStore.getState().fullControl).toBe(true);
    expect(
      [...storeDispatchMock.mock.calls, ...dispatchActionMock.mock.calls]
        .map(([action]) => action),
    ).toEqual([]);
    storeDispatchMock.mockClear();
    dispatchActionMock.mockClear();

    fireEvent.keyDown(launcher, { key: "Enter" });
    expect(
      [...storeDispatchMock.mock.calls, ...dispatchActionMock.mock.calls]
        .map(([action]) => action),
    ).toEqual([{
      type: "SetAutoPass",
      data: { mode: { type: "UntilTurnBoundary", until: "EndOfCurrentTurn" } },
    }]);
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
    expect(dispatchActionMock).not.toHaveBeenCalled();
    expect(storeDispatchMock).not.toHaveBeenCalled();
    expect(event.defaultPrevented).toBe(false);
  });

  it("does not arm end-turn auto-pass from an open Settings panel", async () => {
    render(<KeyboardSettingsHarness />);
    const { dialog, control } = await openSettingsAndFocusControl();
    const event = new KeyboardEvent("keydown", {
      key: "Enter",
      bubbles: true,
      cancelable: true,
    });

    fireEvent(control, event);

    expect(dialog).toBeInTheDocument();
    expect(dispatchActionMock).not.toHaveBeenCalled();
    expect(storeDispatchMock).not.toHaveBeenCalled();
    expect(useUiStore.getState().fullControl).toBe(false);
    expect(event.defaultPrevented).toBe(false);
  });

  it.each([" ", "{Enter}"])(
    "keeps native tab activation working with %s",
    async (key) => {
      const user = userEvent.setup();
      render(<KeyboardSettingsHarness />);
      const { dialog } = await openSettingsAndFocusControl();
      const tab = within(dialog).getByRole("button", { name: "Multiplayer" });
      tab.focus();

      await user.keyboard(key);

      expect(await within(dialog).findByPlaceholderText("Enter your name"))
        .toBeInTheDocument();
      expect(dispatchActionMock).not.toHaveBeenCalled();
      expect(storeDispatchMock).not.toHaveBeenCalled();
      expect(useUiStore.getState().fullControl).toBe(false);
    },
  );

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
    await user.type(nameInput, " fzt");
    expect(nameInput).toHaveValue("Pilot fzt");
    expect(useMultiplayerStore.getState().displayName).toBe("Pilot fzt");
    expect(useUiStore.getState().fullControl).toBe(false);
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

    storeDispatchMock.mockClear();
    dispatchActionMock.mockClear();
    fireEvent.keyDown(launcher, { key: "f" });
    expect(useUiStore.getState().fullControl).toBe(true);
    expect(
      [...storeDispatchMock.mock.calls, ...dispatchActionMock.mock.calls]
        .map(([action]) => action),
    ).toEqual([]);

    storeDispatchMock.mockClear();
    dispatchActionMock.mockClear();
    fireEvent.keyDown(launcher, { key: " " });
    expect(
      [...storeDispatchMock.mock.calls, ...dispatchActionMock.mock.calls]
        .map(([action]) => action),
    ).toEqual([{ type: "PassPriority" }]);
  });

  it("observes Alt in the registered background portal while containing game keys and local arrows", async () => {
    render(<KeyboardSettingsHarness />);
    const { launcher, dialog, control } = await openSettingsAndFocusControl();
    const trigger = within(dialog).getByRole("button", {
      name: "Board Background",
    });
    fireEvent.click(trigger);
    const listbox = await screen.findByRole("listbox", {
      name: "Board Background",
    });
    expect(dialog).not.toContainElement(listbox);
    const selected = within(listbox).getByRole("option", {
      name: "Auto (match deck)",
    });
    const next = within(listbox).getByRole("option", { name: "Random" });
    expect(selected).toBeEnabled();
    expect(selected).toHaveFocus();

    fireEvent.keyDown(selected, { key: "ArrowDown" });
    expect(next).toHaveFocus();
    fireEvent.keyDown(next, { key: "ArrowUp" });
    expect(selected).toHaveFocus();

    control.focus();
    const outsideArrow = new KeyboardEvent("keydown", {
      key: "ArrowDown",
      bubbles: true,
      cancelable: true,
    });
    fireEvent(control, outsideArrow);
    expect(control).toHaveFocus();
    expect(outsideArrow.defaultPrevented).toBe(false);
    expect(listbox).toBeInTheDocument();
    selected.focus();

    const alt = new KeyboardEvent("keydown", {
      key: "Alt",
      bubbles: true,
      cancelable: true,
    });
    fireEvent(selected, alt);
    expect(useUiStore.getState().altHeld).toBe(true);
    expect(alt.defaultPrevented).toBe(true);
    const repeat = new KeyboardEvent("keydown", {
      key: "Alt",
      repeat: true,
      bubbles: true,
      cancelable: true,
    });
    fireEvent(selected, repeat);
    fireEvent.keyUp(selected, { key: "Alt" });
    expect(useUiStore.getState().altHeld).toBe(true);
    expect(repeat.defaultPrevented).toBe(false);

    for (const key of ["f", " ", "Enter"]) {
      const event = new KeyboardEvent("keydown", {
        key,
        bubbles: true,
        cancelable: true,
      });
      fireEvent(selected, event);
      expect(event.defaultPrevented).toBe(false);
      expect(listbox).toBeInTheDocument();
      expect(useUiStore.getState().fullControl).toBe(false);
      expect(dispatchActionMock).not.toHaveBeenCalled();
      expect(storeDispatchMock).not.toHaveBeenCalled();
    }

    fireEvent.keyDown(selected, { key: "Escape" });
    expect(screen.queryByRole("listbox", { name: "Board Background" })).not
      .toBeInTheDocument();
    expect(trigger).toHaveFocus();
    expect(dialog).toBeInTheDocument();
    fireEvent.keyDown(trigger, { key: "Escape" });
    expect(dialog).not.toBeInTheDocument();
    expect(launcher).toHaveFocus();
    expect(dispatchActionMock).not.toHaveBeenCalled();
    expect(storeDispatchMock).not.toHaveBeenCalled();
  });

  it("selects a background through native Enter without arming game auto-pass", async () => {
    const user = userEvent.setup();
    render(<KeyboardSettingsHarness />);
    const { dialog } = await openSettingsAndFocusControl();
    const trigger = within(dialog).getByRole("button", {
      name: "Board Background",
    });
    fireEvent.click(trigger);
    const listbox = await screen.findByRole("listbox", {
      name: "Board Background",
    });
    const selected = within(listbox).getByRole("option", {
      name: "Auto (match deck)",
    });
    const next = within(listbox).getByRole("option", { name: "Random" });
    expect(selected).toHaveFocus();
    fireEvent.keyDown(selected, { key: "ArrowDown" });
    expect(next).toHaveFocus();

    await user.keyboard("{Enter}");

    expect(screen.queryByRole("listbox", { name: "Board Background" })).not
      .toBeInTheDocument();
    expect(usePreferencesStore.getState().boardBackground).toBe("random");
    expect(trigger).toHaveTextContent("Random");
    expect(trigger).toHaveFocus();
    expect(dialog).toBeInTheDocument();
    expect(dispatchActionMock).not.toHaveBeenCalled();
    expect(storeDispatchMock).not.toHaveBeenCalled();
  });
});
