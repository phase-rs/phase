import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const motionState = vi.hoisted(() => ({ reduced: false }));

vi.mock("framer-motion", async (importOriginal) => ({
  ...(await importOriginal<typeof import("framer-motion")>()),
  useReducedMotion: () => motionState.reduced,
}));

import { TurnBanner } from "../TurnBanner";
import { useUiStore } from "../../../stores/uiStore";
import { stubBannerTitleLayout, type TitleWidths } from "../../../test/helpers/bannerTitleLayout";

/** `turnBannerText`'s headline for an opponent named "Alphinaud Leveilleur" (#11117). */
const LONG_TITLE = "ALPHINAUD LEVEILLEUR'S TURN";
/** Equal side gutters outside the wider safe-area inset. */
const SIDE_GUTTERS = "px-[calc(max(env(safe-area-inset-left),env(safe-area-inset-right))+var(--game-edge-right,0.75rem))]";

function showBanner(text: string): void {
  act(() => {
    useUiStore.setState({
      showTurnBanner: true,
      turnBannerText: text,
      turnBannerNumber: 3,
      turnBannerIsYours: false,
    });
  });
}

/** The band's direct children with `className`, the decorative lines among them. */
function bandChildrenWithClass(band: HTMLElement, className: string): Element[] {
  return Array.from(band.children).filter((child) => child.classList.contains(className));
}

// happy-dom does no layout, so these tests pin the classes and structure that
// centre and bound the title, and set the widths a browser would measure (see
// `stubBannerTitleLayout`) to check the title is the one fitted to its line.
describe("TurnBanner", () => {
  let widths: TitleWidths;
  let layout: ReturnType<typeof stubBannerTitleLayout>;

  beforeEach(() => {
    // A phone's 388px line; the title's widths at its 48px size class.
    widths = { line: 388, oneLine: 300, longestRun: 160 };
    layout = stubBannerTitleLayout(widths);
  });

  afterEach(() => {
    cleanup();
    layout.restore();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    motionState.reduced = false;
    useUiStore.setState({
      showTurnBanner: false,
      turnBannerText: "",
      turnBannerNumber: null,
      turnBannerIsYours: false,
    });
  });

  it("centres a long name inside the side gutters, at the floor size where it has to wrap", () => {
    // Too wide for one line above the 28px floor.
    widths.oneLine = 1200;
    render(<TurnBanner />);
    showBanner(LONG_TITLE);

    const title = screen.getByText(LONG_TITLE);
    expect(title).toHaveClass("block", "text-center", "text-balance");
    expect(title.style.fontSize).toBe("28px");

    // The band is the title's line: the full width, inside the gutters.
    const band = title.parentElement!;
    expect(band).toHaveClass("w-full", SIDE_GUTTERS);
  });

  it("frames a wrapped title with the band's edges instead of fixed offsets from the centre", () => {
    widths.oneLine = 1200;
    render(<TurnBanner />);
    showBanner(LONG_TITLE);

    const band = screen.getByText(LONG_TITLE).parentElement!;
    // The decorative lines ride the band's top and bottom edges, so a title
    // that wraps pushes them out rather than being cut through.
    expect(bandChildrenWithClass(band, "top-0")).toHaveLength(1);
    expect(bandChildrenWithClass(band, "bottom-0")).toHaveLength(1);
    // The "Turn 3" subtitle sits above the band, however tall it grows.
    expect(screen.getByText("Turn 3")).toHaveClass("bottom-full");
    expect(screen.getByText("Turn 3").parentElement).toBe(band);
  });

  it("renders a short name as before: its size class, in a one-line band", () => {
    render(<TurnBanner />);
    showBanner("YOUR TURN");

    const title = screen.getByText("YOUR TURN");
    expect(title).toHaveClass("text-5xl", "font-extrabold", "tracking-[0.2em]", "uppercase");
    // It fits its line, so it keeps the size class: no inline size.
    expect(title.style.fontSize).toBe("");

    // 12px of padding around the 48px line: the lines sit 36px from the centre,
    // where they were drawn before, and the strip reaches 4px past them.
    const band = title.parentElement!;
    expect(band).toHaveClass("py-3");
    const strip = band.firstElementChild as HTMLElement;
    expect(strip.style.top).toBe("-4px");
    expect(strip.style.bottom).toBe("-4px");
  });

  it("fits the reduced-motion title to its line the same way", () => {
    motionState.reduced = true;
    widths.oneLine = 600;
    render(<TurnBanner />);
    showBanner(LONG_TITLE);

    const title = screen.getByText(LONG_TITLE);
    expect(title).toHaveClass("text-center", "text-balance");
    // 600px at 48px fits the 386px the title may fill at 48 × 386 / 600.
    expect(parseFloat(title.style.fontSize)).toBeCloseTo((48 * 386) / 600);
    expect(title.parentElement).toHaveClass("w-full", SIDE_GUTTERS);
  });
});
