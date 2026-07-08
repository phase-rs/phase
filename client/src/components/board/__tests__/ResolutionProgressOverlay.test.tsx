import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore";
import { ResolutionProgressOverlay } from "../ResolutionProgressOverlay";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, opts?: Record<string, unknown>) =>
      key === "resolutionProgress.label"
        ? `Resolving ${opts?.resolved} / ${opts?.total}`
        : key === "resolutionProgress.collapse"
          ? "Collapse resolving progress"
          : key === "resolutionProgress.expand"
            ? "Expand resolving progress"
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

  it("collapses and expands the progress display without clearing progress", async () => {
    useGameStore.setState({ resolutionProgress: { resolved: 50, total: 19192 } });
    render(<ResolutionProgressOverlay />);

    fireEvent.click(await screen.findByRole("button", { name: "Collapse resolving progress" }));
    expect(await screen.findByRole("button", { name: "Expand resolving progress" })).toBeInTheDocument();
    expect(useGameStore.getState().resolutionProgress).toEqual({ resolved: 50, total: 19192 });

    fireEvent.click(screen.getByRole("button", { name: "Expand resolving progress" }));
    expect(await screen.findByRole("button", { name: "Collapse resolving progress" })).toBeInTheDocument();
  });
});
