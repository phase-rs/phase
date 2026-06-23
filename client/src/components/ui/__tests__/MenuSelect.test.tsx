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

  it("uses an anchored dropdown when menuLayout is dropdown, even on mobile", () => {
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: query === "(max-width: 819px)",
      media: query,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    }));

    render(
      <MenuSelect
        label="All types"
        items={items}
        onSelect={vi.fn()}
        menuLayout="dropdown"
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "All types" }));

    const listbox = screen.getByRole("listbox");
    expect(listbox).toBeInTheDocument();
    expect(listbox.className).toContain("rounded-xl");
    expect(listbox.className).not.toContain("rounded-t-2xl");
    expect(screen.queryByRole("button", { name: "All types" })).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "All types" })).toHaveLength(1);

    vi.unstubAllGlobals();
  });

  it("anchors the dropdown to the trigger box when opened", () => {
    const rect = {
      left: 96,
      right: 296,
      top: 48,
      bottom: 88,
      width: 200,
      height: 40,
      x: 96,
      y: 48,
      toJSON: () => ({}),
    };
    const spy = vi
      .spyOn(HTMLElement.prototype, "getBoundingClientRect")
      .mockReturnValue(rect as DOMRect);

    render(
      <MenuSelect
        label="Standard"
        items={[{ value: "Standard", label: "Standard" }]}
        onSelect={vi.fn()}
        fitContainer
        wrapperClassName="w-full min-w-0"
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Standard" }));

    const listbox = screen.getByRole("listbox");
    expect(listbox).toHaveStyle({ left: "96px", width: "200px" });

    spy.mockRestore();
  });

  it("filters options as the user types and focuses the search box on open", () => {
    render(
      <MenuSelect
        label="Load deck..."
        items={items}
        onSelect={vi.fn()}
        filterable
        filterPlaceholder="Search decks…"
        noMatchesLabel="No decks match"
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Load deck..." }));

    const search = screen.getByPlaceholderText("Search decks…");
    expect(search).toHaveFocus();
    expect(screen.getAllByRole("option")).toHaveLength(2);

    fireEvent.change(search, { target: { value: "azor" } });
    const filtered = screen.getAllByRole("option");
    expect(filtered.map((o) => o.textContent)).toEqual(["Azorius Control"]);

    fireEvent.change(search, { target: { value: "zzz" } });
    expect(screen.queryAllByRole("option")).toHaveLength(0);
    expect(screen.getByText("No decks match")).toBeInTheDocument();
  });

  it("filters grouped options and drops groups with no matches", () => {
    render(
      <MenuSelect
        label="Switch deck"
        groups={[
          { label: "Starred", items: [{ value: "Atraxa", label: "Atraxa" }] },
          { label: "Aggro", items: [{ value: "Mono Red", label: "Mono Red" }] },
        ]}
        onSelect={vi.fn()}
        filterable
        filterPlaceholder="Search decks…"
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Switch deck" }));
    fireEvent.change(screen.getByPlaceholderText("Search decks…"), {
      target: { value: "atra" },
    });

    expect(screen.getByText("Starred")).toBeInTheDocument();
    expect(screen.queryByText("Aggro")).not.toBeInTheDocument();
    expect(screen.getAllByRole("option").map((o) => o.textContent)).toEqual(["Atraxa"]);
  });

  it("does not apply content-based minWidth when fitContainer is set", () => {
    const longLabel = "Option ".repeat(12).trimEnd();
    const items = [{ value: "option-a", label: longLabel }];

    const { container: fitContainerRoot } = render(
      <MenuSelect
        label={longLabel}
        items={items}
        onSelect={vi.fn()}
        fitContainer
        wrapperClassName="w-full min-w-0"
      />,
    );

    const { container: contentSizedRoot } = render(
      <MenuSelect label={longLabel} items={items} onSelect={vi.fn()} />,
    );

    const fitWrapper = fitContainerRoot.firstElementChild as HTMLElement;
    const contentWrapper = contentSizedRoot.firstElementChild as HTMLElement;

    expect(fitWrapper.style.minWidth).toBe("");
    expect(contentWrapper.style.minWidth).not.toBe("");
  });
});
