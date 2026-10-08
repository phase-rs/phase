import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  bundled: vi.fn(),
  desktop: vi.fn(),
  getVersion: vi.fn(),
  marker: vi.fn(),
  progress: 0,
  serviceWorker: vi.fn(),
  status: "idle",
  tauri: vi.fn(),
  tauriUpdate: vi.fn(),
  updateError: null as string | null,
}));

vi.mock("../../../services/platform", () => ({
  isBundledTauriOrigin: mocks.bundled,
  isDesktopTauri: mocks.desktop,
  isTauri: mocks.tauri,
}));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: mocks.getVersion }));
vi.mock("../../../pwa/registerServiceWorker", () => ({
  checkForServiceWorkerUpdate: mocks.serviceWorker,
}));
vi.mock("../../../pwa/tauriUpdater", () => ({ checkForTauriUpdate: mocks.tauriUpdate }));
vi.mock("../../../hooks/useCardDataMeta", () => ({
  useCardDataMeta: () => null,
  formatRelativeDate: () => "today",
}));
vi.mock("../../../pwa/updateMarker", () => ({ consumeRecentAutoUpdateMarker: mocks.marker }));
vi.mock("../../../pwa/updateStatus", () => ({
  useUpdateStatus: () => mocks.status,
  useDownloadProgress: () => mocks.progress,
  useUpdateError: () => mocks.updateError,
  getUpdateDebugReport: () => "debug report",
}));
vi.mock("react-i18next", () => ({ useTranslation: () => ({ t: (key: string) => key }) }));

import { BuildBadge } from "../BuildBadge";

const mobileLayouts = [
  { bundled: true, compact: true },
  { bundled: true, compact: false },
  { bundled: false, compact: true },
  { bundled: false, compact: false },
] as const;

beforeEach(() => {
  vi.clearAllMocks();
  mocks.tauri.mockReturnValue(true);
  mocks.desktop.mockReturnValue(false);
  mocks.bundled.mockReturnValue(true);
  mocks.marker.mockReturnValue(false);
  mocks.progress = 0;
  mocks.status = "idle";
  mocks.updateError = null;
});

afterEach(cleanup);

it.each(mobileLayouts)("keeps mobile Tauri metadata but hides idle updater affordances ($bundled, $compact)", ({ bundled, compact }) => {
  mocks.bundled.mockReturnValue(bundled);
  render(<BuildBadge compact={compact} inline />);
  expect(screen.getByText(`v${__APP_VERSION__}`)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "buildBadge.checkForUpdates" })).not.toBeInTheDocument();
  expect(document.querySelector('[title*="buildBadge.checkForUpdates"]')).not.toBeInTheDocument();
  expect(mocks.tauriUpdate).not.toHaveBeenCalled();
  expect(mocks.serviceWorker).not.toHaveBeenCalled();
});

it.each(mobileLayouts)("hides mobile Tauri download status and progress ($bundled, $compact)", ({ bundled, compact }) => {
  mocks.bundled.mockReturnValue(bundled);
  mocks.status = "downloading";
  mocks.progress = 37;
  render(<BuildBadge compact={compact} inline />);
  expect(screen.queryByText("buildBadge.downloading")).not.toBeInTheDocument();
  expect(document.querySelector('[style="width: 37%;"]')).not.toBeInTheDocument();
});

it.each(mobileLayouts)("hides mobile Tauri updater errors and debug controls ($bundled, $compact)", ({ bundled, compact }) => {
  mocks.bundled.mockReturnValue(bundled);
  mocks.updateError = "network unavailable";
  render(<BuildBadge compact={compact} inline />);
  expect(screen.queryByText("buildBadge.updateIssue")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "buildBadge.updaterDebugInfo" })).not.toBeInTheDocument();
});

it.each(mobileLayouts)("hides the recent-update marker on mobile Tauri ($bundled, $compact)", ({ bundled, compact }) => {
  mocks.bundled.mockReturnValue(bundled);
  mocks.marker.mockReturnValue(true);
  render(<BuildBadge compact={compact} inline />);
  expect(screen.queryByText("buildBadge.updated")).not.toBeInTheDocument();
});

it("retains browser service-worker update dispatch", () => {
  mocks.tauri.mockReturnValue(false);
  render(<BuildBadge compact />);
  fireEvent.click(screen.getByRole("button", { name: "buildBadge.checkForUpdates" }));
  expect(mocks.tauriUpdate).not.toHaveBeenCalled();
  expect(mocks.serviceWorker).toHaveBeenCalledOnce();
});

it("retains desktop app-version and updater reachability", async () => {
  mocks.desktop.mockReturnValue(true);
  mocks.bundled.mockReturnValue(false);
  mocks.getVersion.mockResolvedValue("0.60.0");
  render(<BuildBadge compact />);
  await waitFor(() => expect(mocks.getVersion).toHaveBeenCalledOnce());
  fireEvent.click(screen.getByRole("button", { name: "buildBadge.checkForUpdates" }));
  expect(mocks.tauriUpdate).toHaveBeenCalledOnce();
  expect(mocks.serviceWorker).toHaveBeenCalledOnce();
});
