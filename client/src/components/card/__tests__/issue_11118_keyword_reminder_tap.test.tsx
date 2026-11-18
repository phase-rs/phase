import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../../stores/gameStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { CardPreview } from "../CardPreview.tsx";

// Issue #11118: a tap no longer opens gameplay tooltips, so a tap on Resolve
// leaves the board clear. A keyword's reminder text in the card preview only
// explains, and on a touch tablet wide enough for this preview a tap is the
// only way to read it, so the keyword opts in to opening on a tap and closing
// on the next one.

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
  });
  useGameStore.setState({
    gameState: buildGameState({
      objects: buildObjectMap(object),
      next_object_id: 102,
      battlefield: [object.id],
      next_timestamp: 2,
    }),
    spellCosts: {},
  });
  useUiStore.setState({ inspectedObjectId: object.id, altHeld: false });

  render(
    <>
      <CardPreview cardName="Serra Angel" position={{ x: 20, y: 20 }} />
      <button type="button">Elsewhere</button>
    </>,
  );
  const keyword = screen.getByText("Flying").closest<HTMLElement>("[aria-describedby]");
  if (!keyword) throw new Error("the Flying keyword has no described trigger");
  return {
    keyword,
    reminder: screen.getByText(/creatures with flying or reach/).closest('[role="tooltip"]'),
    elsewhere: screen.getByRole("button", { name: "Elsewhere" }),
  };
}

// The events a browser sends for a finger tapping an element, in order: the
// finger lifting fires `pointerleave`, and the focus and `click` come after it.
function tap(element: HTMLElement) {
  fireEvent.pointerEnter(element, { pointerType: "touch" });
  fireEvent.pointerDown(element, { pointerType: "touch" });
  fireEvent.pointerUp(element, { pointerType: "touch" });
  fireEvent.pointerLeave(element, { pointerType: "touch" });
  act(() => element.focus());
  fireEvent.click(element);
}

describe("issue #11118: a tap on a keyword in the card preview shows its reminder text", () => {
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
    expect(keyword).toBeInTheDocument();
    expect(reminder).not.toBeVisible();
  });

  it("closes when a tap lands elsewhere", () => {
    const { keyword, reminder, elsewhere } = renderFlyingPreview();
    tap(keyword);
    expect(reminder).toBeVisible();

    tap(elsewhere);

    expect(keyword).toBeInTheDocument();
    expect(reminder).not.toBeVisible();
  });
});
