import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const motionState = vi.hoisted(() => ({ reduced: false }));

vi.mock("framer-motion", async (importOriginal) => ({
  ...(await importOriginal<typeof import("framer-motion")>()),
  useReducedMotion: () => motionState.reduced,
}));

import { TurnBanner } from "../TurnBanner";
import { useUiStore } from "../../../stores/uiStore";

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
// centre, bound and size the title, not rendered positions.
describe("TurnBanner", () => {
  afterEach(() => {
    cleanup();
    motionState.reduced = false;
    useUiStore.setState({
      showTurnBanner: false,
      turnBannerText: "",
      turnBannerNumber: null,
      turnBannerIsYours: false,
    });
  });

  it("centres and balances a long name inside the side gutters, sized to its estimated width", () => {
    render(<TurnBanner />);
    showBanner(LONG_TITLE);

    const title = screen.getByText(LONG_TITLE);
    expect(title).toHaveClass("block", "text-center", "text-balance");
    // 27 characters, and the 12 of "LEVEILLEUR'S", at (0.68 + 0.2)em each.
    expect(title.style.getPropertyValue("--title-width-em")).toBe("23.76");
    expect(title.style.getPropertyValue("--title-run-width-em")).toBe("10.56");

    // The band the title fits itself to: the full width, inside the gutters.
    const band = title.parentElement!;
    expect(band).toHaveClass("@container", "w-full", SIDE_GUTTERS);
  });

  it("frames a wrapped title with the band's edges instead of fixed offsets from the centre", () => {
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

  it("keeps a short name's size class and the one-line band's geometry", () => {
    render(<TurnBanner />);
    showBanner("YOUR TURN");

    const title = screen.getByText("YOUR TURN");
    expect(title).toHaveClass("text-5xl", "font-extrabold", "tracking-[0.2em]", "uppercase");
    expect(title.style.getPropertyValue("--title-width-em")).toBe("7.92");

    // 12px of padding around the 48px line: the lines sit 36px from the centre,
    // where they were drawn before, and the strip reaches 4px past them.
    const band = title.parentElement!;
    expect(band).toHaveClass("py-3");
    const strip = band.firstElementChild as HTMLElement;
    expect(strip.style.top).toBe("-4px");
    expect(strip.style.bottom).toBe("-4px");
  });

  it("centres and balances a long name under reduced motion too", () => {
    motionState.reduced = true;
    render(<TurnBanner />);
    showBanner(LONG_TITLE);

    const title = screen.getByText(LONG_TITLE);
    expect(title).toHaveClass("text-center", "text-balance");
    // 27 characters at (0.68 + 0.05)em: this title is `tracking-wider`.
    expect(title.style.getPropertyValue("--title-width-em")).toBe("19.71");
    expect(title.parentElement).toHaveClass("@container", "w-full", SIDE_GUTTERS);
  });
});
