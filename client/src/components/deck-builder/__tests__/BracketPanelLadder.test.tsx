import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import type {
  BracketCard,
  BracketCut,
  BracketLadderStep,
  BracketReason,
  CommanderBracketTier,
} from "../../../types/bracket";
import { BracketPanelLadder } from "../BracketPanelLadder";

afterEach(cleanup);

const deck = (name: string): BracketCard => ({ name, role: "deck" });
const commander = (name: string): BracketCard => ({ name, role: "commander" });

function step(landsOn: CommanderBracketTier, cuts: BracketCut[]): BracketLadderStep {
  const reason = {
    floor: "upgraded",
    source: { kind: "combo_floor", trigger: { kind: "standalone_two_card" }, combos: [] },
    official_line: "line",
    source_document: "doc",
    source_published: "2026-01-01",
    source_url: "https://example.test",
    cuts,
  } as BracketReason;
  return { lands_on: landsOn, reasons: [reason] };
}

describe("BracketPanelLadder", () => {
  it("labels each step with the tier it lands on", () => {
    render(
      <BracketPanelLadder
        steps={[step("upgraded", []), step("core", [])]}
        onCardClick={vi.fn()}
      />,
    );
    const chips = screen.getAllByText(/^B\d$/).map((el) => el.textContent);
    expect(chips).toEqual(["B3", "B2"]);
  });

  it("says remove all when the cut takes every card", () => {
    const from = [deck("A"), deck("B"), deck("C")];
    render(
      <BracketPanelLadder
        steps={[step("core", [{ kind: "remove_cards", count: 3, from }])]}
        onCardClick={vi.fn()}
      />,
    );
    expect(screen.getByText(/Remove all 3:/)).toBeInTheDocument();
  });

  it("says remove N of when the cut takes some", () => {
    const from = ["A", "B", "C", "D", "E"].map(deck);
    render(
      <BracketPanelLadder
        steps={[step("core", [{ kind: "remove_cards", count: 2, from }])]}
        onCardClick={vi.fn()}
      />,
    );
    expect(screen.getByText(/Remove 2 of:/)).toBeInTheDocument();
  });

  it("breaks a combo by naming only deck pieces", () => {
    const cut: BracketCut = {
      kind: "break_combo",
      combo: {
        pieces: [commander("Kiki-Jiki"), deck("Land Feeder")],
        outcomes: [{ kind: "wins" }],
        assemble_cost: 4,
      },
      remove_one_of: [deck("Land Feeder")],
    };
    render(<BracketPanelLadder steps={[step("core", [cut])]} onCardClick={vi.fn()} />);
    expect(screen.getByText(/Break the combo/)).toBeInTheDocument();
    expect(screen.getByText(/Remove:/)).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Land Feeder" }).length).toBeGreaterThan(0);
  });

  it("asks for a different commander when partners form the combo", () => {
    const cut: BracketCut = {
      kind: "replace_commander",
      count: 1,
      of: [commander("Partner A"), commander("Partner B")],
    };
    render(<BracketPanelLadder steps={[step("core", [cut])]} onCardClick={vi.fn()} />);
    expect(screen.getByText(/Going lower also needs a different commander:/)).toBeInTheDocument();
    expect(screen.getAllByText("Commander")).toHaveLength(2);
    expect(screen.queryByText(/Remove one of:/)).toBeNull();
  });

  it("shows only the footnote when there are no steps", () => {
    const { container } = render(<BracketPanelLadder steps={[]} onCardClick={vi.fn()} />);
    expect(container.textContent).toBe("B2 is as low as the estimate goes; B1 depends on deck theme.");
  });

  it("renders twelve cards as twelve buttons", () => {
    const from = Array.from({ length: 12 }, (_, i) => deck(`Card ${i + 1}`));
    render(
      <BracketPanelLadder
        steps={[step("core", [{ kind: "remove_cards", count: 12, from }])]}
        onCardClick={vi.fn()}
      />,
    );
    for (const card of from) {
      expect(screen.getByRole("button", { name: card.name })).toBeInTheDocument();
    }
    expect(screen.getAllByRole("button")).toHaveLength(12);
  });
});
