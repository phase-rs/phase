import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { StatusBanner } from "../StatusBanner";
import { fetchStatus, type StatusMessage } from "../../../services/status";
import { openExternal } from "../../../services/openExternal";
import { usePreferencesStore } from "../../../stores/preferencesStore";

// The banner is propless — it sources its message through the hook — so the
// service is the only injection point a test has. Everything else (the expiry
// predicate, the validator) stays real.
vi.mock("../../../services/status", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../../services/status")>()),
  fetchStatus: vi.fn(),
}));

// The link must route through openExternal, which is both the Tauri-webview
// opener and the http/https guard against a javascript: URL in a published
// payload. Mocking it is what lets the test assert the routing rather than the
// markup.
vi.mock("../../../services/openExternal", () => ({ openExternal: vi.fn() }));

const BASE: StatusMessage = {
  id: 1756482000000,
  severity: "info",
  title: "Multiplayer maintenance",
  body: "The lobby restarts at 20:00 UTC.",
  dismissible: true,
};

function publish(message: StatusMessage | null) {
  vi.mocked(fetchStatus).mockResolvedValue(message);
}

beforeEach(() => {
  localStorage.clear();
  usePreferencesStore.setState({ dismissedStatusId: undefined });
  publish(BASE);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("StatusBanner", () => {
  it("leaves the dismissal watermark unset by default", () => {
    // getInitialState(), not getState(): the beforeEach above writes its own
    // snapshot, which would make a getState() read vacuous.
    expect(usePreferencesStore.getInitialState().dismissedStatusId).toBeUndefined();
  });

  it.each([
    ["info", "status", "Notice"],
    ["warning", "status", "Heads up"],
    ["critical", "alert", "Important"],
  ] as const)("announces a %s message as role=%s", async (severity, role, eyebrow) => {
    publish({ ...BASE, severity });
    render(<StatusBanner />);

    const banner = await screen.findByRole(role);
    expect(banner).toHaveTextContent(eyebrow);
    expect(banner).toHaveTextContent("Multiplayer maintenance");
    expect(banner).toHaveTextContent("The lobby restarts at 20:00 UTC.");
  });

  it("hides the banner and records the id when the message is dismissed", async () => {
    const user = userEvent.setup();
    render(<StatusBanner />);

    await user.click(await screen.findByRole("button", { name: "Dismiss this message" }));

    await waitFor(() => expect(screen.queryByRole("status")).not.toBeInTheDocument());
    expect(usePreferencesStore.getState().dismissedStatusId).toBe(BASE.id);
  });

  it("offers no dismiss affordance when the author marked the message undismissible", async () => {
    publish({ ...BASE, dismissible: false });
    render(<StatusBanner />);

    await screen.findByRole("status");
    expect(screen.queryByRole("button", { name: "Dismiss this message" })).not.toBeInTheDocument();
  });

  it("stays hidden for a message the player already dismissed", async () => {
    usePreferencesStore.setState({ dismissedStatusId: BASE.id });
    render(<StatusBanner />);

    await waitFor(() => expect(fetchStatus).toHaveBeenCalled());
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("re-shows when a newer message is published after a dismissal", async () => {
    usePreferencesStore.setState({ dismissedStatusId: BASE.id });
    publish({ ...BASE, id: BASE.id + 1, title: "Lobby is back" });
    render(<StatusBanner />);

    expect(await screen.findByRole("status")).toHaveTextContent("Lobby is back");
  });

  it("renders nothing once the message has expired", async () => {
    publish({ ...BASE, expiresAt: new Date(Date.now() - 60_000).toISOString() });
    render(<StatusBanner />);

    await waitFor(() => expect(fetchStatus).toHaveBeenCalled());
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("opens an optional link through openExternal", async () => {
    publish({
      ...BASE,
      link: { url: "https://discord.gg/example", label: "Details on Discord" },
    });
    render(<StatusBanner />);

    await userEvent.click(await screen.findByRole("button", { name: "Details on Discord" }));
    expect(openExternal).toHaveBeenCalledWith("https://discord.gg/example");
  });

  it("renders no link affordance when the message carries none", async () => {
    publish(BASE);
    render(<StatusBanner />);

    await screen.findByRole("status");
    expect(screen.queryByRole("button", { name: "Details on Discord" })).not.toBeInTheDocument();
  });
});
