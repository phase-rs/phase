import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { useMultiplayerStore } from "../../../stores/multiplayerStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { PlayerHand } from "../PlayerHand.tsx";
import { HAND_PREVIEW_HOLD_DELAY_MS } from "../useHandScrubPreview.ts";

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

describe("PlayerHand mobile lift", () => {
  const originalWidth = window.innerWidth;

  beforeEach(() => {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 390 });
    useMultiplayerStore.setState({ activePlayerId: 0 });
    useUiStore.setState({
      inspectedObjectId: null,
      mobileHandGesture: null,
      previewSticky: false,
    });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    useGameStore.setState({ gameState: null, spellCosts: {} });
    useUiStore.getState().dismissPreview();
    Object.defineProperty(window, "innerWidth", {
      configurable: true,
      value: originalWidth,
    });
  });

  it("lowers a tapped-up hand when the player next taps outside it", () => {
    const card = gameObjectFactory.withId(301).inHand().named("Mobile Card").build();
    const gameState = gameStateFactory
      .withPlayers({ id: 0, hand: [card.id] }, 1)
      .withObjects(card)
      .build();
    act(() => {
      useGameStore.setState({ gameState, spellCosts: {} });
    });

    const { container } = render(<PlayerHand />);
    const handLift = container.querySelector<HTMLElement>("[data-player-hand-lift]");
    const cardElement = container.querySelector<HTMLElement>("[data-hand-card]");
    expect(handLift).toHaveAttribute("data-player-hand-expanded", "false");
    expect(cardElement).not.toBeNull();
    expect(cardElement).toHaveAttribute("data-hand-hover-enabled", "false");

    fireEvent.click(cardElement!);
    expect(handLift).toHaveAttribute("data-player-hand-expanded", "true");
    expect(useUiStore.getState().inspectedObjectId).toBeNull();

    fireEvent.click(cardElement!);
    expect(useUiStore.getState().inspectedObjectId).toBeNull();

    fireEvent.pointerDown(document.body);
    expect(handLift).toHaveAttribute("data-player-hand-expanded", "false");
  });

  it("opens a held hand card immediately and keeps it until a later dismissal", () => {
    vi.useFakeTimers();
    const card = gameObjectFactory.withId(302).inHand().named("Held Card").build();
    const gameState = gameStateFactory
      .withPlayers({ id: 0, hand: [card.id] }, 1)
      .withObjects(card)
      .build();
    act(() => {
      useGameStore.setState({ gameState, spellCosts: {} });
    });

    const { container } = render(<PlayerHand />);
    const cardElement = container.querySelector<HTMLElement>("[data-hand-card]");
    const handLift = container.querySelector<HTMLElement>("[data-player-hand-lift]");
    expect(cardElement).not.toBeNull();
    cardElement!.getBoundingClientRect = () => ({
      bottom: 580,
      height: 140,
      left: 500,
      right: 600,
      top: 440,
      width: 100,
      x: 500,
      y: 440,
      toJSON: () => ({}),
    } as DOMRect);

    fireEvent.pointerDown(cardElement!, {
      button: 0,
      clientX: 550,
      clientY: 500,
      isPrimary: true,
      pointerId: 19,
      pointerType: "touch",
    });
    act(() => vi.advanceTimersByTime(HAND_PREVIEW_HOLD_DELAY_MS));

    expect(useUiStore.getState().inspectedObjectId).toBe(card.id);
    expect(useUiStore.getState().previewSticky).toBe(true);

    fireEvent.pointerUp(cardElement!, {
      button: 0,
      clientX: 550,
      clientY: 500,
      isPrimary: true,
      pointerId: 19,
      pointerType: "touch",
    });
    fireEvent.click(cardElement!);

    expect(useUiStore.getState().inspectedObjectId).toBe(card.id);
    expect(useUiStore.getState().mobileHandGesture).toBeNull();
    expect(handLift).toHaveAttribute("data-player-hand-expanded", "false");

    act(() => useUiStore.getState().dismissPreview());
    expect(useUiStore.getState().inspectedObjectId).toBeNull();
  });
});
