import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { LoyaltyBadge } from "../LoyaltyBadge.tsx";

vi.mock("../../icons/ManaFontIcon.tsx", () => ({
  ManaFontIcon: ({
    fallbackText,
    iconClass,
    label,
    style,
  }: {
    fallbackText: string;
    iconClass: string;
    label?: string;
    style?: { filter?: string };
  }) => (
    <span
      data-testid="mana-font-icon"
      data-icon-class={iconClass}
      data-filter={style?.filter}
      role="img"
      aria-label={label}
    >
      {fallbackText}
    </span>
  ),
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("LoyaltyBadge", () => {
  it("uses the shared mana-font symbol for a supported loyalty cost", () => {
    render(<LoyaltyBadge amount={3} kind="cost" />);

    expect(screen.getByText("+3")).toBeInTheDocument();
    expect(screen.getAllByTestId("mana-font-icon")).toHaveLength(3);
    expect(screen.getAllByTestId("mana-font-icon")[0]).toHaveAttribute(
      "data-icon-class",
      "ms-loyalty-up",
    );
  });

  it("keeps the loyalty silhouette for a total without a mana-font numeral", () => {
    render(<LoyaltyBadge amount={26} kind="total" />);

    expect(screen.getByText("26")).toBeInTheDocument();
    expect(screen.getAllByTestId("mana-font-icon")).toHaveLength(3);
    expect(screen.getAllByTestId("mana-font-icon")[0]).toHaveAttribute(
      "data-icon-class",
      "ms-loyalty-start",
    );
  });

  it("reinforces only the top rim for compact art-crop badges", () => {
    render(<LoyaltyBadge amount={4} kind="total" reinforcedTopRim />);

    expect(screen.getAllByTestId("mana-font-icon")[0]).toHaveAttribute(
      "data-filter",
      "drop-shadow(-1px -1px 0 rgba(255,255,255,0.8)) drop-shadow(0 -1.25px 0 #e2e8f0) drop-shadow(1px 1px 1px rgba(15,23,42,0.98))",
    );
  });

  // CR 306.5c: a planeswalker's loyalty IS the number of loyalty counters on it, so an accepted
  // counter-growth loop on that counter makes the TOTAL unbounded and the badge renders ∞.
  it("renders ∞ instead of the finite amount for an unbounded loyalty TOTAL", () => {
    render(<LoyaltyBadge amount={4} kind="total" isUnbounded />);

    expect(screen.getByText("∞")).toBeInTheDocument();
    expect(screen.queryByText("4")).not.toBeInTheDocument();
    // Accessibility is preserved, not simplified away: `aria-label` follows the text.
    expect(screen.getByRole("img", { name: "∞" })).toBeInTheDocument();
    // …while the DOM attribute stays truthful, so existing selectors keep working.
    expect(screen.getByRole("img", { name: "∞" })).toHaveAttribute("data-loyalty-value", "4");
  });

  it("renders the finite total when the same badge is not marked unbounded (matched pair)", () => {
    render(<LoyaltyBadge amount={4} kind="total" />);

    expect(screen.getByText("4")).toBeInTheDocument();
    expect(screen.queryByText("∞")).not.toBeInTheDocument();
  });

  // CR 606.4: a loyalty ABILITY COST is a number of loyalty counters to put on or remove, shown
  // by the ability's loyalty symbol — a different game fact from the total, and never unbounded.
  // Rendering ∞ on an activation cost would be a rules-visible bug. This is the revert-probe for
  // the `kind === "total"` guard: delete it and this test reds.
  it("NEVER renders ∞ on a loyalty COST badge, even when isUnbounded is passed", () => {
    render(<LoyaltyBadge amount={4} kind="cost" isUnbounded />);

    expect(screen.getByText("+4")).toBeInTheDocument();
    expect(screen.queryByText("∞")).not.toBeInTheDocument();
  });

  it("NEVER renders ∞ on a NEGATIVE loyalty COST badge either", () => {
    render(<LoyaltyBadge amount={-4} kind="cost" isUnbounded />);

    expect(screen.getByText("−4")).toBeInTheDocument();
    expect(screen.queryByText("∞")).not.toBeInTheDocument();
  });
});
