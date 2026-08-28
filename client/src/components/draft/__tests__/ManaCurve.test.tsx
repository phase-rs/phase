import { describe, expect, it } from "vitest";
import { render, within } from "@testing-library/react";

import type { DraftCardInstance } from "../../../adapter/draft-adapter";
import { ManaCurve } from "../ManaCurve";

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
        <ManaCurve pool={[...POOL]} cards={["One Drop", "Three Drop"]} />
        <ManaCurve pool={[...POOL]} cards={["One Drop", "Three Drop"]} presentation="compact" />
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
        <ManaCurve pool={[...POOL]} cards={["One Drop"]} />
        <ManaCurve pool={[...POOL]} cards={["One Drop"]} presentation="compact" />
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
});
