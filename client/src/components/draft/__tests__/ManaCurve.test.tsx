import { describe, expect, it } from "vitest";
import { render, within } from "@testing-library/react";

import type { DraftCardInstance } from "../../../adapter/draft-adapter";
import { ManaCurve } from "../ManaCurve";

const DISTRIBUTION = [
  { color: "Blue" as const, count: 1, percentage: 50, display_percentage: 50 },
  { color: "Red" as const, count: 1, percentage: 50, display_percentage: 50 },
];

const POOL: DraftCardInstance[] = [
  {
    instance_id: "one-drop",
    name: "One Drop",
    set_code: "tst",
    collector_number: "1",
    rarity: "common" as const,
    colors: ["U"],
    cmc: 1,
    type_line: "Creature",
  },
  {
    instance_id: "three-drop",
    name: "Three Drop",
    set_code: "tst",
    collector_number: "2",
    rarity: "common" as const,
    colors: ["R"],
    cmc: 3,
    type_line: "Creature",
  },
  {
    instance_id: "land",
    name: "Red Land",
    set_code: "tst",
    collector_number: "3",
    rarity: "common" as const,
    colors: [],
    cmc: 0,
    type_line: "Land",
  },
];

function meterSemantics(curve: HTMLElement) {
  return within(curve).getAllByRole("meter").map((meter) => ({
    label: meter.getAttribute("aria-label"),
    min: meter.getAttribute("aria-valuemin"),
    max: meter.getAttribute("aria-valuemax"),
    now: meter.getAttribute("aria-valuenow"),
  }));
}

describe("ManaCurve", () => {
  it("keeps all seven meter semantics in the compact presentation", () => {
    const { container } = render(
      <>
        <ManaCurve pool={[...POOL]} cards={["One Drop", "Three Drop"]} colorDistribution={DISTRIBUTION} />
        <ManaCurve pool={[...POOL]} cards={["One Drop", "Three Drop"]} colorDistribution={DISTRIBUTION} presentation="compact" />
      </>,
    );

    const defaultCurve = container.querySelector<HTMLElement>("[data-mana-curve-presentation='default']")!;
    const compactCurve = container.querySelector<HTMLElement>("[data-mana-curve-presentation='compact']")!;
    expect(meterSemantics(compactCurve)).toEqual(meterSemantics(defaultCurve));
    expect(within(compactCurve).getAllByRole("meter")).toHaveLength(7);
    expect(within(compactCurve).getByText("Mana Curve")).toBeInTheDocument();
  });

  it("uses explicit compact geometry while retaining the full curve labels", () => {
    const { container } = render(
      <>
        <ManaCurve pool={[...POOL]} cards={["One Drop"]} colorDistribution={DISTRIBUTION} />
        <ManaCurve pool={[...POOL]} cards={["One Drop"]} colorDistribution={DISTRIBUTION} presentation="compact" />
      </>,
    );

    const defaultCurve = container.querySelector<HTMLElement>("[data-mana-curve-geometry='default']")!;
    const compactCurve = container.querySelector<HTMLElement>("[data-mana-curve-geometry='compact']")!;
    expect(defaultCurve.querySelector<HTMLElement>("[data-mana-curve-plot]")).toHaveStyle({ height: "124px" });
    expect(compactCurve.querySelector<HTMLElement>("[data-mana-curve-plot]")).toHaveStyle({ height: "52px" });
    expect(compactCurve).toHaveClass("gap-0.5");
    expect(compactCurve.querySelector("[data-mana-curve-title]")).toHaveClass("leading-none");
    expect(defaultCurve.querySelectorAll("[data-mana-curve-count]")).toHaveLength(7);
    expect(defaultCurve.querySelectorAll("[data-mana-curve-bucket]")).toHaveLength(7);
    expect(compactCurve.querySelectorAll("[data-mana-curve-count]")).toHaveLength(7);
    expect(compactCurve.querySelectorAll("[data-mana-curve-bucket]")).toHaveLength(7);
    expect(Array.from(compactCurve.querySelectorAll("[data-mana-curve-bucket]"), (bucket) => bucket.textContent))
      .toEqual(["0", "1", "2", "3", "4", "5", "6+"]);
    expect(compactCurve.querySelector("[data-mana-curve-meter='1'] [data-mana-curve-count]"))
      .toHaveTextContent("1");
  });

  it("renders the supplied engine distribution independently of the mana curve", () => {
    const { container } = render(
      <ManaCurve
        pool={POOL}
        cards={["One Drop", "Red Land"]}
        colorDistribution={[
          { color: "Blue", count: 1, percentage: 33.25, display_percentage: 33 },
          { color: "Red", count: 2, percentage: 66.75, display_percentage: 67 },
        ]}
      />,
    );

    expect(container.querySelector("[data-mana-curve-meter='0']"))
      .toHaveAttribute("aria-valuenow", "0");
    expect(container.querySelector("[data-mana-curve-meter='1']"))
      .toHaveAttribute("aria-valuenow", "1");
    expect(container.querySelector("[data-color-distribution]")).toHaveTextContent("U 33%");
    expect(container.querySelector("[data-color-distribution]")).toHaveTextContent("R 67%");
  });
});
