import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CardPreview } from "../CardPreview.tsx";

// Issue #11118: a tap no longer opens gameplay tooltips, so a tap on Resolve
// leaves the board clear. A keyword's reminder text and a counter's
// explanation in the card preview only explain, and on a touch tablet wide
// enough for this preview a tap is the only way to read them, so both opt in
// to opening on a tap and closing on the next one.

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: vi.fn((cardName: string) => ({
    src: `${cardName}.png`,
    isLoading: false,
    isRotated: false,
    isFlip: false,
  })),
}));

vi.mock("../../../hooks/useEngineCardData.ts", () => ({
  useEngineCardData: () => null,
  useCardParseDetails: () => null,
  useCardRulings: () => [],
}));

// happy-dom matches `:focus-visible` for every focused element; a browser does
// not match it for the focus a tap gives.
let focusVisible = false;
const initialInnerWidth = window.innerWidth;

function renderFlyingPreview() {
  const object = buildGameObject({
    id: 101,
    card_id: 1,
    zone: "Battlefield",
    name: "Serra Angel",
    keywords: ["Flying"],
    base_keywords: ["Flying"],
    counters: { charge: 2 },
  });
  const gameState = buildGameState({
    objects: buildObjectMap(object),
    next_object_id: 102,
    battlefield: [object.id],
    next_timestamp: 2,
  });
  // The engine's counter projection is the one source of the preview's counter rows.
  gameState.derived = { counter_display: { [object.id]: { pills: [{ counter: "charge", count: 2 }] } } };
  useGameStore.setState({ gameState, spellCosts: {} });
  useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });

  render(
    <>
      <CardPreview cardName="Serra Angel" position={{ x: 20, y: 20 }} />
      <button type="button">Elsewhere</button>
    </>,
  );
  const keyword = screen.getByText("Flying").closest<HTMLElement>("[aria-describedby]");
  if (!keyword) throw new Error("the Flying keyword has no described trigger");
  const counter = screen.getByText("charge: 2").closest<HTMLElement>(".group");
  if (!counter) throw new Error("the charge counter has no tooltip trigger");
  return {
    keyword,
    reminder: screen.getByText(/creatures with flying or reach/).closest('[role="tooltip"]'),
    counter,
    counterExplanation: screen.getByText(/2 charge counters/i).closest('[role="tooltip"]'),
    elsewhere: screen.getByRole("button", { name: "Elsewhere" }),
  };
}

// A subset, in order, of the events a browser sends for a finger tapping an
// element: the finger lifting fires `pointerleave`, and the focus (for a
// focusable element) and `click` come after it. happy-dom reports a tabIndex
// of -1 for any element without a `tabindex` attribute, buttons included; the
// keyword carries `tabindex="0"` and the counter carries none.
function tap(element: HTMLElement) {
  fireEvent.pointerEnter(element, { pointerType: "touch" });
  fireEvent.pointerDown(element, { pointerType: "touch" });
  fireEvent.pointerUp(element, { pointerType: "touch" });
  fireEvent.pointerLeave(element, { pointerType: "touch" });
  if (element.tabIndex >= 0) act(() => element.focus());
  fireEvent.click(element);
}

describe("issue #11118: a tap in the card preview shows a keyword's or a counter's explanation", () => {
  beforeEach(() => {
    focusVisible = false;
    const matches = Element.prototype.matches;
    vi.spyOn(Element.prototype, "matches").mockImplementation(function (this: Element, selector: string) {
      return selector === ":focus-visible" ? focusVisible : matches.call(this, selector);
    });
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 1280 });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: initialInnerWidth });
    useGameStore.setState(useGameStore.getInitialState(), true);
    useUiStore.getState().dismissPreview();
  });

  it("is reachable by keyboard and described by its reminder", () => {
    const { keyword, reminder } = renderFlyingPreview();

    expect(keyword).toHaveAttribute("tabindex", "0");
    expect(reminder).toHaveAttribute("id", keyword.getAttribute("aria-describedby"));
  });

  it("opens on a tap and closes on the next tap on the keyword", () => {
    const { keyword, reminder } = renderFlyingPreview();
    expect(reminder).not.toBeVisible();

    tap(keyword);
    expect(reminder).toBeVisible();

    tap(keyword);
    expect(reminder).toBeInTheDocument();
    expect(reminder).not.toBeVisible();
  });

  it("closes when a tap lands elsewhere", () => {
    const { keyword, reminder, elsewhere } = renderFlyingPreview();
    tap(keyword);
    expect(reminder).toBeVisible();

    // Closed by the tap's `pointerdown`, before focus moves to the other control.
    fireEvent.pointerDown(elsewhere, { pointerType: "touch" });

    expect(keyword).toHaveFocus();
    expect(reminder).toBeInTheDocument();
    expect(reminder).not.toBeVisible();
  });

  it("lets a counter take pointer events inside the click-through preview", () => {
    const { counter } = renderFlyingPreview();

    // The desktop preview is `pointer-events-none`, so a counter is reachable
    // only if the nearest pointer-events rule above it turns them back on.
    const nearestRule = counter.closest(".pointer-events-auto, .pointer-events-none");
    expect(nearestRule).toHaveClass("pointer-events-auto");
  });

  it("opens a counter's explanation on a tap and closes it on the next tap", () => {
    const { counter, counterExplanation } = renderFlyingPreview();
    expect(counterExplanation).not.toBeVisible();

    tap(counter);
    expect(counterExplanation).toBeVisible();

    tap(counter);
    expect(counterExplanation).toBeInTheDocument();
    expect(counterExplanation).not.toBeVisible();
  });
});
