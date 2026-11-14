import { describe, expect, it } from "vitest";

import { cardCandidateGroups } from "../candidateKeys.ts";
import { isPackedContentLocale, PACKED_CONTENT_LOCALES } from "../types.ts";

/**
 * A shipped UI locale is not necessarily a locale that installed packs carry
 * content for. `pl` has been in that position for a while and `zh` joins it once
 * its catalogs ship, while `SUPPORTED_LNGS` describes only what the *interface*
 * can be read in.
 *
 * `encodeCandidateKey` validates a localized key against `PACKED_CONTENT_LOCALES`
 * and throws otherwise. Building that key unconditionally therefore threw for
 * `zh`/`pl`, and the callers catch that and degrade to no candidates at all —
 * which discarded the English and oracle groups that were perfectly resolvable.
 * The consequence in production: a player selecting `zh` with an English
 * printing installed only in a `printing/complete` pack lost that installed
 * candidate and fell through to an offline fallback.
 *
 * These tests are builder controls: they pin the group shapes for an unsupported
 * locale, a supported one, and the retained English/oracle groups.
 */
const PRINTING = "11111111-abcd-4111-8111-111111111111";
const ORACLE = "22222222-abcd-4222-8222-222222222222";

function build(language?: string) {
  return cardCandidateGroups({
    language,
    englishPrintingId: PRINTING,
    oracleId: ORACLE,
    faceIndex: 0,
    variant: "full_card",
    rung: "large",
  });
}

/** Kinds present in a built group set, in build order. */
function kinds(language?: string): string[] {
  return build(language).flatMap((group) =>
    group.keys.map((key) => key.split(":")[2] ?? ""),
  );
}

describe("cardCandidateGroups and packed content locales", () => {
  it("keeps the two locale lists distinct", () => {
    // The regression only makes sense while `zh` is a UI locale without packed
    // content. If a pack locale list ever gains `zh`, this test should be
    // revisited rather than silently passing for a different reason.
    expect(PACKED_CONTENT_LOCALES).not.toContain("zh");
    expect(PACKED_CONTENT_LOCALES).not.toContain("pl");
    expect(isPackedContentLocale("zh")).toBe(false);
    expect(isPackedContentLocale("pl")).toBe(false);
    expect(isPackedContentLocale("de")).toBe(true);
  });

  it("retains the English and oracle groups for an unsupported packed locale (zh)", () => {
    const built = kinds("zh");

    // The localized group is correctly absent...
    expect(built).not.toContain("localized_printing");
    expect(built).not.toContain("localized_alias");
    // ...and the resolvable groups SURVIVE. This is the assertion that fails
    // when the unsupported key is encoded and the throw wipes the whole set.
    expect(built).toContain("english_printing");
    expect(built).toContain("oracle");
  });

  it("retains the English and oracle groups for the other unsupported locale (pl)", () => {
    // `pl` has the same shape and predates this change, so the fix is shared
    // rather than special-cased to `zh`.
    const built = kinds("pl");

    expect(built).not.toContain("localized_printing");
    expect(built).toContain("english_printing");
    expect(built).toContain("oracle");
  });

  it("includes the localized group for a supported packed locale (de)", () => {
    const built = kinds("de");

    expect(built).toContain("localized_printing");
    // The fallbacks remain, so a pack with no German rendition still resolves.
    expect(built).toContain("english_printing");
    expect(built).toContain("oracle");
  });

  it("builds no localized group for English", () => {
    const built = kinds("en");

    expect(built).not.toContain("localized_printing");
    expect(built).toContain("english_printing");
    expect(built).toContain("oracle");
  });
});
