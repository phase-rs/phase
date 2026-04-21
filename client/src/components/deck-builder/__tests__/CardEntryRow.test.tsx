import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CardEntryRow } from "../CardEntryRow";

afterEach(cleanup);

const entry = { count: 3, name: "Lightning Bolt" };

describe("CardEntryRow", () => {
  it("renders entry name and count", () => {
    render(
      <CardEntryRow entry={entry} section="main" onMove={vi.fn()} onRemove={vi.fn()} />,
    );
    expect(screen.getByText("Lightning Bolt")).toBeInTheDocument();
    expect(screen.getByText("3x")).toBeInTheDocument();
  });

  it("fires onMove with (name, section) when the arrow button is clicked", () => {
    const onMove = vi.fn();
    render(
      <CardEntryRow entry={entry} section="main" onMove={onMove} onRemove={vi.fn()} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /move one lightning bolt to sideboard/i }));
    expect(onMove).toHaveBeenCalledWith("Lightning Bolt", "main");
  });

  it("shows an arrow pointing the opposite direction for sideboard rows", () => {
    const onMove = vi.fn();
    render(
      <CardEntryRow entry={entry} section="sideboard" onMove={onMove} onRemove={vi.fn()} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /move one lightning bolt to main deck/i }));
    expect(onMove).toHaveBeenCalledWith("Lightning Bolt", "sideboard");
  });

  it("fires onRemove with (name, section) when the remove button is clicked", () => {
    const onRemove = vi.fn();
    render(
      <CardEntryRow entry={entry} section="main" onMove={vi.fn()} onRemove={onRemove} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /remove one lightning bolt/i }));
    expect(onRemove).toHaveBeenCalledWith("Lightning Bolt", "main");
  });

  it("omits the remove button when onRemove is not provided (partition-only mode)", () => {
    render(<CardEntryRow entry={entry} section="main" onMove={vi.fn()} />);
    expect(
      screen.queryByRole("button", { name: /remove one lightning bolt/i }),
    ).not.toBeInTheDocument();
    // Move button still present so the row is functional in partition mode.
    expect(
      screen.getByRole("button", { name: /move one lightning bolt to sideboard/i }),
    ).toBeInTheDocument();
  });

  it("renders an unsupported-mechanics badge with expandable details", () => {
    render(
      <CardEntryRow
        entry={entry}
        section="main"
        onMove={vi.fn()}
        onRemove={vi.fn()}
        unsupported={{
          name: "Lightning Bolt",
          gaps: ["DealDamage.PlayerOnly"],
          copies: 3,
          oracle_text: "Lightning Bolt deals 3 damage to any target.",
          parse_details: [],
        }}
      />,
    );
    const badge = screen.getByRole("button", { name: /1 unsupported mechanic/i });
    expect(badge).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(badge);
    expect(badge).toHaveAttribute("aria-expanded", "true");
    expect(
      screen.getByText("Lightning Bolt deals 3 damage to any target."),
    ).toBeInTheDocument();
  });
});
