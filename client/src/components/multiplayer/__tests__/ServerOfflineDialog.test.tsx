import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

import { ServerOfflineDialog } from "../ServerOfflineDialog";

describe("ServerOfflineDialog", () => {
  afterEach(() => {
    cleanup();
  });

  it("shows the server address and opens settings on click", () => {
    const onOpenSettings = vi.fn();
    render(
      <ServerOfflineDialog
        isOpen
        serverAddress="ws://localhost:9374/ws"
        onOpenSettings={onOpenSettings}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByText("Server Offline")).toBeInTheDocument();
    expect(screen.getByText("ws://localhost:9374/ws")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Open Settings" }));

    expect(onOpenSettings).toHaveBeenCalledTimes(1);
  });

  it("does not render when closed", () => {
    render(
      <ServerOfflineDialog
        isOpen={false}
        serverAddress="ws://localhost:9374/ws"
        onOpenSettings={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(screen.queryByText("Server Offline")).not.toBeInTheDocument();
  });
});
