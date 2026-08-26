import { act, cleanup, fireEvent, render, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameAction } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { gameStateFactory } from "../../../test/factories/gameStateFactory.ts";
import { GameCardPreview } from "../../card/GameCardPreview.tsx";
import { HAND_REORDER_SELECTOR } from "../handInsertionSlot.ts";
import { PlayerHand } from "../PlayerHand.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({
    src: "card.png",
    isLoading: false,
    isRotated: false,
    isFlip: false,
  }),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
  cleanup();
  useGameStore.setState({ gameState: null, spellCosts: {}, legalActionsByObject: {} });
  useUiStore.setState({
    inspectedObjectId: null,
    inspectedFaceIndex: 0,
    previewSticky: false,
    altHeld: false,
  });
});

function castSpell(objectId: number): GameAction {
  return { type: "CastSpell", data: { object_id: objectId, card_id: 1, targets: [] } };
}

function graveyardWingState() {
  const gyCard = gameObjectFactory.withId(301).inGraveyard().named("Encore Card").build();
  const handCard = gameObjectFactory.withId(302).inHand().named("Hand Card").build();
  const gameState = gameStateFactory
    .withPlayers({ id: 0, hand: [handCard.id], graveyard: [gyCard.id] }, 1)
    .withObjects(gyCard, handCard)
    .build();
  useGameStore.setState({
    gameState,
    spellCosts: {},
    legalActionsByObject: { [String(gyCard.id)]: [castSpell(gyCard.id)] },
  });
  return { gyCard, handCard };
}

function exileWingState() {
  const exileCard = gameObjectFactory.withId(401).inExile().named("Plot Card").build();
  const handCard = gameObjectFactory.withId(402).inHand().named("Hand Card").build();
  const gameState = gameStateFactory
    .withPlayers({ id: 0, hand: [handCard.id] }, 1)
    .withObjects(exileCard, handCard)
    .build({ exile: [exileCard.id] });
  useGameStore.setState({
    gameState,
    spellCosts: {},
    legalActionsByObject: { [String(exileCard.id)]: [castSpell(exileCard.id)] },
  });
  return { exileCard };
}

/**
 * Stand in for the browser's `:hover` hit-test, which jsdom does not implement.
 *
 * Deliberately consults the real attribute on the element under the cursor
 * rather than returning it unconditionally: that is what makes the dismissal
 * test discriminating. Revert the production change and `hovered` no longer
 * matches `[data-card-hover]`, so this returns null exactly as a real browser
 * would over an untagged element, and the poll dismisses the preview.
 */
function simulatePointerOver(hovered: Element) {
  const realQuerySelector = document.querySelector.bind(document);
  vi.spyOn(document, "querySelector").mockImplementation((selector: string) => {
    if (selector === "[data-card-hover]:hover") {
      return hovered.matches("[data-card-hover]") ? hovered : null;
    }
    return realQuerySelector(selector);
  });
}

/**
 * Regression guard for the graveyard/exile wing preview.
 *
 * `data-card-hover` means "inspectable card" — usePreviewDismiss polls
 * `[data-card-hover]:hover` every 300ms and tears the preview down when nothing
 * matches. The wings once omitted the attribute (to stay out of the reorder DOM
 * sweep), so hovering a flashback / escape / encore card showed its image for
 * roughly 600ms and then dropped it, with the cursor still on the card.
 *
 * The two concerns are now carried by two attributes: `data-card-hover` for
 * inspectability (wings included) and `data-hand-card` for reorderability
 * (wings excluded). Both halves are asserted here — dropping either one
 * reintroduces a bug.
 */
describe("castable graveyard/exile wing hover", () => {
  it("keeps the preview open while the pointer rests on a wing card", () => {
    vi.useFakeTimers();
    graveyardWingState();

    const { container } = render(
      <>
        <PlayerHand />
        <GameCardPreview />
      </>,
    );

    // Locate the wing by its card art and climb to ZoneFanCard's root (the
    // element that owns the hover handlers). Deliberately NOT located by
    // `[data-card-hover]`: finding it through the very attribute under test
    // would make a revert fail at this lookup rather than at the dismissal
    // assertion below, which is the behaviour this test exists to pin.
    const art = within(container).getByAltText("Encore Card");
    const wing = art.closest<HTMLElement>(".cursor-grab");
    expect(wing).not.toBeNull();
    simulatePointerOver(wing!);

    act(() => {
      fireEvent.mouseEnter(wing!);
      vi.advanceTimersByTime(0);
    });

    const preview = () => container.querySelector<HTMLElement>("[data-card-preview]");
    expect(preview()).not.toBeNull();
    expect(within(preview()!).getByAltText("Encore Card")).toBeInTheDocument();

    // usePreviewDismiss polls every 300ms and skips its first tick, so the old
    // behaviour dropped the preview by ~600ms. Run well past that with the
    // pointer never leaving the card.
    act(() => vi.advanceTimersByTime(1500));

    expect(useUiStore.getState().inspectedObjectId).not.toBeNull();
    expect(preview()).not.toBeNull();
    expect(within(preview()!).getByAltText("Encore Card")).toBeInTheDocument();
  });

  it("marks wing cards inspectable but not reorderable", () => {
    const { handCard } = graveyardWingState();

    const { container } = render(<PlayerHand />);

    // The wing rendered at all (engine surfaced a cast action for it).
    const inspectable = container.querySelectorAll("[data-card-hover]");
    expect(inspectable.length).toBe(2);

    // ...but only the hand card is part of the reorder index space. Queried
    // through the exported selector, so pointing it back at `[data-card-hover]`
    // — the naive one-line version of this fix — turns this assertion red
    // instead of silently admitting the wings into the reorder rects.
    const reorderable = container.querySelectorAll(HAND_REORDER_SELECTOR);
    expect(reorderable.length).toBe(1);
    expect((reorderable[0] as HTMLElement).dataset.objectId).toBe(String(handCard.id));
  });

  it("keeps exile and graveyard wing cards in the fan's interactive stacking layer", () => {
    graveyardWingState();
    const { container: graveyardContainer } = render(<PlayerHand />);
    const graveyardWing = within(graveyardContainer)
      .getByAltText("Encore Card")
      .closest<HTMLElement>(".cursor-grab");
    expect(graveyardWing).not.toBeNull();
    expect(graveyardWing).toHaveStyle({ zIndex: "1" });

    cleanup();
    exileWingState();
    const { container: exileContainer } = render(<PlayerHand />);
    const exileWing = within(exileContainer)
      .getByAltText("Plot Card")
      .closest<HTMLElement>(".cursor-grab");
    expect(exileWing).not.toBeNull();
    expect(exileWing).toHaveStyle({ zIndex: "0" });
  });
});
