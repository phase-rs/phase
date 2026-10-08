import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { FEED_ERROR_KEYS } from "../../../services/feedService";
import { useConnectivityStore } from "../../../stores/connectivityStore";
import { FeedManagerModal } from "../FeedManagerModal";

const mocks = vi.hoisted(() => ({
  subscriptions: [] as Array<{
    sourceId: string;
    url: string;
    type: "bundled" | "remote";
    subscribedAt: number;
    lastRefreshedAt: number;
    lastVersion: number;
  }>,
  subscribe: vi.fn(),
  unsubscribe: vi.fn(),
  refreshFeed: vi.fn(),
  refreshAllFeeds: vi.fn(),
}));

vi.mock("../../../services/feedService", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../../services/feedService")>(),
  listSubscriptions: () => mocks.subscriptions,
  subscribe: mocks.subscribe,
  unsubscribe: mocks.unsubscribe,
  refreshFeed: mocks.refreshFeed,
  refreshAllFeeds: mocks.refreshAllFeeds,
}));

describe("FeedManagerModal offline actions", () => {
  beforeEach(() => {
    mocks.subscriptions = [{
      sourceId: "starter-decks",
      url: "/feeds/starter-decks.json",
      type: "bundled",
      subscribedAt: 1,
      lastRefreshedAt: 1,
      lastVersion: 1,
    }];
    mocks.subscribe.mockReset();
    mocks.unsubscribe.mockReset();
    mocks.refreshFeed.mockReset();
    mocks.refreshAllFeeds.mockReset();
    mocks.unsubscribe.mockImplementation((feedId: string) => {
      mocks.subscriptions = mocks.subscriptions.filter((sub) => sub.sourceId !== feedId);
    });
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  afterEach(() => {
    cleanup();
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  it("reactively disables feed network actions but keeps local removal available", async () => {
    const user = userEvent.setup();
    render(<FeedManagerModal open onClose={vi.fn()} />);

    const customUrl = screen.getByPlaceholderText("https://example.com/feed.json");
    await user.type(customUrl, "https://example.com/custom.json");
    expect(screen.getByRole("button", { name: "Refresh all" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Refresh" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Unsubscribe" })).toBeEnabled();
    expect(screen.getAllByRole("button", { name: "Subscribe" })[0]).toBeEnabled();
    expect(screen.getByRole("button", { name: "Add" })).toBeEnabled();

    act(() => useConnectivityStore.getState().setForcedOffline(true));

    expect(screen.getByText(/Feed updates are unavailable while offline/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Refresh all" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Refresh" })).toBeDisabled();
    for (const button of screen.getAllByRole("button", { name: "Subscribe" })) {
      expect(button).toBeDisabled();
    }
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();
    expect(customUrl).toBeEnabled();
    expect(screen.getByRole("button", { name: "Unsubscribe" })).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "Refresh all" }));
    await user.click(screen.getByRole("button", { name: "Refresh" }));
    await user.click(screen.getAllByRole("button", { name: "Subscribe" })[0]);
    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.click(customUrl);
    await user.keyboard("{Enter}");
    expect(mocks.refreshAllFeeds).not.toHaveBeenCalled();
    expect(mocks.refreshFeed).not.toHaveBeenCalled();
    expect(mocks.subscribe).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Unsubscribe" }));
    expect(mocks.unsubscribe).toHaveBeenCalledWith("starter-decks");
    expect(screen.queryByRole("button", { name: "Unsubscribe" })).not.toBeInTheDocument();
  });

  it("translates only the service offline key and leaves ordinary service errors intact", async () => {
    const user = userEvent.setup();
    mocks.subscribe
      .mockRejectedValueOnce(new Error(FEED_ERROR_KEYS.offline))
      .mockRejectedValueOnce(new Error(FEED_ERROR_KEYS.offline));
    render(<FeedManagerModal open onClose={vi.fn()} />);

    await user.click(screen.getAllByRole("button", { name: "Subscribe" })[0]);
    expect(await screen.findByText(/Feed updates are unavailable while offline/i)).toBeInTheDocument();

    await user.type(
      screen.getByPlaceholderText("https://example.com/feed.json"),
      "https://example.com/custom.json",
    );
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(mocks.subscribe).toHaveBeenLastCalledWith("https://example.com/custom.json");
    expect(screen.getByText(/Feed updates are unavailable while offline/i)).toBeInTheDocument();

    mocks.subscribe.mockRejectedValueOnce(new Error("Feed server is unavailable"));
    await user.click(screen.getAllByRole("button", { name: "Subscribe" })[0]);
    expect(await screen.findByText("Feed server is unavailable")).toBeInTheDocument();
  });
});
