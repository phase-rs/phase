import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { RuleSummaryLine } from "../../../adapter/types.ts";
import { HordeRulesBadge } from "../HudBadges.tsx";

// Engine-authored rules lines are rendered verbatim; the badge never derives
// them, so the test asserts the exact strings pass through untouched.
const RULES: RuleSummaryLine[] = [
  { label: "Waves", detail: "Reveals until the first nontoken card is cast" },
  { label: "Survivor life", detail: "100 life shared, -15 per extra survivor" },
];

describe("HordeRulesBadge", () => {
  afterEach(cleanup);

  it("renders an accessible info chip and keeps the popover closed until hovered", () => {
    render(<HordeRulesBadge deckLabel="Cyberman Horde" rules={RULES} />);

    expect(screen.getByRole("img", { name: "Horde deck rules" })).toBeInTheDocument();
    // Popover content is not mounted until the chip is hovered.
    expect(screen.queryByText("Cyberman Horde")).not.toBeInTheDocument();
    expect(
      screen.queryByText("Reveals until the first nontoken card is cast"),
    ).not.toBeInTheDocument();
  });

  it("opens a popover listing the deck label and every rule line on hover", () => {
    render(<HordeRulesBadge deckLabel="Cyberman Horde" rules={RULES} />);

    fireEvent.mouseEnter(screen.getByRole("img", { name: "Horde deck rules" }));

    // Deck heading (engine-provided label) plus each rule's label + verbatim detail.
    expect(screen.getByText("Cyberman Horde")).toBeInTheDocument();
    expect(screen.getByText("Waves")).toBeInTheDocument();
    expect(
      screen.getByText("Reveals until the first nontoken card is cast"),
    ).toBeInTheDocument();
    expect(screen.getByText("Survivor life")).toBeInTheDocument();
    expect(
      screen.getByText("100 life shared, -15 per extra survivor"),
    ).toBeInTheDocument();
  });

  it("renders nothing when there are no rules to show", () => {
    const { container } = render(<HordeRulesBadge deckLabel="Cyberman Horde" rules={[]} />);

    expect(container).toBeEmptyDOMElement();
    expect(screen.queryByRole("img", { name: "Horde deck rules" })).not.toBeInTheDocument();
  });
});
