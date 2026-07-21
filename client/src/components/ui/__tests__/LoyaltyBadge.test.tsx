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
});
