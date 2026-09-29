import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { BracketEstimateChip, ManualBracketChip } from "../BracketEstimateChip";

afterEach(cleanup);

describe("BracketEstimateChip", () => {
  it("renders 'Estimated: B3' for an upgraded tier", () => {
    render(<BracketEstimateChip tier="upgraded" />);
    expect(screen.getByText(/Estimated:/i)).toHaveTextContent("B3");
  });

  it("renders nothing when tier is null", () => {
    const { container } = render(<BracketEstimateChip tier={null} />);
    expect(container).toBeEmptyDOMElement();
  });
});

describe("ManualBracketChip", () => {
  it("renders the bare tier without an 'Estimated:' prefix", () => {
    render(<ManualBracketChip bracket={2} />);
    expect(screen.getByText("B2")).toBeInTheDocument();
    expect(screen.queryByText(/Estimated:/i)).not.toBeInTheDocument();
  });

  it("exposes the declared bracket as its accessible label", () => {
    render(<ManualBracketChip bracket={5} />);
    expect(screen.getByLabelText("Declared bracket: B5 cEDH")).toHaveTextContent("B5");
  });
});
