import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";

import { HostSetup } from "../HostSetup";

describe("HostSetup", () => {
  afterEach(() => {
    cleanup();
  });

  it("uses P2P labeling/theme and hides server-only lobby listing in p2p mode", () => {
    render(
      <HostSetup
        onHost={vi.fn()}
        onBack={vi.fn()}
        connectionMode="p2p"
      />,
    );

    expect(screen.getByRole("heading", { name: "Host Direct Match" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Host P2P Game" })).toBeInTheDocument();
    expect(screen.queryByText("List in lobby")).not.toBeInTheDocument();
    expect(screen.getByText("P2P currently supports 2-player Standard.")).toBeInTheDocument();
  });

  it("keeps server labeling and lobby listing in server mode", () => {
    render(
      <HostSetup
        onHost={vi.fn()}
        onBack={vi.fn()}
        connectionMode="server"
      />,
    );

    expect(screen.getByRole("heading", { name: "Host Match" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Host Game" })).toBeInTheDocument();
    expect(screen.getByText("List in lobby")).toBeInTheDocument();
  });
});
