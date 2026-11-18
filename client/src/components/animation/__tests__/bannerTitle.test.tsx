import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useBannerTitleFit } from "../bannerTitle";
import { stubBannerTitleLayout, type TitleWidths } from "../../../test/helpers/bannerTitleLayout";

const LONG_TITLE = "ALPHINAUD LEVEILLEUR'S TURN";

/** A 48px (`text-5xl`) title on its line, floored at 1.75rem (28px), as the turn banner's. */
function FittedTitle({ text }: { text: string }) {
  const titleRef = useBannerTitleFit(text, 1.75);
  return (
    <div data-testid="line">
      <span ref={titleRef} className="text-5xl">
        {text}
      </span>
    </div>
  );
}

// happy-dom does no layout: each test sets the widths a browser would measure
// at the 48px size class (see `stubBannerTitleLayout`) and checks the size the
// hook gives the title. The title may fill its line less a pixel of slack: 387px.
describe("useBannerTitleFit", () => {
  let widths: TitleWidths;
  let layout: ReturnType<typeof stubBannerTitleLayout>;

  beforeEach(() => {
    widths = { line: 388, oneLine: 300, longestRun: 120 };
    layout = stubBannerTitleLayout(widths);
  });

  afterEach(() => {
    cleanup();
    layout.restore();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("keeps a title that fits on one line at its size class, with no inline size", () => {
    render(<FittedTitle text={LONG_TITLE} />);

    expect(screen.getByText(LONG_TITLE).style.fontSize).toBe("");
  });

  it("shrinks a title that overflows its line until it fits on one line", () => {
    widths.oneLine = 600;
    render(<FittedTitle text={LONG_TITLE} />);

    const title = screen.getByText(LONG_TITLE);
    expect(parseFloat(title.style.fontSize)).toBeCloseTo((48 * 387) / 600);
    // The measuring styles are gone, so the title lays out as its classes say.
    expect(title.style.position).toBe("");
    expect(title.style.width).toBe("");
  });

  it("holds the floor for a title that fits one line only below it, and leaves it to wrap", () => {
    // One line would need 48 × 387 / 1200 ≈ 15.5px, under the 28px floor.
    widths.oneLine = 1200;
    render(<FittedTitle text={LONG_TITLE} />);

    const title = screen.getByText(LONG_TITLE);
    expect(title.style.fontSize).toBe("28px");
    expect(title.style.whiteSpace).toBe("");
  });

  it("lowers the floor only as far as a run too wide for a line at it needs", () => {
    // A 700px unbreakable run (a one-word name) is 408px at 28px, wider than the
    // line, so the floor drops to 48 × 387 / 700 ≈ 26.5px, where the run fits.
    widths.oneLine = 1200;
    widths.longestRun = 700;
    render(<FittedTitle text={LONG_TITLE} />);

    expect(parseFloat(screen.getByText(LONG_TITLE).style.fontSize)).toBeCloseTo((48 * 387) / 700);
  });

  it("measures again when its line resizes, and only then", async () => {
    widths.oneLine = 600;
    render(<FittedTitle text={LONG_TITLE} />);
    const title = screen.getByText(LONG_TITLE);
    expect(parseFloat(title.style.fontSize)).toBeCloseTo((48 * 387) / 600);

    // A wider line, as when the phone turns to landscape. Another element's
    // resize leaves the title alone; its line's resize refits it.
    widths.line = 802;
    layout.reportResize(document.body);
    await layout.nextFrame();
    expect(parseFloat(title.style.fontSize)).toBeCloseTo((48 * 387) / 600);

    layout.reportResize(screen.getByTestId("line"));
    await layout.nextFrame();
    expect(title.style.fontSize).toBe("");
  });

  it("measures again when the page's fonts finish loading", async () => {
    render(<FittedTitle text={LONG_TITLE} />);
    const title = screen.getByText(LONG_TITLE);
    expect(title.style.fontSize).toBe("");

    // The loaded font runs wider than the fallback the first fit measured.
    widths.oneLine = 600;
    await layout.loadFonts();
    expect(parseFloat(title.style.fontSize)).toBeCloseTo((48 * 387) / 600);
  });

  it("measures again when the title's text changes", () => {
    const view = render(<FittedTitle text="YOUR TURN" />);
    expect(screen.getByText("YOUR TURN").style.fontSize).toBe("");

    widths.oneLine = 600;
    view.rerender(<FittedTitle text={LONG_TITLE} />);
    expect(parseFloat(screen.getByText(LONG_TITLE).style.fontSize)).toBeCloseTo((48 * 387) / 600);
  });
});
