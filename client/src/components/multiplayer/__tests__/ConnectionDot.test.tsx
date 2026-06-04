import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { ConnectionDot } from "../ConnectionDot";

describe("ConnectionDot", () => {
  beforeEach(() => {
    act(() => useMultiplayerStore.setState({ connectionStatus: "disconnected", latencyMs: null }));
  });

  afterEach(() => {
    cleanup();
  });

  it('shows "Disconnected" label when disconnected', () => {
    render(<ConnectionDot />);
    expect(screen.getByText("Disconnected")).toBeInTheDocument();
  });

  it('shows "Connecting..." label when connecting', () => {
    act(() => useMultiplayerStore.setState({ connectionStatus: "connecting" }));
    render(<ConnectionDot />);
    expect(screen.getByText("Connecting...")).toBeInTheDocument();
  });

  it('shows "Connected" label when connected', () => {
    act(() => useMultiplayerStore.setState({ connectionStatus: "connected", latencyMs: null }));
    render(<ConnectionDot />);
    expect(screen.getByText("Connected")).toBeInTheDocument();
  });

  it("shows latency in ms when connected with a latency value", () => {
    act(() => useMultiplayerStore.setState({ connectionStatus: "connected", latencyMs: 42 }));
    render(<ConnectionDot />);
    expect(screen.getByText("42ms")).toBeInTheDocument();
  });

  it("does not show latency label when disconnected even if latencyMs is set", () => {
    act(() => useMultiplayerStore.setState({ connectionStatus: "disconnected", latencyMs: 50 }));
    render(<ConnectionDot />);
    expect(screen.queryByText("50ms")).not.toBeInTheDocument();
  });

  it("does not show latency label when latencyMs is null", () => {
    act(() => useMultiplayerStore.setState({ connectionStatus: "connected", latencyMs: null }));
    render(<ConnectionDot />);
    expect(screen.queryByText(/ms$/)).not.toBeInTheDocument();
  });

  it("applies green latency color class for sub-100ms latency", () => {
    act(() => useMultiplayerStore.setState({ connectionStatus: "connected", latencyMs: 50 }));
    render(<ConnectionDot />);
    const latencyEl = screen.getByText("50ms");
    expect(latencyEl.className).toContain("text-green-400");
  });

  it("applies yellow latency color class for 100–249ms latency", () => {
    act(() => useMultiplayerStore.setState({ connectionStatus: "connected", latencyMs: 150 }));
    render(<ConnectionDot />);
    const latencyEl = screen.getByText("150ms");
    expect(latencyEl.className).toContain("text-yellow-400");
  });

  it("applies red latency color class for 250ms+ latency", () => {
    act(() => useMultiplayerStore.setState({ connectionStatus: "connected", latencyMs: 300 }));
    render(<ConnectionDot />);
    const latencyEl = screen.getByText("300ms");
    expect(latencyEl.className).toContain("text-red-400");
  });

  it("uses the status as the title attribute", () => {
    act(() => useMultiplayerStore.setState({ connectionStatus: "connected", latencyMs: null }));
    render(<ConnectionDot />);
    const container = screen.getByTitle("Connected");
    expect(container).toBeInTheDocument();
  });
});
