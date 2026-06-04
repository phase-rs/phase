import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { ManaCost } from "../../../adapter/types";
import { ManaCostPips } from "../ManaCostPips";

// ManaSymbol renders an <img> from Scryfall SVG URLs — just assert the
// container structure without asserting the image src (network not available in tests).

const FREE: ManaCost = { type: "NoCost" };
const ZERO_COST: ManaCost = { type: "Cost", generic: 0, shards: [] };
const GENERIC_TWO: ManaCost = { type: "Cost", generic: 2, shards: [] };
const WW: ManaCost = { type: "Cost", generic: 0, shards: ["White", "White"] };
const TWO_WW: ManaCost = { type: "Cost", generic: 2, shards: ["White", "White"] };
const SELF_MANA: ManaCost = { type: "SelfManaCost" };

describe("ManaCostPips", () => {
  afterEach(() => {
    cleanup();
  });

  it("renders nothing for NoCost", () => {
    const { container } = render(<ManaCostPips cost={FREE} />);
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing for SelfManaCost", () => {
    const { container } = render(<ManaCostPips cost={SELF_MANA} />);
    expect(container.firstChild).toBeNull();
  });

  it("renders nothing for a zero-cost card without isReduced", () => {
    const { container } = render(<ManaCostPips cost={ZERO_COST} />);
    expect(container.firstChild).toBeNull();
  });

  it("renders a {0} pip for a reduced-to-zero cost when isReduced is true", () => {
    render(<ManaCostPips cost={ZERO_COST} isReduced={true} />);
    // ManaSymbol renders an img with alt text derived from the shard
    expect(screen.getByRole("img")).toBeInTheDocument();
  });

  it("renders one pip per generic + colored shard", () => {
    // 2WW → 3 pips: "2", "W", "W"
    render(<ManaCostPips cost={TWO_WW} />);
    const imgs = screen.getAllByRole("img");
    expect(imgs).toHaveLength(3);
  });

  it("renders only colored pips for a pure-colored cost", () => {
    // WW → 2 pips
    render(<ManaCostPips cost={WW} />);
    const imgs = screen.getAllByRole("img");
    expect(imgs).toHaveLength(2);
  });

  it("renders only a generic pip for a pure-generic cost", () => {
    // {2} → 1 pip
    render(<ManaCostPips cost={GENERIC_TWO} />);
    const imgs = screen.getAllByRole("img");
    expect(imgs).toHaveLength(1);
  });

  it("applies reduced ring styling when isReduced is true", () => {
    render(<ManaCostPips cost={WW} isReduced={true} />);
    // Each pip div should have the ring class
    const { container } = render(<ManaCostPips cost={WW} isReduced={true} />);
    const pipDivs = container.querySelectorAll('[class*="ring-green-400"]');
    expect(pipDivs.length).toBeGreaterThan(0);
  });

  it("does not apply reduced ring styling when isReduced is false", () => {
    const { container } = render(<ManaCostPips cost={WW} isReduced={false} />);
    const pipDivs = container.querySelectorAll('[class*="ring-green-400"]');
    expect(pipDivs).toHaveLength(0);
  });

  it("accepts a custom className on the wrapper", () => {
    const { container } = render(<ManaCostPips cost={WW} className="my-custom-class" />);
    const wrapper = container.firstChild as HTMLElement;
    expect(wrapper.className).toContain("my-custom-class");
  });
});
