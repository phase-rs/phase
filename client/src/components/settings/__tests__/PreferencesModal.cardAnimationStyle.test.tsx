import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore";
import { PreferencesModal } from "../PreferencesModal";

vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

describe("PreferencesModal card animation style", () => {
  beforeEach(() => {
    usePreferencesStore.setState({
      cardAnimationStyle: "webgl",
      vfxQuality: "full",
    });
  });

  afterEach(() => cleanup());

  it("switches between the New and Classic card animations", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="visual" />);

    const group = screen.getByRole("radiogroup", { name: "Card Animations" });
    expect(within(group).getByRole("radio", { name: "New" })).toBeChecked();
    expect(within(group).getByRole("radio", { name: "Classic" })).not.toBeChecked();

    fireEvent.click(within(group).getByRole("radio", { name: "Classic" }));

    expect(usePreferencesStore.getState().cardAnimationStyle).toBe("classic");
    expect(usePreferencesStore.getState().vfxQuality).toBe("full");

    fireEvent.click(within(group).getByRole("radio", { name: "New" }));

    expect(usePreferencesStore.getState().cardAnimationStyle).toBe("webgl");
    expect(usePreferencesStore.getState().vfxQuality).toBe("full");
  });

  it("leaves the card animation style unchanged when VFX quality changes", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="visual" />);

    const vfxGroup = screen.getByText("VFX Quality").parentElement!;

    fireEvent.click(within(vfxGroup).getByRole("button", { name: "Minimal" }));

    expect(usePreferencesStore.getState().vfxQuality).toBe("minimal");
    expect(usePreferencesStore.getState().cardAnimationStyle).toBe("webgl");
  });
});
