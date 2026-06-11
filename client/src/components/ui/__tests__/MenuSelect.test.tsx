import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { MenuSelect } from "../MenuSelect";

afterEach(cleanup);

const items = [
  { value: "Mono Red", label: "Mono Red" },
  { value: "Azorius Control", label: "Azorius Control" },
];

function renderMenu(onSelect = vi.fn()) {
  render(<MenuSelect label="Load deck..." items={items} onSelect={onSelect} />);
  return onSelect;
}

describe("MenuSelect", () => {
  it("renders a closed trigger with no menu", () => {
    renderMenu();
    expect(screen.getByRole("button", { name: "Load deck..." })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("opens on click, lists every item, and focuses the first option", () => {
    renderMenu();
    fireEvent.click(screen.getByRole("button", { name: "Load deck..." }));
    expect(screen.getByRole("listbox")).toBeInTheDocument();
    const options = screen.getAllByRole("option");
    expect(options.map((o) => o.textContent)).toEqual(["Mono Red", "Azorius Control"]);
    expect(options[0]).toHaveFocus();
  });

  it("fires onSelect with the item value and closes", () => {
    const onSelect = renderMenu();
    fireEvent.click(screen.getByRole("button", { name: "Load deck..." }));
    fireEvent.click(screen.getByRole("option", { name: "Azorius Control" }));
    expect(onSelect).toHaveBeenCalledWith("Azorius Control");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("moves focus with arrow keys, wrapping at the ends", () => {
    renderMenu();
    fireEvent.click(screen.getByRole("button", { name: "Load deck..." }));
    const options = screen.getAllByRole("option");
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(options[1]).toHaveFocus();
    fireEvent.keyDown(window, { key: "ArrowDown" });
    expect(options[0]).toHaveFocus();
    fireEvent.keyDown(window, { key: "ArrowUp" });
    expect(options[1]).toHaveFocus();
  });

  it("closes on Escape and restores focus to the trigger", () => {
    renderMenu();
    const trigger = screen.getByRole("button", { name: "Load deck..." });
    fireEvent.click(trigger);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("does not open when disabled", () => {
    render(
      <MenuSelect label="Load deck..." items={items} onSelect={vi.fn()} disabled />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Load deck..." }));
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });
});
