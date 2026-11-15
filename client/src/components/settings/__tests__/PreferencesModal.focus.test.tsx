import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore";
import { PreferencesModal } from "../PreferencesModal";

vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

vi.mock("../visual-packs/VisualPackManager.tsx", () => ({
  VisualPackManager: () => null,
}));

async function dismissConfirmation() {
  const dialog = await screen.findByRole("alertdialog");
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus(),
  );
  fireEvent.keyDown(dialog, { key: "Escape" });
  await waitFor(() =>
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
  );
}

describe("PreferencesModal focus restoration", () => {
  beforeEach(() => {
    usePreferencesStore.getState().resetAllPreferences();
  });

  afterEach(() => {
    cleanup();
    usePreferencesStore.getState().resetAllPreferences();
    vi.clearAllMocks();
  });

  it("returns an import confirmation to its visible launcher after file-picker focus loss", async () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="data" />);
    const exportButton = screen.getByRole("button", { name: "Export backup…" });
    const importButton = screen.getByRole("button", { name: "Import backup…" });
    const fileInput = document.querySelector<HTMLInputElement>('input[type="file"]');
    expect(fileInput).not.toBeNull();

    exportButton.focus();
    fireEvent.click(importButton);
    fireEvent.change(fileInput!, {
      target: { files: [new File(["{}"], "backup.json", { type: "application/json" })] },
    });
    await dismissConfirmation();

    expect(importButton).toHaveFocus();
  });

  it("returns Reset All to the exact launcher after a pointer-style open", async () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="gameplay" />);
    const trigger = screen.getAllByRole("button", {
      name: "Reset all preferences",
    })[0];
    screen.getByRole("button", { name: "Gameplay" }).focus();

    fireEvent.click(trigger);
    await dismissConfirmation();

    expect(trigger).toHaveFocus();
  });

  it("returns Clear Art Overrides to its launcher after a pointer-style open", async () => {
    usePreferencesStore.setState({
      artOverrides: {
        "oracle-id": {
          scryfallId: "printing-id",
          setCode: "tst",
          collectorNumber: "1",
        },
      },
    });
    render(<PreferencesModal onClose={vi.fn()} initialTab="visual" />);
    const trigger = screen.getByRole("button", {
      name: "Clear All Art Overrides (1)",
    });
    screen.getByRole("button", { name: "Visual" }).focus();

    fireEvent.click(trigger);
    await dismissConfirmation();

    expect(trigger).toHaveFocus();
  });

  it("hands focus to the durable Visual tab when clearing removes its launcher", async () => {
    usePreferencesStore.setState({
      artOverrides: {
        "oracle-id": {
          scryfallId: "printing-id",
          setCode: "tst",
          collectorNumber: "1",
        },
      },
    });
    render(<PreferencesModal onClose={vi.fn()} initialTab="visual" />);
    const visualTab = screen.getByRole("button", { name: "Visual" });
    fireEvent.click(screen.getByRole("button", {
      name: "Clear All Art Overrides (1)",
    }));

    fireEvent.click(await screen.findByRole("button", { name: "Clear" }));

    await waitFor(() =>
      expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument(),
    );
    expect(screen.queryByRole("button", {
      name: "Clear All Art Overrides (1)",
    })).not.toBeInTheDocument();
    expect(visualTab).toHaveFocus();
  });
});
