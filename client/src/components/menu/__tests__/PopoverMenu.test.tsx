import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { PopoverMenu } from "../PopoverMenu.tsx";

afterEach(() => {
  cleanup();
});

describe("PopoverMenu", () => {
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
});
