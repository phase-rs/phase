import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore";
import { PreferencesModal } from "../PreferencesModal";

vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

describe("PreferencesModal priority passing", () => {
  beforeEach(() => {
    usePreferencesStore.setState({
      priorityPassingMode: "Standard",
      experimentalTournamentsEnabled: false,
    });
  });

  afterEach(() => cleanup());

  it("offers an opt-in checkbox for skipping low-use priority windows", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="gameplay" />);

    expect(screen.getByText("Auto-Pass")).toBeInTheDocument();
    expect(
      screen.getByText(/automatically pass your own empty-stack upkeep, draw, and end-step/i),
    ).toBeInTheDocument();
    const checkbox = screen.getByRole("checkbox", {
      name: /skip low-use priority windows/i,
    });
    expect(checkbox).not.toBeChecked();

    fireEvent.click(checkbox);
    expect(usePreferencesStore.getState().priorityPassingMode).toBe("SkipLowUseWindows");

    fireEvent.click(checkbox);
    expect(usePreferencesStore.getState().priorityPassingMode).toBe("Standard");
  });

  it("renders the command-zone layout label from the settings catalog", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="gameplay" />);

    expect(screen.getByText("Command Zone")).toBeInTheDocument();
    expect(screen.queryByText("GAMEPLAY.COMMANDZONE")).not.toBeInTheDocument();
  });

  it("offers an opt-in toggle for tournament navigation", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="experimental" />);

    const checkbox = screen.getByRole("checkbox", {
      name: /show tournaments in navigation/i,
    });
    expect(checkbox).not.toBeChecked();

    fireEvent.click(checkbox);
    expect(usePreferencesStore.getState().experimentalTournamentsEnabled).toBe(true);
  });
});
