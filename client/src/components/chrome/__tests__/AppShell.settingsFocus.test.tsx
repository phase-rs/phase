import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router";

import { AppShell } from "../AppShell";

vi.mock("../../../hooks/useChangelog", () => ({
  useChangelog: () => ({
    hasUnread: false,
    entries: [],
    loading: false,
    failed: false,
    openAndLoad: vi.fn(),
  }),
}));
vi.mock("../../menu/MenuParticles", () => ({ SceneParticles: () => null }));
vi.mock("../../modal/WhatsNewModal", () => ({ WhatsNewModal: () => null }));
vi.mock("../../settings/visual-packs/VisualPackManager.tsx", () => ({
  VisualPackManager: () => null,
}));
vi.mock("../AccountControl", () => ({ AccountControl: () => null }));
vi.mock("../BuildBadge", () => ({ BuildBadge: () => null }));
vi.mock("../CardDataLoadingBar", () => ({ CardDataLoadingBar: () => null }));
vi.mock("../FullscreenButton", () => ({ FullscreenButton: () => null }));
vi.mock("../SocialBar", () => ({ SocialBar: () => null }));
vi.mock("../TabBar", () => ({ TabBar: () => null }));
vi.mock("../VolumeControl", () => ({ VolumeControl: () => null }));

function renderShell() {
  render(
    <MemoryRouter initialEntries={["/decks"]}>
      <Routes>
        <Route element={<AppShell />}>
          <Route path="/decks" element={<div>Decks page</div>} />
        </Route>
      </Routes>
    </MemoryRouter>,
  );

  const navigation = screen.getByRole("navigation", {
    name: "Primary navigation",
  });
  const railSettings = within(navigation).getByRole("button", {
    name: "Settings",
  });
  const chromeSettings = screen
    .getAllByRole("button", { name: "Settings" })
    .find((button) => !navigation.contains(button));
  if (!chromeSettings) throw new Error("Chrome Settings launcher was not rendered");
  return { railSettings, chromeSettings };
}

async function closeSettings(): Promise<void> {
  const dialog = await screen.findByRole("dialog", { name: "Settings" });
  await waitFor(() => expect(dialog).toHaveFocus());
  fireEvent.keyDown(dialog, { key: "Escape" });
  await waitFor(() =>
    expect(screen.queryByRole("dialog", { name: "Settings" })).not.toBeInTheDocument(),
  );
}

describe("AppShell settings focus restoration", () => {
  afterEach(cleanup);

  it("returns the controlled modal to the Rail launcher", async () => {
    const { railSettings, chromeSettings } = renderShell();
    chromeSettings.focus();

    fireEvent.click(railSettings);
    await closeSettings();

    expect(railSettings).toHaveFocus();
  });

  it("returns the controlled modal to the Chrome launcher", async () => {
    const { railSettings, chromeSettings } = renderShell();
    railSettings.focus();

    fireEvent.click(chromeSettings);
    await closeSettings();

    expect(chromeSettings).toHaveFocus();
  });
});
