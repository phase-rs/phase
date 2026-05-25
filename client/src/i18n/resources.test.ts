import { describe, expect, it } from "vitest";

import { detectInitialLanguage, resources, SUPPORTED_LNGS } from "./resources";

// Gate test (plan §9 Phase 0 step 1): proves Vite's import.meta.glob runs under
// vitest's transform pipeline AND that the reshape yields { lng: { ns: {...} } }.
// Both the runtime catalogs and the "don't mock t, keep getByText" test strategy
// depend on this, so it must pass before anything else is built on the glob.
describe("i18n resources", () => {
  it("aggregates locale JSON into a { lng: { ns: {...} } } shape", () => {
    expect(resources.en).toBeDefined();
    expect(resources.en.common).toMatchObject({ actions: { cancel: "Cancel" } });
  });

  it("derives every populated locale into the resources map", () => {
    // Every glob-discovered locale directory must be a known supported language.
    for (const lng of Object.keys(resources)) {
      expect(SUPPORTED_LNGS as readonly string[]).toContain(lng);
    }
  });

  it("detects a supported language or falls back to en", () => {
    expect(SUPPORTED_LNGS as readonly string[]).toContain(detectInitialLanguage());
  });
});
