import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { MoveList } from "../MoveList";

afterEach(cleanup);

const entries = [
  { count: 4, name: "Lightning Bolt" },
  { count: 2, name: "Counterspell" },
];

describe("MoveList", () => {
  it("renders entries as CardEntryRows", () => {
    render(
      <MoveList
        section="main"
        title="Main"
        entries={entries}
        onMove={vi.fn()}
        onRemove={vi.fn()}
      />,
    );
    expect(screen.getByText("Lightning Bolt")).toBeInTheDocument();
    expect(screen.getByText("Counterspell")).toBeInTheDocument();
  });

  it("does not render when entries is empty and alwaysShow is false", () => {
    const { container } = render(
      <MoveList section="sideboard" title="Sideboard" entries={[]} onMove={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders an empty-state hint when entries is empty and alwaysShow is true", () => {
    render(
      <MoveList
        section="sideboard"
        title="Sideboard"
        entries={[]}
        onMove={vi.fn()}
        alwaysShow
        emptyHint="Hover a main-deck card and click → to move it here."
      />,
    );
    expect(
      screen.getByText("Hover a main-deck card and click → to move it here."),
    ).toBeInTheDocument();
  });

  it("renders a warning banner when warning prop is provided", () => {
    render(
      <MoveList
        section="sideboard"
        title="Sideboard"
        entries={entries}
        onMove={vi.fn()}
        warning="Sideboard exceeds 15-card limit"
      />,
    );
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("Sideboard exceeds 15-card limit");
  });

  it("displays the combined total card count next to the title", () => {
    render(
      <MoveList section="main" title="Main" entries={entries} onMove={vi.fn()} />,
    );
    // 4 + 2 = 6
    expect(screen.getByText("(6)")).toBeInTheDocument();
  });

  it("propagates onRemove absence to rows (partition-only mode)", () => {
    render(
      <MoveList section="main" title="Main" entries={entries} onMove={vi.fn()} />,
    );
    // No remove buttons rendered for any row.
    expect(
      screen.queryByRole("button", { name: /remove one lightning bolt/i }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /remove one counterspell/i }),
    ).not.toBeInTheDocument();
  });
});
