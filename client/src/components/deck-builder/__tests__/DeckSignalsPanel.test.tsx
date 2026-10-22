import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { DeckSignals } from "../../../types/deckSignals";
import { DeckSignalsPanel } from "../DeckSignalsPanel";

afterEach(cleanup);

function buildSignals(overrides: Partial<DeckSignals> = {}): DeckSignals {
  return {
    readings: {
      counterspells: { count: 1, contributing: ["Counterspell"] },
      spot_removal: { count: 0, contributing: [] },
      sweepers: { count: 0, contributing: [] },
      card_advantage: { count: 0, contributing: [] },
      free_interaction: { count: 0, contributing: [] },
      mana_producers: { count: 0, contributing: [] },
      land_fetch: { count: 0, contributing: [] },
      rituals: { count: 0, contributing: [] },
      extra_land_drops: { count: 0, contributing: [] },
    },
    nonland_cards: 1,
    average_mana_value_centi: null,
    resolved_cards: 1,
    unresolved_cards: 0,
    ...overrides,
  };
}

describe("DeckSignalsPanel", () => {
  it('renders "not measurable" for a null average and never 0.00', () => {
    render(<DeckSignalsPanel signals={buildSignals()} />);
    expect(screen.getByText(/Average mana value \(nonland\): not measurable/)).toBeInTheDocument();
    expect(screen.queryByText(/0\.00/)).not.toBeInTheDocument();
  });

  it("renders a zero row for a non-firing kind", () => {
    render(<DeckSignalsPanel signals={buildSignals()} />);
    const row = screen.getByText("Spot removal").closest("div");
    expect(row).toHaveTextContent("Spot removal0");
  });

  it("renders the disclaimer above the rows", () => {
    render(<DeckSignalsPanel signals={buildSignals()} />);
    const disclaimer = screen.getByText(/Descriptive statistics from the deck's parsed card text/);
    const firstRow = screen.getByText("Counterspells");
    expect(disclaimer.compareDocumentPosition(firstRow) & Node.DOCUMENT_POSITION_FOLLOWING)
      .not.toBe(0);
  });

  it("shows the unresolved line only when unresolved_cards is positive", () => {
    const { rerender } = render(<DeckSignalsPanel signals={buildSignals()} />);
    expect(screen.queryByText(/card names were not found/)).not.toBeInTheDocument();

    rerender(<DeckSignalsPanel signals={buildSignals({ unresolved_cards: 2 })} />);
    expect(screen.getByText("2 card names were not found in the card data.")).toBeInTheDocument();
  });
});
