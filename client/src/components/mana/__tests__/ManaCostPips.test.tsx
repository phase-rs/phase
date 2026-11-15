import { cleanup, render, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ManaCost } from "../../../adapter/types.ts";
import { ManaCostPips } from "../ManaCostPips.tsx";

const useManaSymbolImage = vi.hoisted(() => vi.fn());

vi.mock("../../../hooks/useFixedVisualImage.ts", () => ({
  useManaSymbolImage,
}));

beforeEach(() => {
  useManaSymbolImage.mockImplementation((shard: string | null) => ({
    src: shard ? `visual-pack://mana/${encodeURIComponent(shard)}` : null,
    isLoading: false,
    advanceFailedSource: vi.fn(),
  }));
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

/** Reads one of this component's own declared geometry values off the DOM. */
function geometry(pattern: RegExp, className: string): number {
  const match = pattern.exec(className);
  if (!match) throw new Error(`no ${pattern} in ${className}`);
  return Number(match[1]);
}

const RIGHT_OFFSET = /right-\[([\d.]+)%\]/;
const PIP_WIDTH = /w-\[([\d.]+)cqi\]/;

const printedCost: ManaCost = { type: "Cost", shards: ["Red"], generic: 1 };

describe("ManaCostPips over card art", () => {
  it("keeps a free-cast {0} clear of the printed cost instead of covering it", () => {
    // A cost reduced to {0} says nothing the offer doesn't already say, while
    // the mana VALUE underneath it is what alternative costs are measured in
    // (Amped Raptor pays {E} equal to it, Nashi pays that much life). So this
    // badge parks in the frame's right margin.
    const printed = render(<ManaCostPips cost={printedCost} size="fluid" />);
    const printedEdge = geometry(RIGHT_OFFSET, printed.container.firstElementChild!.className);

    const free = render(<ManaCostPips cost={{ type: "NoCost" }} isReduced size="fluid" />);
    const badge = free.container.firstElementChild!;
    const pip = within(free.container).getByAltText("0").parentElement!;

    // 1cqi is 1% of the card's width, so the two units are comparable: the
    // whole free badge fits between the card edge and the printed cost's own
    // right edge, which is where the ordinary badge is anchored.
    expect(
      geometry(RIGHT_OFFSET, badge.className) + geometry(PIP_WIDTH, pip.className),
    ).toBeLessThanOrEqual(printedEdge);
  });

  it("still anchors a reduced cost on the printed cost — that is what is paid", () => {
    // Only the {0} badge leaves the printed cost. A cost reduced to a real
    // amount REPLACES the printed one, so covering it is the whole point of
    // the overlay — it must stay farther from the card edge than the {0}.
    const free = render(<ManaCostPips cost={{ type: "NoCost" }} isReduced size="fluid" />);
    const reduced = render(
      <ManaCostPips cost={{ type: "Cost", shards: [], generic: 3 }} isReduced size="fluid" />,
    );

    expect(
      geometry(RIGHT_OFFSET, reduced.container.firstElementChild!.className),
    ).toBeGreaterThan(geometry(RIGHT_OFFSET, free.container.firstElementChild!.className));
  });

  it("renders nothing for a naturally free card — no badge to place", () => {
    const { container } = render(<ManaCostPips cost={{ type: "NoCost" }} size="fluid" />);

    expect(container.firstElementChild).toBeNull();
  });
});

describe("ManaCostPips for a card with two castable spell faces", () => {
  // CR 709.3 + CR 712.11b: a Room, split card or spell//spell MDFC has two
  // payable costs; the engine publishes the other face's live cost and the
  // badge shows both, `front // back`.
  const front: ManaCost = { type: "Cost", shards: ["White", "White"], generic: 2 };
  const back: ManaCost = { type: "Cost", shards: ["Blue", "Blue"], generic: 5 };

  it("shows both faces' symbols with a // between them", () => {
    const { container } = render(
      <ManaCostPips cost={front} backFace={{ cost: back }} size="fluid" />,
    );
    const alts = within(container).getAllByRole("img").map((el) => el.getAttribute("alt"));

    expect(alts).toEqual(["2", "W", "W", "5", "U", "U"]);
    expect(container.querySelector("[data-mana-cost-face-separator]")?.textContent).toBe("//");
  });

  it("keeps the single badge's own pip for a pair the frame affords — four symbols", () => {
    // 4 * (6 + 0.5) + 2.4 = 28.4cqi: no reason to shrink Fire // Ice.
    const single = render(<ManaCostPips cost={printedCost} size="fluid" />);
    const pair = render(
      <ManaCostPips
        cost={{ type: "Cost", shards: ["Red"], generic: 1 }}
        backFace={{ cost: { type: "Cost", shards: ["Blue"], generic: 1 } }}
        size="fluid"
      />,
    );
    const pipWidth = (c: HTMLElement) =>
      geometry(PIP_WIDTH, within(c).getAllByRole("img")[0].parentElement!.className);

    expect(pipWidth(pair.container)).toBe(pipWidth(single.container));
  });

  it.each([
    // From five symbols on the pip shrinks: 5, then the corpus's widest pairs
    // — 6 (Restricted Office // Lecture Hall), 7 (Expansion // Explosion) and
    // 8 (Esika, God of the Tree // The Prismatic Bridge). Each must stay
    // inside the 32cqi a lone five-symbol cost may span, or the pair runs
    // through the card name. Pip width, gap and the separator's declared width
    // are read off the DOM; the row holds the pips AND the separator, so it
    // has as many gaps as pips.
    [
      "5",
      { type: "Cost", shards: ["Red"], generic: 1 } as ManaCost,
      { type: "Cost", shards: ["Blue", "Blue"], generic: 2 } as ManaCost,
    ],
    ["6", front, back],
    [
      "7",
      { type: "Cost", shards: ["BlueRed", "BlueRed"], generic: 0 } as ManaCost,
      { type: "Cost", shards: ["X", "Blue", "Blue", "Red", "Red"], generic: 0 } as ManaCost,
    ],
    [
      "8",
      { type: "Cost", shards: ["Green", "Green"], generic: 1 } as ManaCost,
      { type: "Cost", shards: ["White", "Blue", "Black", "Red", "Green"], generic: 0 } as ManaCost,
    ],
  ])("keeps a %s-symbol pair no wider than a single five-symbol cost", (_symbols, frontCost, backCost) => {
    const { container } = render(
      <ManaCostPips cost={frontCost} backFace={{ cost: backCost }} size="fluid" />,
    );
    const pips = within(container).getAllByRole("img").map((img) => img.parentElement!);
    const row = pips[0].parentElement!;
    const width = geometry(PIP_WIDTH, pips[0].className);
    const gap = geometry(/gap-\[([\d.]+)cqi\]/, row.className);
    const separator = geometry(
      /w-\[([\d.]+)cqi\]/,
      container.querySelector("[data-mana-cost-face-separator]")!.className,
    );

    expect(pips.length * (width + gap) + separator).toBeLessThanOrEqual(32);
  });

  it("clamps a pair past the corpus maximum to the smallest tier instead of vanishing", () => {
    // A live front that grew a symbol can push a pair to nine; it renders at
    // the 8-symbol tier and, as documented, may exceed the 32cqi budget.
    const { container } = render(
      <ManaCostPips
        cost={{ type: "Cost", shards: ["Green", "Green", "Green"], generic: 1 }}
        backFace={{ cost: { type: "Cost", shards: ["White", "Blue", "Black", "Red", "Green"], generic: 0 } }}
        size="fluid"
      />,
    );
    const eight = render(
      <ManaCostPips
        cost={{ type: "Cost", shards: ["Green", "Green"], generic: 1 }}
        backFace={{ cost: { type: "Cost", shards: ["White", "Blue", "Black", "Red", "Green"], generic: 0 } }}
        size="fluid"
      />,
    );
    const pipWidth = (c: HTMLElement) =>
      geometry(PIP_WIDTH, within(c).getAllByRole("img")[0].parentElement!.className);

    expect(within(container).getAllByRole("img")).toHaveLength(9);
    expect(pipWidth(container)).toBe(pipWidth(eight.container));
  });

  it("draws nothing for a back face without a front — a lone number names no face", () => {
    const { container } = render(
      <ManaCostPips cost={{ type: "NoCost" }} backFace={{ cost: back }} size="fluid" />,
    );

    expect(container.firstElementChild).toBeNull();
  });

  it("ignores the back face outside the fluid size — fixed-px pips have no width budget", () => {
    const { container } = render(<ManaCostPips cost={front} backFace={{ cost: back }} size="xs" />);

    expect(within(container).getAllByRole("img")).toHaveLength(3);
    expect(container.querySelector("[data-mana-cost-face-separator]")).toBeNull();
  });

  it("rings only the face the engine reduced", () => {
    const { container } = render(
      <ManaCostPips cost={front} backFace={{ cost: back, isReduced: true }} size="fluid" />,
    );
    const ringed = (alt: string) =>
      within(container).getAllByAltText(alt)[0].parentElement!.className.includes("ring-green-400");

    expect(ringed("W")).toBe(false);
    expect(ringed("U")).toBe(true);
  });
});
