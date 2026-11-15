import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore";
import { PreferencesModal } from "../PreferencesModal";

vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

describe("PreferencesModal card preview", () => {
  beforeEach(() => {
    usePreferencesStore.setState({
      draftCardPreviewMode: "none",
      draftDoubleClickConfirmPick: true,
      showCardPreviewFooter: true,
    });
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

  it("configures the draft-only hover preview beneath the general preview setting", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="visual" />);

    const generalGroup = screen.getByText("Card Hover Preview").parentElement;
    const draftGroup = screen.getByText("Draft Card Hover Preview").parentElement;
    expect(generalGroup?.compareDocumentPosition(draftGroup!)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
    expect(draftGroup).toHaveTextContent("Applies only while drafting and building your draft deck.");
    expect(within(draftGroup!).getByRole("button", { name: "Off" }))
      .toHaveClass("bg-sky-500/80");

    fireEvent.click(within(draftGroup!).getByRole("button", { name: "Dock to side" }));

    expect(usePreferencesStore.getState().draftCardPreviewMode).toBe("side");
  });

  it("configures enabled-by-default draft double-click confirmation beneath draft preview", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="visual" />);

    const previewGroup = screen.getByText("Draft Card Hover Preview").parentElement;
    const doubleClickGroup = screen.getByText("Draft Double-click Confirm Pick").parentElement;
    expect(previewGroup?.compareDocumentPosition(doubleClickGroup!)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
    expect(within(doubleClickGroup!).getByRole("button", { name: "Enabled" }))
      .toHaveClass("bg-sky-500/80");

    fireEvent.click(within(doubleClickGroup!).getByRole("button", { name: "Disabled" }));

    expect(usePreferencesStore.getState().draftDoubleClickConfirmPick).toBe(false);
  });
});
