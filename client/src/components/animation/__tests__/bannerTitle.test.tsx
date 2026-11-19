import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useBannerTitleFit } from "../bannerTitle";
import { stubBannerTitleLayout, type TitleWidths } from "../../../test/helpers/bannerTitleLayout";

const LONG_TITLE = "ALPHINAUD LEVEILLEUR'S TURN";

/** A 48px (`text-5xl`) title on its line, floored at 1.75rem (28px), as the turn banner's. */
function FittedTitle({ text, className = "" }: { text: string; className?: string }) {
  const titleRef = useBannerTitleFit(text, 1.75);
  return (
    <div data-testid="line">
      <span ref={titleRef} className={`text-5xl ${className}`}>
        {text}
      </span>
    </div>
  );
}

// happy-dom does no layout: each test sets the widths a browser would measure
// at the 48px size class (see `stubBannerTitleLayout`) and checks the size the
// hook gives the title. The title may fill its line less two pixels of slack
// for how browsers round those widths: 386px of the 388px line.
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
    expect(parseFloat(title.style.fontSize)).toBeCloseTo((48 * 386) / 600);
    // The measuring styles are gone, so the title lays out as its classes say.
    expect(title.style.position).toBe("");
    expect(title.style.width).toBe("");
  });

  it("holds the floor for a title that fits one line only below it, and leaves it to wrap", () => {
    // One line would need 48 × 386 / 1200 ≈ 15.4px, under the 28px floor.
    widths.oneLine = 1200;
    render(<FittedTitle text={LONG_TITLE} />);

    const title = screen.getByText(LONG_TITLE);
    expect(title.style.fontSize).toBe("28px");
    expect(title.style.whiteSpace).toBe("");
  });

  it("leaves two pixels of slack for how browsers round the widths it measures", () => {
    // Each reading can be off by just under a pixel. 386px measured fits the
    // 388px line. 387px could lay out just under 388px, on a line that could lay
    // out just over 387px, so it shrinks to fill the 386px the title may fill.
    widths.oneLine = 386;
    const view = render(<FittedTitle text="YOUR TURN" />);
    expect(screen.getByText("YOUR TURN").style.fontSize).toBe("");

    widths.oneLine = 387;
    view.rerender(<FittedTitle text={LONG_TITLE} />);
    expect(parseFloat(screen.getByText(LONG_TITLE).style.fontSize)).toBeCloseTo((48 * 386) / 387);
  });

  it("scales only the text, not the title's padding and border", () => {
    // 20px of padding and a 1px border each side: 42px that keep their size.
    // The text is 600 − 42 = 558px at 48px, and may fill 386 − 42 = 344px.
    const chrome = document.createElement("style");
    chrome.textContent =
      ".pill { padding-left: 20px; padding-right: 20px; border-left-width: 1px; border-right-width: 1px; " +
      "border-left-style: solid; border-right-style: solid; }";
    document.head.appendChild(chrome);
    widths.oneLine = 600;
    render(<FittedTitle text={LONG_TITLE} className="pill" />);

    expect(parseFloat(screen.getByText(LONG_TITLE).style.fontSize)).toBeCloseTo((48 * 344) / 558);
    chrome.remove();
  });

  it("lowers the floor only as far as a run too wide for a line at it needs", () => {
    // A 700px unbreakable run (a one-word name) is 408px at 28px, wider than the
    // line, so the floor drops to 48 × 386 / 700 ≈ 26.5px, where the run fits.
    widths.oneLine = 1200;
    widths.longestRun = 700;
    render(<FittedTitle text={LONG_TITLE} />);

    expect(parseFloat(screen.getByText(LONG_TITLE).style.fontSize)).toBeCloseTo((48 * 386) / 700);
  });

  it("measures again when its line resizes, and only then", async () => {
    widths.oneLine = 600;
    render(<FittedTitle text={LONG_TITLE} />);
    const title = screen.getByText(LONG_TITLE);
    expect(parseFloat(title.style.fontSize)).toBeCloseTo((48 * 386) / 600);

    // A wider line, as when the phone turns to landscape. Another element's
    // resize leaves the title alone; its line's resize refits it.
    widths.line = 802;
    layout.reportResize(document.body);
    await layout.nextFrame();
    expect(parseFloat(title.style.fontSize)).toBeCloseTo((48 * 386) / 600);

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
    expect(parseFloat(title.style.fontSize)).toBeCloseTo((48 * 386) / 600);
  });

  it("measures again when the title's text changes", () => {
    const view = render(<FittedTitle text="YOUR TURN" />);
    expect(screen.getByText("YOUR TURN").style.fontSize).toBe("");

    widths.oneLine = 600;
    view.rerender(<FittedTitle text={LONG_TITLE} />);
    expect(parseFloat(screen.getByText(LONG_TITLE).style.fontSize)).toBeCloseTo((48 * 386) / 600);
  });
});
