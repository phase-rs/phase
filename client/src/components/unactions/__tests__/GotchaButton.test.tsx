import { act } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Zone } from "../../../adapter/types";
import { useGameStore } from "../../../stores/gameStore";
import { gameStateFactory } from "../../../test/factories/gameStateFactory";
import { GOTCHA_BUTTON_MS, GotchaButton } from "../GotchaButton";

const { dispatch } = vi.hoisted(() => ({ dispatch: vi.fn() }));

// Exit animations are not under test; unmount immediately so the DOM mirrors the visible offers.
vi.mock("framer-motion", async () => {
  const React = await import("react");
  const MOTION_PROPS = new Set([
    "initial",
    "animate",
    "exit",
    "transition",
    "whileHover",
    "whileTap",
  ]);
  const motion = new Proxy(
    {},
    {
      get: (_target, tag: string) =>
        function MotionStub({ children, ...props }: Record<string, unknown>) {
          const dom = Object.fromEntries(
            Object.entries(props).filter(([key]) => !MOTION_PROPS.has(key)),
          );
          return React.createElement(tag, dom, children as React.ReactNode);
        },
    },
  );
  return { motion, AnimatePresence: ({ children }: { children: React.ReactNode }) => children };
});

vi.mock("../../../hooks/useGameDispatch", () => ({
  useGameDispatch: () => dispatch,
}));

function objectBinding(object_id: number) {
  return {
    reference: { object_id, incarnation: 0 },
    expected_zone: "Graveyard" as Zone,
  };
}

function offerGotcha(player: number, id = 1) {
  offerGotchas([{ id, player, source: objectBinding(9), returns: objectBinding(9) }]);
}

function offerGotchas(
  offers: {
    id: number;
    player: number;
    source: { reference: { object_id: number; incarnation: number }; expected_zone: Zone };
    returns: { reference: { object_id: number; incarnation: number }; expected_zone: Zone };
  }[],
) {
  act(() => {
    useGameStore.setState({
      gameMode: "ai",
      gameState: gameStateFactory.build({
        un_actions: { enabled_players: [0, 1], gotcha_offers: offers },
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

  it("keeps an expired offer hidden when an unrelated offer arrives, and times each offer separately", () => {
    const a = { id: 1, player: 0, source: objectBinding(9), returns: objectBinding(9) };
    const b = { id: 2, player: 0, source: objectBinding(10), returns: objectBinding(10) };
    offerGotchas([a]);
    render(<GotchaButton />);

    act(() => {
      vi.advanceTimersByTime(GOTCHA_BUTTON_MS);
    });
    expect(screen.queryByRole("button", { name: /gotcha!/i })).not.toBeInTheDocument();

    // B arrives while A is still in engine state: only B is shown.
    offerGotchas([a, b]);
    expect(screen.getAllByRole("button", { name: /gotcha!/i })).toHaveLength(1);

    // A third offer arrives mid-fuse: B keeps its own clock.
    act(() => {
      vi.advanceTimersByTime(1500);
    });
    const c = { id: 3, player: 0, source: objectBinding(11), returns: objectBinding(11) };
    offerGotchas([a, b, c]);
    act(() => {
      // B's fuse (2000ms from t=2000) runs out; let its exit animation finish.
      vi.advanceTimersByTime(1000);
    });
    expect(screen.getAllByRole("button", { name: /gotcha!/i })).toHaveLength(1);
    act(() => {
      vi.advanceTimersByTime(1100);
    });
    expect(screen.queryByRole("button", { name: /gotcha!/i })).not.toBeInTheDocument();
  });
});
