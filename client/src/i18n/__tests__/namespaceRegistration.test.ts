import i18n from "i18next";
import { describe, expect, it } from "vitest";

/**
 * Guards the one registration surface nothing else in this repo reaches.
 *
 * `resources.test.ts` and `localeParity.test.ts` both read the catalog files
 * off disk — the latter builds its own `createInstance()` — so neither observes
 * whether `test-setup.ts` actually registered a namespace. `react-i18next.d.ts`
 * cannot cover the gap either: i18next v26 declares `CustomTypeOptions` in
 * module "i18next" while this repo augments module "react-i18next", so that
 * oracle is inert and `tsc` proves nothing about key resolution. Measured:
 * deregistering an already-shipped namespace from `test-setup.ts` leaves all
 * three of those green, and the failure it lets through is silent — an
 * unregistered lookup renders its bare key path with the `ns:` prefix stripped,
 * which reads as plausible UI text. `replay` is the live instance.
 *
 * `test-setup.ts` initialises the DEFAULT `i18next` export (not a
 * `createInstance()`), so importing it here is importing the configured harness
 * instance — the same idiom as
 * `components/chrome/__tests__/DebugLibraryViewer.focus.test.tsx`.
 */
describe("test-harness i18n registration", () => {
  it("resolves_the_tournament_namespace_through_the_test_harness_instance", () => {
    // Reach-guard: prove the negative below is not vacuous. A namespace that is
    // registered nowhere resolves to its bare key path, `ns:` prefix stripped.
    expect(i18n.t("definitelyNotARealNamespace:someKey")).toBe("someKey");

    // The claim: `tournament` IS registered, so this is NOT the bare key path.
    expect(i18n.t("tournament:list.heading")).not.toBe("list.heading");
  });
});
