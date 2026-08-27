import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { HoverCardPreview } from "../HoverCardPreview.tsx";

vi.mock("../CardPreview.tsx", () => ({
  CardPreview: ({
    cardName,
    dockSide,
    dockPosition,
  }: {
    cardName: string | null;
    dockSide?: boolean;
    dockPosition?: string;
  }) => (
    <div
      data-card-preview=""
      data-dock-position={dockPosition}
      data-dock-side={dockSide}
      data-testid="preview"
    >
      {cardName}
    </div>
  ),
}));

const CARD = { name: "Pithing Needle" };

describe("HoverCardPreview", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    usePreferencesStore.setState({ cardPreviewMode: "follow", cardPreviewHoverDelayMs: 0 });
    useUiStore.setState({ shiftHeld: false });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    usePreferencesStore.setState({ cardPreviewMode: "follow", cardPreviewHoverDelayMs: 0 });
    useUiStore.setState({ shiftHeld: false });
  });

  it("docks non-game hover previews when the side preference is selected", () => {
    usePreferencesStore.setState({ cardPreviewMode: "side" });

    render(<HoverCardPreview card={CARD} />);

    expect(screen.getByTestId("preview")).toHaveTextContent(CARD.name);
    expect(screen.getByTestId("preview")).toHaveAttribute("data-dock-side", "true");
  });

  it("can keep a workspace preview docked without changing the game-board preference", () => {
    render(<HoverCardPreview card={CARD} forceDockSide dockPosition="middle-right" />);

    expect(screen.getByTestId("preview")).toHaveAttribute("data-dock-side", "true");
    expect(screen.getByTestId("preview")).toHaveAttribute("data-dock-position", "middle-right");
  });

  it("dismisses a deck-owned preview when its hover source is removed", () => {
    const onDismiss = vi.fn();
    render(<HoverCardPreview card={CARD} onDismiss={onDismiss} />);
    const querySelector = vi.spyOn(document, "querySelector").mockReturnValue(null);

    fireEvent.pointerMove(window, { pointerType: "mouse" });

    expect(onDismiss).toHaveBeenCalledOnce();
    querySelector.mockRestore();
  });

  it("keeps a deck preview open while the pointer is over its interactive panel", () => {
    const onDismiss = vi.fn();
    render(<HoverCardPreview card={CARD} onDismiss={onDismiss} />);

    fireEvent.pointerMove(screen.getByTestId("preview"), { pointerType: "mouse" });

    expect(onDismiss).not.toHaveBeenCalled();
  });

  it("shows a hovered card only while Shift is held in shift mode", () => {
    usePreferencesStore.setState({ cardPreviewMode: "shift" });
    render(<HoverCardPreview card={CARD} />);

    expect(screen.getByTestId("preview")).toBeEmptyDOMElement();

    fireEvent.keyDown(window, { key: "Shift" });
    expect(screen.getByTestId("preview")).toHaveTextContent(CARD.name);

    fireEvent.keyUp(window, { key: "Shift" });
    expect(screen.getByTestId("preview")).toBeEmptyDOMElement();
  });

  it("applies the configured delay before the first hover preview", () => {
    usePreferencesStore.setState({ cardPreviewHoverDelayMs: 250 });
    const { rerender } = render(<HoverCardPreview card={null} />);

    rerender(<HoverCardPreview card={CARD} />);
    expect(screen.getByTestId("preview")).toBeEmptyDOMElement();

    act(() => vi.advanceTimersByTime(250));
    expect(screen.getByTestId("preview")).toHaveTextContent(CARD.name);
  });
});
