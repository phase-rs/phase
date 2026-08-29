import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { PopoverMenu } from "../PopoverMenu.tsx";

afterEach(() => {
  cleanup();
});

describe("PopoverMenu", () => {
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
