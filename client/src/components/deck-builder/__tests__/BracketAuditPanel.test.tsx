import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { BracketAuditPanel } from "../BracketAuditPanel";
import type { BracketEstimate } from "../../../types/bracket";

afterEach(cleanup);

const estimate: BracketEstimate = {
  tier: "upgraded",
  axes: { game_changers: 2, mass_land_denial: 0, extra_turns: 1, efficient_tutors: 3 },
  axis_caps_at_tier: { game_changers: 3, mass_land_denial: 0, extra_turns: null, efficient_tutors: null },
  contributing: {
    game_changers: ["Smothering Tithe", "Cyclonic Rift"],
    mass_land_denial: [],
    extra_turns: ["Time Warp"],
    efficient_tutors: ["Demonic Tutor", "Vampiric Tutor", "Enlightened Tutor"],
  },
  violations: {},
  data_version: "2025-09-24-wotc",
};

describe("BracketAuditPanel", () => {
  it("renders the estimated tier chip", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={null} onCardClick={() => {}} />);
    expect(screen.getByText(/Estimated:/i)).toHaveTextContent("B3");
    expect(screen.getByText(/Upgraded/i)).toBeInTheDocument();
  });

  it("hides the mismatch chip when manual matches estimate", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={3} onCardClick={() => {}} />);
    expect(screen.queryByText(/mismatch/i)).not.toBeInTheDocument();
  });

  it("shows the mismatch chip when manual differs from estimate", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={2} onCardClick={() => {}} />);
    expect(screen.getByText(/mismatch/i)).toBeInTheDocument();
  });

  it("expands to show per-axis breakdown", () => {
    render(<BracketAuditPanel estimate={estimate} manualBracket={null} onCardClick={() => {}} />);
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    expect(screen.getByText(/Game Changers/i)).toBeInTheDocument();
    expect(screen.getByText("Smothering Tithe")).toBeInTheDocument();
    expect(screen.getByText("Cyclonic Rift")).toBeInTheDocument();
    expect(screen.getByText(/2025-09-24-wotc/)).toBeInTheDocument();
  });

  it("fires onCardClick when a contributing card is clicked", () => {
    const onCardClick = vi.fn();
    render(<BracketAuditPanel estimate={estimate} manualBracket={null} onCardClick={onCardClick} />);
    fireEvent.click(screen.getByRole("button", { name: /breakdown/i }));
    fireEvent.click(screen.getByText("Smothering Tithe"));
    expect(onCardClick).toHaveBeenCalledWith("Smothering Tithe");
  });

  it("renders an empty-state placeholder when estimate is null and format is Commander", () => {
    render(
      <BracketAuditPanel
        estimate={null}
        manualBracket={null}
        onCardClick={() => {}}
        emptyReason="no-commander"
      />,
    );
    expect(screen.getByText(/Add a commander/i)).toBeInTheDocument();
  });

  it("renders nothing for non-Commander formats", () => {
    const { container } = render(
      <BracketAuditPanel
        estimate={null}
        manualBracket={null}
        onCardClick={() => {}}
        emptyReason="not-commander"
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});
