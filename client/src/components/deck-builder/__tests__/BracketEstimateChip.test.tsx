import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { BracketEstimateChip } from "../BracketEstimateChip";

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
