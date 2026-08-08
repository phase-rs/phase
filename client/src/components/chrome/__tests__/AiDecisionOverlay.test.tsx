import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { AiDecisionDiagnosticReceipt } from "../../../adapter/types";
import { AiDecisionOverlay } from "../AiDecisionOverlay";

afterEach(cleanup);

const RANKED_RECEIPT: AiDecisionDiagnosticReceipt = {
  semanticOwner: 1,
  authorizedActor: 1,
  selectedAction: { type: "PassPriority" },
  status: "ranked",
  selectionExplanation: "Softmax sampled rank 2 (20.0%) instead of rank 1 (80.0%) at temperature 1.00.",
  samplingTemperature: 1,
  candidates: [
    {
      action: { type: "CancelCast" },
      objectName: null,
      details: [{ label: "Object ID", value: "7" }],
      rank: 1,
      isTopRanked: true,
      isSelected: false,
      score: 0.85,
      weight: 4.2,
      probability: 0.8,
    },
    {
      action: { type: "PassPriority" },
      objectName: null,
      details: [],
      rank: 2,
      isTopRanked: false,
      isSelected: true,
      score: 0.1,
      weight: 1.1,
      probability: 0.2,
    },
  ],
};

describe("AiDecisionOverlay", () => {
  it("renders the engine-ranked candidates as a color-coded probability chart", () => {
    render(<AiDecisionOverlay receipt={RANKED_RECEIPT} visible onClose={() => {}} />);

    expect(screen.getByLabelText("AI decision")).toBeInTheDocument();
    expect(screen.getByText("Cancel Cast")).toBeInTheDocument();
    expect(screen.getByText("Pass Priority")).toBeInTheDocument();
    expect(screen.getByText("TOP")).toBeInTheDocument();
    expect(screen.getByText("CHOSEN")).toBeInTheDocument();
    expect(screen.getByText("S +0.85 · W 4.2")).toBeInTheDocument();
    expect(screen.getByText("Object ID")).toBeInTheDocument();
    expect(screen.getByText("7")).toBeInTheDocument();
    expect(screen.getByText(/sampled rank 2/i)).toBeInTheDocument();
    expect(screen.getByText("80%")).toBeInTheDocument();
    expect(screen.getByText("20%")).toBeInTheDocument();
  });

  it("does not render while the visibility checkbox is off", () => {
    render(<AiDecisionOverlay receipt={RANKED_RECEIPT} visible={false} onClose={() => {}} />);

    expect(screen.queryByLabelText("AI decision")).not.toBeInTheDocument();
  });

  it("keeps direct-policy decisions legible without inventing rank data", () => {
    render(
      <AiDecisionOverlay
        visible
        onClose={() => {}}
        receipt={{
          ...RANKED_RECEIPT,
          status: "direct",
          candidates: [{
            ...RANKED_RECEIPT.candidates[1],
            rank: null,
            isSelected: true,
            probability: null,
          }],
        }}
      />,
    );

    expect(screen.getByText("Selected by a direct AI policy.")).toBeInTheDocument();
    expect(screen.getByText("CHOSEN")).toBeInTheDocument();
    expect(screen.queryByText("20%")).not.toBeInTheDocument();
  });

  it("collapses details while keeping the decision available to reopen", () => {
    render(<AiDecisionOverlay receipt={RANKED_RECEIPT} visible onClose={() => {}} />);

    fireEvent.click(screen.getByRole("button", { name: "Collapse AI decision details" }));

    expect(screen.getByLabelText("AI decision")).toBeInTheDocument();
    expect(screen.getByText("AI decision")).toBeInTheDocument();
    expect(screen.queryByText("Cancel Cast")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Expand AI decision details" })).toBeInTheDocument();
  });

  it("closes the overlay through its close control", () => {
    const onClose = vi.fn();
    render(<AiDecisionOverlay receipt={RANKED_RECEIPT} visible onClose={onClose} />);

    fireEvent.click(screen.getByRole("button", { name: "Close AI decision overlay" }));

    expect(onClose).toHaveBeenCalledOnce();
  });
});
