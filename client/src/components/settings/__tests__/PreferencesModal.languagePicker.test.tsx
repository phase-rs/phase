import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SUPPORTED_LNGS } from "../../../i18n/resources";
import { usePreferencesStore } from "../../../stores/preferencesStore";
import { PreferencesModal } from "../PreferencesModal";

vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

/**
 * Every shipped locale must be selectable from the picker.
 *
 * Registering a locale in `SUPPORTED_LNGS` and shipping its catalogs is not the
 * same as letting a player choose it: the picker renders from its own
 * `LANGUAGE_OPTIONS` list, and a locale missing there is unreachable — the
 * translation exists, the tag resolves, and no user can ever select it. That is
 * exactly what happened when the Chinese catalogs were split away from the
 * component that lists them, and nothing caught it, because every other test
 * asserts over the catalogs and the normalization function rather than over the
 * picker.
 *
 * Counting against `SUPPORTED_LNGS` is the point: a hardcoded expectation would
 * need the same edit as the picker, so it would drift in lockstep and stay
 * silent.
 */
describe("PreferencesModal language picker", () => {
  beforeEach(() => {
    usePreferencesStore.setState({ language: "en" });
  });

  afterEach(() => cleanup());

  it("renders exactly one selectable entry per shipped locale", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="gameplay" />);

    // The language group is the only surface in this modal that renders
    // `aria-pressed` buttons, so the count is a direct read of the picker.
    const pickerButtons = screen
      .getAllByRole("button")
      .filter((button) => button.getAttribute("aria-pressed") !== null);

    expect(pickerButtons).toHaveLength(SUPPORTED_LNGS.length);
  });

  it("can actually select Simplified Chinese", () => {
    render(<PreferencesModal onClose={vi.fn()} initialTab="gameplay" />);

    // Presence alone is not the contract — selecting is what proves the entry is
    // wired to the store rather than merely rendered.
    const zhButton = screen.getByRole("button", { name: "简体中文" });

    fireEvent.click(zhButton);

    expect(usePreferencesStore.getState().language).toBe("zh");
    expect(zhButton).toHaveAttribute("aria-pressed", "true");
  });

  it("marks the active locale as pressed and the others as not", () => {
    usePreferencesStore.setState({ language: "zh" });
    render(<PreferencesModal onClose={vi.fn()} initialTab="gameplay" />);

    const pickerButtons = screen
      .getAllByRole("button")
      .filter((button) => button.getAttribute("aria-pressed") !== null);

    const pressed = pickerButtons.filter((b) => b.getAttribute("aria-pressed") === "true");
    expect(pressed).toHaveLength(1);
    expect(pressed[0]).toHaveAccessibleName("简体中文");
  });
});
