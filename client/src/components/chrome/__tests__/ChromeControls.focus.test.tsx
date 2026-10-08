import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore";
import { ChromeControls } from "../ChromeControls";

vi.mock("../AccountControl", () => ({ AccountControl: () => null }));
vi.mock("../FullscreenButton", () => ({ FullscreenButton: () => null }));
vi.mock("../VolumeControl", () => ({ VolumeControl: () => null }));

describe("ChromeControls settings focus restoration", () => {
  beforeEach(() => {
    usePreferencesStore.getState().resetAllPreferences();
  });

  afterEach(() => {
    cleanup();
    usePreferencesStore.getState().resetAllPreferences();
  });

  it.each([
    ["language", /Language \(EN\) — open settings/],
    ["settings", "Settings"],
  ])("returns focus to the exact %s launcher after a pointer-style open", async (_name, launcherName) => {
    render(<ChromeControls hideVolume />);
    const languageLauncher = screen.getByRole("button", {
      name: /Language \(EN\) — open settings/,
    });
    const settingsLauncher = screen.getByRole("button", { name: "Settings" });
    const launcher = screen.getByRole("button", { name: launcherName });
    const otherLauncher = launcher === languageLauncher ? settingsLauncher : languageLauncher;

    otherLauncher.focus();
    fireEvent.click(launcher);

    const dialog = await screen.findByRole("dialog", { name: "Settings" });
    await waitFor(() => expect(dialog).toHaveFocus());
    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() => expect(dialog).not.toBeInTheDocument());
    expect(launcher).toHaveFocus();
  });
});
