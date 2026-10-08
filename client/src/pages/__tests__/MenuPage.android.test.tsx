import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  desktop: vi.fn(),
  exit: vi.fn(),
  warm: vi.fn(() => Promise.resolve()),
}));

vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));
vi.mock("react-router", () => ({ useNavigate: () => vi.fn() }));
vi.mock("../../audio/useAudioContext", () => ({ useAudioContext: vi.fn() }));
vi.mock("../../components/chrome/PreviewBadge", () => ({ PreviewBadge: () => null }));
vi.mock("../../components/menu/LoadGameStateModal", () => ({ LoadGameStateModal: () => null }));
vi.mock("../../components/menu/home/HomeDashboard", () => ({ HomeDashboard: () => null }));
vi.mock("../../services/aiDeckCatalog", () => ({ buildLegalAiDeckCatalog: vi.fn() }));
vi.mock("../../services/platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../services/platform")>()),
  isDesktopTauri: mocks.desktop,
}));
vi.mock("../../stores/gameStore", () => ({
  saveActiveGame: vi.fn(),
  saveGame: vi.fn(),
  useGameStore: { setState: vi.fn() },
}));
vi.mock("../../stores/cardDataStore", () => {
  const useCardDataStore = Object.assign(
    (selector: (state: { status: string }) => unknown) => selector({ status: "idle" }),
    { getState: () => ({ warm: mocks.warm }) },
  );
  return { useCardDataStore };
});
vi.mock("../../stores/preferencesStore", () => ({
  usePreferencesStore: (selector: (state: { lastFormat: null; lastMatchType: string }) => unknown) =>
    selector({ lastFormat: null, lastMatchType: "Bo1" }),
}));
vi.mock("@tauri-apps/plugin-process", () => ({ exit: mocks.exit }));

import { MenuPage } from "../MenuPage";

beforeEach(() => {
  vi.clearAllMocks();
  mocks.desktop.mockReturnValue(false);
});

afterEach(cleanup);

it("renders and invokes process exit only on a proven desktop shell", async () => {
  const view = render(<MenuPage />);
  expect(screen.queryByRole("button", { name: "home.exit" })).not.toBeInTheDocument();

  mocks.desktop.mockReturnValue(true);
  view.rerender(<MenuPage />);
  fireEvent.click(screen.getByRole("button", { name: "home.exit" }));

  await waitFor(() => expect(mocks.exit).toHaveBeenCalledWith(0));
});
