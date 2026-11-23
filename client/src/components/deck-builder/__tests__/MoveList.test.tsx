import { fireEvent, cleanup, render, screen } from "@testing-library/react";
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

  it("labels repeated names and dispatches controls with the clicked row", () => {
    const one = { count: 4, name: "Forest", sourcePrinting: { setCode: "one", collectorNumber: "276" } };
    const dmu = { count: 3, name: "Forest", sourcePrinting: { setCode: "dmu", collectorNumber: "277" } };
    const unprinted = { count: 1, name: "Forest" };
    const island = { count: 1, name: "Island", sourcePrinting: { setCode: "neo", collectorNumber: "295" } };
    const onIncrement = vi.fn();
    const onRemove = vi.fn();
    const onMove = vi.fn();
    const onSetAsCommander = vi.fn();
    const consoleError = vi.spyOn(console, "error");
    try {
      const { container } = render(
        <MoveList section="main" title="Main" entries={[one, dmu, unprinted, island]}
          onIncrement={onIncrement} onRemove={onRemove} onMove={onMove}
          onSetAsCommander={onSetAsCommander} isCommanderEligible={() => true} />,
      );
      expect(screen.getByText("ONE 276")).toBeInTheDocument();
      expect(screen.getByText("DMU 277")).toBeInTheDocument();
      expect(screen.queryByText("NEO 295")).not.toBeInTheDocument();
      expect(container.querySelectorAll('[data-card-name="forest"]')).toHaveLength(3);
      expect(consoleError.mock.calls.flat().join(" ")).not.toContain("same key");
      // Same-name rows name their printing in each control; the unprinted row does not.
      expect(screen.getByRole("button", { name: "Add one Forest" })).toBeInTheDocument();
      fireEvent.click(screen.getByRole("button", { name: "Add one Forest (DMU 277)" }));
      fireEvent.click(screen.getByRole("button", { name: "Remove one Forest (DMU 277)" }));
      fireEvent.click(screen.getByRole("button", { name: "Move one Forest (DMU 277) to sideboard" }));
      expect(onIncrement).toHaveBeenCalledWith(dmu, "main");
      expect(onRemove).toHaveBeenCalledWith(dmu, "main");
      expect(onMove).toHaveBeenCalledWith(dmu, "main");
      fireEvent.click(screen.getByRole("button", { name: "Make Forest (DMU 277) the commander" }));
      expect(onSetAsCommander).toHaveBeenCalledWith(dmu);
    } finally {
      consoleError.mockRestore();
    }
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
