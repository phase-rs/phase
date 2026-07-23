import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore";
import { PreferencesModal } from "../PreferencesModal";

vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

describe("PreferencesModal card preview", () => {
  beforeEach(() => {
    usePreferencesStore.setState({ showCardPreviewFooter: true });
  });

  afterEach(() => cleanup());

  it("lets the player hide the informational preview footer", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="visual" />);

    const checkbox = screen.getByRole("checkbox", {
      name: /show information below card previews/i,
    });
    expect(checkbox).toBeChecked();

    fireEvent.click(checkbox);

    expect(usePreferencesStore.getState().showCardPreviewFooter).toBe(false);
  });
});
