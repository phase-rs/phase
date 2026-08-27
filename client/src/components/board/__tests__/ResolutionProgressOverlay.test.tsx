import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore";
import { ResolutionProgressOverlay } from "../ResolutionProgressOverlay";

vi.mock("react-i18next", () => ({
  initReactI18next: { type: "3rdParty", init: () => {} },
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) =>
      key === "restoredAutomation.progressed.title"
        ? "Stack automation resumed"
        : key === "restoredAutomation.progressed.summary"
          ? `Resolved ${opts?.count};`
          : key === "restoredAutomation.progressed.omitted"
            ? `omitted ${opts?.count}`
            : key === "restoredAutomation.dismiss"
            ? "Dismiss"
            : key,
  }),
}));

describe("ResolutionProgressOverlay", () => {
  beforeEach(() => useGameStore.setState({ restoredStackAutomation: null }));

  afterEach(() => {
    cleanup();
    useGameStore.setState({ restoredStackAutomation: null });
  });

  it("renders the exact engine-authored restored automation summary", async () => {
    useGameStore.setState({
      restoredStackAutomation: {
        outcome: "progressed",
        automatedResolutionCount: 80,
        omittedEventCount: 200,
        logEntries: [],
      },
    });
    render(<ResolutionProgressOverlay />);

    expect(await screen.findByText("Stack automation resumed")).toBeInTheDocument();
    expect(screen.getByText("Resolved 80; omitted 200")).toBeInTheDocument();
  });

  it("dismisses the one-shot presentation without dispatching an engine action", async () => {
    useGameStore.setState({
      restoredStackAutomation: {
        outcome: "progressed",
        automatedResolutionCount: 1,
        omittedEventCount: 2,
        logEntries: [],
      },
    });
    render(<ResolutionProgressOverlay />);

    fireEvent.click(await screen.findByRole("button", { name: "Dismiss" }));

    expect(useGameStore.getState().restoredStackAutomation).toBeNull();
  });
});
