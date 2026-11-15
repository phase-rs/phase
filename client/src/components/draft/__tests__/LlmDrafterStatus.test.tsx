import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const store = vi.hoisted(() => ({ state: {} as Record<string, unknown> }));

vi.mock("../../../stores/draftStore", () => ({
  useDraftStore: (selector: (state: Record<string, unknown>) => unknown) =>
    selector(store.state),
}));

import { LlmDrafterStatus } from "../LlmDrafterStatus";

afterEach(cleanup);

beforeEach(() => {
  store.state = {
    llmDrafters: [],
    awaitingDrafters: false,
    view: {
      seats: [
        { seat_index: 0, display_name: "You" },
        { seat_index: 1, display_name: "Bot 1" },
        { seat_index: 2, display_name: "Bot 2" },
      ],
    },
  };
});

describe("LlmDrafterStatus", () => {
  it("renders nothing when no LLM is drafting this pack", () => {
    const { container } = render(<LlmDrafterStatus />);
    expect(container).toBeEmptyDOMElement();
  });

  it("shows each drafter's progress under the engine's seat name", () => {
    store.state.llmDrafters = [
      { seat: 1, state: "ready" },
      { seat: 2, state: "picking" },
    ];
    render(<LlmDrafterStatus />);

    expect(screen.getByLabelText("Bot 1: Picked")).toHaveAttribute("data-llm-drafter-state", "ready");
    expect(screen.getByLabelText("Bot 2: Picking")).toHaveAttribute("data-llm-drafter-state", "picking");
    expect(screen.getByText("AI drafters")).toBeInTheDocument();
    expect(screen.queryByText("Waiting on all players to pick…")).not.toBeInTheDocument();
  });

  it("tells the player the pack is waiting on the table once they have picked", () => {
    store.state.llmDrafters = [
      { seat: 1, state: "fallback" },
      { seat: 2, state: "picking" },
    ];
    store.state.awaitingDrafters = true;
    render(<LlmDrafterStatus />);

    expect(screen.getByRole("status")).toHaveTextContent("Waiting on all players to pick…");
    expect(screen.getByLabelText("Bot 1: Engine pick")).toBeInTheDocument();
  });
});
