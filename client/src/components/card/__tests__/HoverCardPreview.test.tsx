import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { HoverCardPreview } from "../HoverCardPreview.tsx";

vi.mock("../CardPreview.tsx", () => ({
  CardPreview: ({ cardName, dockSide }: { cardName: string | null; dockSide?: boolean }) => (
    <div data-dock-side={dockSide} data-testid="preview">
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
