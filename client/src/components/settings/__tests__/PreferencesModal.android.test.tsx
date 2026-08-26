import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const { desktop } = vi.hoisted(() => ({ desktop: vi.fn() }));

vi.mock("../../../services/platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../services/platform")>()),
  isDesktopTauri: desktop,
}));
vi.mock("../../../services/backup", () => ({
  downloadBackup: vi.fn(),
  importBackupFromFile: vi.fn(),
}));

import { usePreferencesStore } from "../../../stores/preferencesStore";
import { PreferencesModal } from "../PreferencesModal";

beforeEach(() => {
  desktop.mockReturnValue(false);
  usePreferencesStore.setState({ nativeEngineEnabled: false });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("renders and updates the native-engine preference only on a proven desktop shell", () => {
  const view = render(<PreferencesModal onClose={vi.fn()} initialTab="gameplay" />);
  expect(screen.queryByRole("checkbox", { name: /native engine/i })).not.toBeInTheDocument();

  desktop.mockReturnValue(true);
  view.rerender(<PreferencesModal onClose={vi.fn()} initialTab="gameplay" />);
  const checkbox = screen.getByRole("checkbox", { name: /native engine/i });
  fireEvent.click(checkbox);

  expect(usePreferencesStore.getState().nativeEngineEnabled).toBe(true);
});
