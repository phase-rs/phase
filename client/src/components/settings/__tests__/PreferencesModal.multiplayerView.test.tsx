import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore";
import { PreferencesModal } from "../PreferencesModal";

vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

describe("PreferencesModal multiplayer view", () => {
  beforeEach(() => {
    usePreferencesStore.setState({ followActiveOpponent: false });
  });

  afterEach(() => cleanup());

  it("moves Follow active opponent into persistent multiplayer settings", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="multiplayer" />);

    const checkbox = screen.getByRole("checkbox", {
      name: /follow active opponent/i,
    });
    expect(checkbox).not.toBeChecked();
    expect(
      screen.getByText(/automatically focus the opponent whose turn it is/i),
    ).toBeInTheDocument();

    fireEvent.click(checkbox);

    expect(usePreferencesStore.getState().followActiveOpponent).toBe(true);
  });
});
