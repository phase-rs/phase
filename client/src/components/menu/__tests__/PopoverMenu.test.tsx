import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { act, useState } from "react";

import { PopoverMenu } from "../PopoverMenu.tsx";
import { FolderActionsMenu } from "../FolderActionsMenu.tsx";
import { ConfirmDialog } from "../../ui/ConfirmDialog.tsx";
import { useKeyboardShortcuts } from "../../../hooks/useKeyboardShortcuts.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { setGameStoreForTest } from "../../../test/helpers/gameStoreHelpers.ts";

const { dispatchActionMock } = vi.hoisted(() => ({ dispatchActionMock: vi.fn() }));
vi.mock("../../../game/dispatch.ts", () => ({ dispatchAction: dispatchActionMock }));

afterEach(() => {
  cleanup();
});

describe("PopoverMenu", () => {
  describe("keyboard boundary", () => {
    function KeyboardPopoverHarness({ variant = "dialog" }: { variant?: "menu" | "dialog" }) {
      useKeyboardShortcuts();
      return (
        <>
          <button type="button">Outside control</button>
          <PopoverMenu ariaLabel="Keyboard controls" variant={variant}>
            {(close) => (
              <>
                <details>
                  <summary>Saved variants</summary>
                  <p>First replacement, then second replacement</p>
                </details>
                <button type="button" onClick={close}>Close panel</button>
              </>
            )}
          </PopoverMenu>
        </>
      );
    }

    beforeEach(() => {
      dispatchActionMock.mockClear();
      useGameStore.getState().reset();
      act(() => {
        useUiStore.setState({ helpSheetOpen: false, flexEditMode: false, fullControl: false, selectedCardIds: [10] });
      });
    });

    afterEach(() => {
      cleanup();
      useGameStore.getState().reset();
      act(() => useUiStore.setState({ selectedCardIds: [], fullControl: false }));
    });

    it.each(["Enter", " "])("blocks the active game shortcut for %j inside a dialog", (key) => {
      const { dispatch } = setGameStoreForTest({ gameState: gameStateFactory.priority().build() });
      render(<KeyboardPopoverHarness />);

      // Paired control proves the real hook and both dispatch paths are active.
      fireEvent.keyDown(screen.getByRole("button", { name: "Outside control" }), { key });
      if (key === "Enter") {
        expect(dispatchActionMock).toHaveBeenCalledWith({
          type: "SetAutoPass",
          data: { mode: { type: "UntilTurnBoundary", until: "EndOfCurrentTurn" } },
        });
      } else {
        expect(dispatch).toHaveBeenCalledWith({ type: "PassPriority" });
      }
      dispatchActionMock.mockClear();
      vi.mocked(dispatch).mockClear();

      fireEvent.click(screen.getByRole("button", { name: "Keyboard controls" }));
      const summary = screen.getByText("Saved variants");
      const button = screen.getByRole("button", { name: "Close panel" });
      for (const control of [summary, button]) {
        control.focus();
        const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
        fireEvent(control, event);
        expect(event.defaultPrevented).toBe(false);
      }

      expect(dispatchActionMock).not.toHaveBeenCalled();
      expect(dispatch).not.toHaveBeenCalled();
      expect(screen.getByRole("dialog", { name: "Keyboard controls" })).toBeInTheDocument();
    });

    it.each(["Tab", "F"])("keeps %j local without preventing its default", (key) => {
      render(<KeyboardPopoverHarness />);
      fireEvent.click(screen.getByRole("button", { name: "Keyboard controls" }));
      const documentKeyDown = vi.fn();
      const windowKeyDown = vi.fn();
      document.addEventListener("keydown", documentKeyDown);
      window.addEventListener("keydown", windowKeyDown);
      try {
        const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
        fireEvent(screen.getByText("Saved variants"), event);

        expect(event.defaultPrevented).toBe(false);
        expect(documentKeyDown).not.toHaveBeenCalled();
        expect(windowKeyDown).not.toHaveBeenCalled();
        expect(useUiStore.getState().fullControl).toBe(false);
      } finally {
        document.removeEventListener("keydown", documentKeyDown);
        window.removeEventListener("keydown", windowKeyDown);
      }
    });

    it("closes on Escape from a dialog control and restores focus without canceling game input", () => {
      render(<KeyboardPopoverHarness />);
      const trigger = screen.getByRole("button", { name: "Keyboard controls" });
      fireEvent.click(trigger);
      const button = screen.getByRole("button", { name: "Close panel" });
      button.focus();

      fireEvent.keyDown(button, { key: "Escape" });

      expect(screen.queryByRole("dialog", { name: "Keyboard controls" })).not.toBeInTheDocument();
      expect(trigger).toHaveFocus();
      expect(useUiStore.getState().selectedCardIds).toEqual([10]);
      expect(dispatchActionMock).not.toHaveBeenCalled();
    });

    it("preserves game shortcut bubbling from the default menu variant", () => {
      const { dispatch } = setGameStoreForTest({ gameState: gameStateFactory.priority().build() });
      render(<KeyboardPopoverHarness variant="menu" />);
      fireEvent.click(screen.getByRole("button", { name: "Keyboard controls" }));
      const button = screen.getByRole("button", { name: "Close panel" });

      fireEvent.keyDown(button, { key: "Enter" });
      fireEvent.keyDown(button, { key: " " });

      expect(dispatchActionMock).toHaveBeenCalledWith({
        type: "SetAutoPass",
        data: { mode: { type: "UntilTurnBoundary", until: "EndOfCurrentTurn" } },
      });
      expect(dispatch).toHaveBeenCalledWith({ type: "PassPriority" });
      expect(screen.getByRole("menu", { name: "Keyboard controls" })).toBeInTheDocument();
    });
  });

  function openDialog() {
    render(
      <PopoverMenu ariaLabel="Layout" variant="dialog">
        {(close) => (
          <button type="button" onClick={close}>
            Sort by color
          </button>
        )}
      </PopoverMenu>,
    );

    const trigger = screen.getByRole("button", { name: "Layout" });
    fireEvent.click(trigger);
    return { dialog: screen.getByRole("dialog", { name: "Layout" }), trigger };
  }

  it("moves_focus_into_an_opt_in_dialog_on_open", () => {
    const { dialog } = openDialog();

    expect(dialog).toHaveFocus();
  });

  it("restores_trigger_focus_when_a_dialog_closes_with_escape", () => {
    const { trigger } = openDialog();

    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Layout" })).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("restores_trigger_focus_when_a_dialog_closes_from_an_outside_pointer", () => {
    const { trigger } = openDialog();

    fireEvent.pointerDown(document.body);

    expect(screen.queryByRole("dialog", { name: "Layout" })).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("restores_trigger_focus_when_a_dialog_closes_from_its_trigger", () => {
    const { trigger } = openDialog();

    fireEvent.click(trigger);

    expect(screen.queryByRole("dialog", { name: "Layout" })).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("restores_trigger_focus_when_a_dialog_closes_from_a_sort_selection", () => {
    const { dialog, trigger } = openDialog();

    fireEvent.click(screen.getByRole("button", { name: "Sort by color" }));

    expect(dialog).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("does not leak menu-item pointer/click events to the ancestor that rendered it", () => {
    // The menu portals to <body>, but React synthetic events bubble through the
    // *component* tree — so without sealing them, an interaction inside the menu
    // reaches the host's handlers (in the stack, a card's long-press →
    // card preview). This reproduces that leak path with spy handlers on the
    // ancestor that wraps the menu.
    const ancestorPointerDown = vi.fn();
    const ancestorClick = vi.fn();

    render(
      <div onPointerDown={ancestorPointerDown} onClick={ancestorClick}>
        <PopoverMenu ariaLabel="Actions">
          {(close) => (
            <button type="button" role="menuitem" onClick={() => close()}>
              Do the thing
            </button>
          )}
        </PopoverMenu>
      </div>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Actions" }));
    const item = screen.getByRole("menuitem", { name: "Do the thing" });

    fireEvent.pointerDown(item);
    fireEvent.click(item);

    // The item's own onClick ran (menu closed), but neither event reached the
    // ancestor — the whole pointer/click family is sealed at the menu panel.
    expect(ancestorPointerDown).not.toHaveBeenCalled();
    expect(ancestorClick).not.toHaveBeenCalled();
    expect(screen.queryByRole("menuitem", { name: "Do the thing" })).not.toBeInTheDocument();
  });

  it("preserves the folder trigger across a transient item and confirmation", async () => {
    function FolderDeleteHarness() {
      const [confirmOpen, setConfirmOpen] = useState(false);
      return (
        <>
          <FolderActionsMenu
            onRename={vi.fn()}
            onDelete={() => setConfirmOpen(true)}
          />
          <ConfirmDialog
            open={confirmOpen}
            title="Delete folder?"
            message="The decks will remain available."
            confirmLabel="Delete"
            onConfirm={vi.fn()}
            onCancel={() => setConfirmOpen(false)}
          />
        </>
      );
    }

    render(<FolderDeleteHarness />);
    const trigger = screen.getByRole("button", { name: "Folder options" });
    fireEvent.click(trigger);
    const deleteItem = screen.getByRole("menuitem", { name: "Delete" });
    deleteItem.focus();
    fireEvent.click(deleteItem);

    const confirmation = screen.getByRole("alertdialog", {
      name: "Delete folder?",
    });
    const cancel = within(confirmation).getByRole("button", { name: "Cancel" });
    await waitFor(() => expect(cancel).toHaveFocus());
    fireEvent.keyDown(cancel, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog", { name: "Delete folder?" })).not.toBeInTheDocument(),
    );
    expect(trigger).toHaveFocus();
  });

  it("clamps_an_oversized_menu_to_viewport_edges_before_positioning_it", () => {
    const originalWidth = Object.getOwnPropertyDescriptor(window, "innerWidth");
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 120 });
    render(
      <PopoverMenu ariaLabel="Wide menu" menuWidthPx={224}>
        {() => <button type="button" role="menuitem">Action</button>}
      </PopoverMenu>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Wide menu" }));
    const menu = screen.getByRole("menu", { name: "Wide menu" });
    expect(menu).toHaveStyle({ left: "8px", width: "104px" });

    if (originalWidth === undefined) delete (window as { innerWidth?: number }).innerWidth;
    else Object.defineProperty(window, "innerWidth", originalWidth);
  });
});
