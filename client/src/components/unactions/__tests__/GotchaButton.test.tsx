import { act } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore";
import { gameStateFactory } from "../../../test/factories/gameStateFactory";
import { GOTCHA_BUTTON_MS, GotchaButton } from "../GotchaButton";

const { dispatch } = vi.hoisted(() => ({ dispatch: vi.fn() }));

vi.mock("../../../hooks/useGameDispatch", () => ({
  useGameDispatch: () => dispatch,
}));

function offerGotcha(player: number, id = 1) {
  act(() => {
    useGameStore.setState({
      gameMode: "ai",
      gameState: gameStateFactory.build({
        un_actions: {
          enabled_players: [0, 1],
          gotcha_offers: [{ id, player, source_id: 9, returns: 9 }],
        },
      }),
    });
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  dispatch.mockReset();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  act(() => useGameStore.setState({ gameState: null }));
});

describe("GotchaButton", () => {
  it("offers Gotcha! to the viewer and submits the engine's offer when clicked", () => {
    offerGotcha(0);
    render(<GotchaButton />);

    fireEvent.click(screen.getByRole("button", { name: /gotcha!/i }));

    expect(dispatch).toHaveBeenCalledWith({
      type: "UnAction",
      data: { action: { type: "SayGotcha", data: { source_id: 9, returns: 9 } } },
    });
  });

  it("disappears two seconds after the window opens", () => {
    offerGotcha(0);
    render(<GotchaButton />);
    expect(screen.getByRole("button", { name: /gotcha!/i })).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(GOTCHA_BUTTON_MS);
    });

    expect(screen.queryByRole("button", { name: /gotcha!/i })).not.toBeInTheDocument();
  });

  it("is not shown for an opponent's window", () => {
    offerGotcha(1);
    render(<GotchaButton />);
    expect(screen.queryByRole("button", { name: /gotcha!/i })).not.toBeInTheDocument();
  });

  it("shows an identical later window again after the first one closed", () => {
    offerGotcha(0);
    render(<GotchaButton />);
    act(() => {
      vi.advanceTimersByTime(GOTCHA_BUTTON_MS);
    });
    expect(screen.queryByRole("button", { name: /gotcha!/i })).not.toBeInTheDocument();

    act(() => useGameStore.setState({ gameState: gameStateFactory.build() }));
    offerGotcha(0);

    expect(screen.getByRole("button", { name: /gotcha!/i })).toBeInTheDocument();
  });

  it("resets the two-second window when a new offer with a new id arrives", () => {
    offerGotcha(0, 1);
    render(<GotchaButton />);
    expect(screen.getByRole("button", { name: /gotcha!/i })).toBeInTheDocument();

    act(() => {
      vi.advanceTimersByTime(1500);
    });
    // New offer arrives with a new id before the first window expires
    offerGotcha(0, 2);
    expect(screen.getAllByRole("button", { name: /gotcha!/i })[0]).toBeInTheDocument();

    act(() => {
      // 1000ms later: 2500ms since offer 1, but only 1000ms since offer 2
      vi.advanceTimersByTime(1000);
    });
    expect(screen.getAllByRole("button", { name: /gotcha!/i })[0]).toBeInTheDocument();

    act(() => {
      // Advance remaining 1000ms to complete offer 2's 2000ms
      vi.advanceTimersByTime(1000);
    });
    expect(screen.queryByRole("button", { name: /gotcha!/i })).not.toBeInTheDocument();
  });
});
