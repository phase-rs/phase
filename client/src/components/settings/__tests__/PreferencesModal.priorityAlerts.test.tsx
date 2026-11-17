import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore";
import { PreferencesModal } from "../PreferencesModal";

vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

const ALERTS = [
  { name: "Glow", field: "priorityAlertVisual" },
  { name: "Sound", field: "priorityAlertSound" },
  { name: "Tab title", field: "priorityAlertTitle" },
] as const;

describe("PreferencesModal priority alerts", () => {
  beforeEach(() => {
    usePreferencesStore.setState({
      priorityAlertVisual: true,
      priorityAlertSound: true,
      priorityAlertTitle: true,
    });
  });

  afterEach(() => cleanup());

  it("labels the group and explains when alerts fire", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="gameplay" />);

    expect(screen.getByText("Priority alerts")).toBeInTheDocument();
    expect(
      screen.getByText(
        /sound plays when you're in another tab or window, or when a window on an opponent's turn sits unanswered for 15 seconds; its glow then gets stronger too/i,
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Priority alerts" })).toBeInTheDocument();
  });

  it.each(ALERTS)("$name chip reflects the store and toggles $field", ({ name, field }) => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="gameplay" />);

    const chip = screen.getByRole("switch", { name });
    expect(chip).toHaveAttribute("aria-checked", "true");

    fireEvent.click(chip);
    expect(usePreferencesStore.getState()[field]).toBe(false);
    expect(chip).toHaveAttribute("aria-checked", "false");

    fireEvent.click(chip);
    expect(usePreferencesStore.getState()[field]).toBe(true);
    expect(chip).toHaveAttribute("aria-checked", "true");
  });

  it("toggles each alert independently", () => {
    usePreferencesStore.setState({ priorityAlertSound: false });
    render(<PreferencesModal onClose={vi.fn()} initialTab="gameplay" />);

    expect(screen.getByRole("switch", { name: "Glow" })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("switch", { name: "Sound" })).toHaveAttribute("aria-checked", "false");
    expect(screen.getByRole("switch", { name: "Tab title" })).toHaveAttribute("aria-checked", "true");

    fireEvent.click(screen.getByRole("switch", { name: "Glow" }));
    const state = usePreferencesStore.getState();
    expect(state.priorityAlertVisual).toBe(false);
    expect(state.priorityAlertSound).toBe(false);
    expect(state.priorityAlertTitle).toBe(true);
  });
});
