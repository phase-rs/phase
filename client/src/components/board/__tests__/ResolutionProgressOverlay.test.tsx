import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore";
import { ResolutionProgressOverlay } from "../ResolutionProgressOverlay";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) =>
      key === "resolutionProgress.label"
        ? `Resolving ${opts?.resolved} / ${opts?.total}`
        : key,
  }),
}));

describe("ResolutionProgressOverlay", () => {
  beforeEach(() => {
    useGameStore.setState({ resolutionProgress: null });
  });

  afterEach(() => {
    cleanup();
    useGameStore.setState({ resolutionProgress: null });
  });

  it("renders the engine-provided resolved/total counts when a storm is in flight", async () => {
    useGameStore.setState({ resolutionProgress: { resolved: 80, total: 200 } });
    render(<ResolutionProgressOverlay />);
    expect(await screen.findByText("Resolving 80 / 200")).toBeInTheDocument();
  });

  it("renders nothing when no resolution is in flight", () => {
    useGameStore.setState({ resolutionProgress: null });
    render(<ResolutionProgressOverlay />);
    expect(screen.queryByText(/Resolving/)).not.toBeInTheDocument();
  });
});
