import type { TFunction } from "i18next";
import { describe, it, expect } from "vitest";
import type {
  SearchSelectionConstraint,
  WaitingFor,
} from "../../../../adapter/types.ts";
import { searchChoiceSubtitle } from "../shared.tsx";

type SearchChoiceData = Extract<WaitingFor, { type: "SearchChoice" }>["data"];

// Fake `t` that returns the translation key, so the test pins which string the
// subtitle selects without depending on locale resources.
const t = ((key: string) => key) as unknown as TFunction<"game">;

const subtitle = (
  constraint: SearchSelectionConstraint | undefined,
  overrides: Partial<SearchChoiceData> = {},
): string =>
  searchChoiceSubtitle(
    {
      player: 0,
      cards: [],
      count: 2,
      up_to: true,
      constraint,
      ...overrides,
    },
    t,
  );

describe("searchChoiceSubtitle", () => {
  it("keeps the distinct-qualities wording for a DoesNotShare constraint", () => {
    const constraint: SearchSelectionConstraint = {
      type: "SharesQualities",
      qualities: ["Name"],
      relation: "DoesNotShare",
    };
    expect(subtitle(constraint)).toBe("cardChoice.search.subtitleDistinctUpTo");
    expect(subtitle(constraint, { up_to: false, allows_partial_find: false })).toBe(
      "cardChoice.search.subtitleDistinctUpTo",
    );
  });

  it("uses the generic count wording for a Shares constraint", () => {
    const constraint: SearchSelectionConstraint = {
      type: "SharesQualities",
      qualities: ["LandType"],
      relation: "Shares",
    };
    expect(subtitle(constraint)).toBe("cardChoice.search.subtitleUpTo");
    expect(subtitle(constraint)).not.toContain("Distinct");
  });

  it("falls back to the plain count wording without a constraint", () => {
    expect(subtitle(undefined)).toBe("cardChoice.search.subtitleUpTo");
    expect(subtitle({ type: "None" }, { up_to: false })).toBe(
      "cardChoice.search.subtitleExact",
    );
  });
});
