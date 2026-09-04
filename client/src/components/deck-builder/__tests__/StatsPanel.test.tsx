import { describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/react";

import { StatsPanel } from "../StatsPanel";

describe("StatsPanel", () => {
  it("owns one mana curve followed by one color distribution", () => {
    const { container } = render(
      <StatsPanel
        compatibility={null}
        cmcValues={[1, 2]}
        colorDistribution={[
          { color: "White", count: 1, percentage: 50, display_percentage: 50 },
          { color: "Blue", count: 1, percentage: 50, display_percentage: 50 },
        ]}
        isCommander={false}
        estimate={null}
        manualBracket={null}
        onBracketChange={vi.fn()}
        onCardClick={vi.fn()}
      />,
    );

    const analysis = container.querySelector("[data-stats-panel-analysis]")!;
    expect(analysis.querySelectorAll("[data-mana-curve]")).toHaveLength(1);
    expect(analysis.querySelectorAll("[data-color-distribution]")).toHaveLength(1);
    const curve = analysis.querySelector("[data-mana-curve]")!;
    const colors = analysis.querySelector("[data-color-distribution]")!;
    expect(curve.compareDocumentPosition(colors) & Node.DOCUMENT_POSITION_FOLLOWING)
      .not.toBe(0);
    expect(curve.querySelector("[data-color-distribution]")).toBeNull();
  });
});