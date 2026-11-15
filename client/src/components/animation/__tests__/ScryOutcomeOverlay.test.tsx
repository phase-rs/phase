import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useUiStore } from "../../../stores/uiStore.ts";
import { ScryOutcomeOverlay } from "../ScryOutcomeOverlay.tsx";

beforeEach(() => {
  useUiStore.getState().resetScryOutcome();
});

afterEach(() => {
  cleanup();
  useUiStore.getState().resetScryOutcome();
});

describe("ScryOutcomeOverlay", () => {
  it("shows the public top and bottom placement outcome", () => {
    useUiStore.setState({ scryOutcome: { playerId: 1, topCount: 1, bottomCount: 2 } });

    render(<ScryOutcomeOverlay />);

    expect(screen.getByText("Scry complete")).toBeInTheDocument();
    expect(screen.getByTestId("scry-outcome")).toHaveTextContent("Opp 2 — 1 on top · 2 on bottom");
  });

  it("renders nothing when there is no completed scry outcome", () => {
    const { container } = render(<ScryOutcomeOverlay />);

    expect(container).toBeEmptyDOMElement();
  });
});
