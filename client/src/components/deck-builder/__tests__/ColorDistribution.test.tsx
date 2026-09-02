import { describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";

import { ColorDistribution } from "../ColorDistribution";

describe("ColorDistribution", () => {
  it.each(["default", "compact"] as const)(
    "renders WUBRG percentages in %s presentation",
    (presentation) => {
      const { container } = render(
        <ColorDistribution
          colorValues={["WU", "WU", "R", ""]}
          presentation={presentation}
        />,
      );

      const distribution = container.querySelector("[data-color-distribution]");
      expect(distribution).toHaveAttribute(
        "data-color-distribution-presentation",
        presentation,
      );
      expect(within(distribution as HTMLElement).getByText("Colors")).toBeInTheDocument();
      expect(distribution).toHaveTextContent("W 40%");
      expect(distribution).toHaveTextContent("U 40%");
      expect(distribution).toHaveTextContent("R 20%");
    },
  );

  it("renders nothing for empty, colorless, and unknown-only inputs", () => {
    const { container, rerender } = render(<ColorDistribution colorValues={[]} />);
    expect(container.querySelector("[data-color-distribution]")).toBeNull();

    rerender(<ColorDistribution colorValues={["", "X"]} />);
    expect(container.querySelector("[data-color-distribution]")).toBeNull();
  });

  it("ignores unknown symbols without changing known-color percentages", () => {
    render(<ColorDistribution colorValues={["WX", "U"]} />);
    expect(screen.getByText("W 50%")).toBeInTheDocument();
    expect(screen.getByText("U 50%")).toBeInTheDocument();
    expect(screen.queryByText(/X \d+%/)).toBeNull();
  });
});