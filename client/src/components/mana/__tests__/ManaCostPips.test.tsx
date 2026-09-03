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
