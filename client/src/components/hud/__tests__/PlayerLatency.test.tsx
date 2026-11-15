import { act } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it } from "vitest";

import { useMultiplayerStore } from "../../../stores/multiplayerStore";
import { PlayerLatency } from "../PlayerLatency";

beforeEach(() => useMultiplayerStore.setState({ playerLatencies: {}, disconnectedPlayers: new Set() }));
afterEach(cleanup);

it("shows each seat's host latency and replaces stale readings with Waiting", () => {
  useMultiplayerStore.setState({ playerLatencies: { 0: 0, 1: 42, 2: 180 } });
  render(<><PlayerLatency playerId={0} /><PlayerLatency playerId={1} /><PlayerLatency playerId={2} /></>);
  expect(screen.getByText("Host")).toBeInTheDocument();
  expect(screen.getByText("42 ms")).toBeInTheDocument();
  expect(screen.getByText("180 ms")).toBeInTheDocument();
  act(() => useMultiplayerStore.setState({ playerLatencies: { 0: 0, 1: null, 2: 180 } }));
  expect(screen.queryByText("42 ms")).not.toBeInTheDocument();
  expect(screen.getByText("Waiting")).toBeInTheDocument();
});

it("does not invent latency for unmeasured or disconnected seats", () => {
  useMultiplayerStore.setState({ playerLatencies: { 1: 42 }, disconnectedPlayers: new Set([1]) });
  const { container } = render(<><PlayerLatency playerId={1} /><PlayerLatency playerId={2} /></>);
  expect(container).toBeEmptyDOMElement();
});
