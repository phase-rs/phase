import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useConnectivityStore } from "../../../stores/connectivityStore";
import { OfflineModeBadge } from "../OfflineModeBadge";

describe("OfflineModeBadge", () => {
  beforeEach(() => {
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: true });
  });

  afterEach(cleanup);

  it("stays out of the way while online", () => {
    const { container } = render(<OfflineModeBadge />);

    expect(container).toBeEmptyDOMElement();
  });

  it("makes an explicit Work Offline choice visible", () => {
    useConnectivityStore.setState({ forcedOffline: true, browserOnline: true });

    render(<OfflineModeBadge />);

    expect(screen.getByRole("status")).toHaveTextContent("Offline");
    expect(screen.getByRole("status")).toHaveTextContent("Online updates and services are paused.");
    expect(screen.getByRole("status")).toHaveAttribute("title", "Work Offline is on.");
  });

  it("distinguishes a lost connection from the explicit preference", () => {
    useConnectivityStore.setState({ forcedOffline: false, browserOnline: false });

    render(<OfflineModeBadge />);

    expect(screen.getByRole("status")).toHaveAttribute("title", "No network connection.");
  });
});
