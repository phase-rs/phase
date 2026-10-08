import { describe, expect, it } from "vitest";
import { render, within } from "@testing-library/react";

import { ColorDistribution } from "../ColorDistribution";

const DISTRIBUTION = [
  { color: "White", count: 2, percentage: 33.25, display_percentage: 33 },
  { color: "Blue", count: 4, percentage: 66.75, display_percentage: 67 },
] as const;

describe("ColorDistribution", () => {
  it.each(["default", "compact"] as const)(
    "renders engine-authored percentages in %s presentation",
    (presentation) => {
      const { container } = render(
        <ColorDistribution
          distribution={DISTRIBUTION}
          presentation={presentation}
        />,
      );

      const distribution = container.querySelector("[data-color-distribution]");
      expect(distribution).toHaveAttribute(
        "data-color-distribution-presentation",
        presentation,
      );
      expect(within(distribution as HTMLElement).getByText("Colors")).toBeInTheDocument();
      expect(distribution).toHaveTextContent("W 33%");
      expect(distribution).toHaveTextContent("U 67%");
      expect(within(distribution as HTMLElement).getByTitle("W: 33%")).toHaveStyle({ width: "33.25%" });
      expect(within(distribution as HTMLElement).getByTitle("U: 67%")).toHaveStyle({ width: "66.75%" });
    },
  );

  it("renders nothing for an empty engine distribution", () => {
    const { container } = render(<ColorDistribution distribution={[]} />);
    expect(container.querySelector("[data-color-distribution]")).toBeNull();
  });
});