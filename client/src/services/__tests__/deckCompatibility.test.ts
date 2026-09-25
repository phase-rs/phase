import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ParsedDeck } from "../deckParser";

/**
 * CR 903.13f(3) reaches the deck-compatibility request. These rows drive the
 * real `evaluateDeckCompatibility`, so `buildRequest` and
 * `compatibilityCacheKey` both run unmocked; only the adapter is replaced, and
 * what it receives is the payload the engine worker would have received.
 *
 * Run with
 * `cd client && npx vitest run --coverage.enabled=false
 * src/services/__tests__/deckCompatibility.test.ts`.
 */

const adapterMock = vi.hoisted(() => ({
  cardDbLoaded: true,
  checkDeckCompatibility: vi.fn(),
}));

vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => adapterMock,
}));

const compatibleResult = () => ({
  standard: { compatible: true, reasons: [] },
  commander: { compatible: true, reasons: [] },
  bo3_ready: true,
  unknown_cards: [],
  selected_format_compatible: true,
  selected_format_reasons: [],
  color_identity: [],
  color_distribution: [],
});

const DECK: ParsedDeck = {
  main: [{ name: "Isamaru, Hound of Konda", count: 1 }, { name: "Plains", count: 59 }],
  sideboard: [],
  commander: ["Isamaru, Hound of Konda"],
};

/**
 * The four compatibility caches are module-scope `Map`s, so a fresh module
 * registry per test is what keeps one row's cached verdict out of the next
 * row's call count. Same `vi.resetModules()` + dynamic-import shape as
 * `loadRuntime` in client/src/services/__tests__/engineRuntime.test.ts.
 */
async function loadDeckCompatibility() {
  vi.resetModules();
  return import("../deckCompatibility.ts");
}

beforeEach(() => {
  adapterMock.checkDeckCompatibility.mockReset();
  adapterMock.checkDeckCompatibility.mockImplementation(async () => compatibleResult());
});

describe("evaluateDeckCompatibility — CR 903.13f(3) draft set codes", () => {
  it("sends the drafted set codes on the request the adapter receives", async () => {
    const { evaluateDeckCompatibility } = await loadDeckCompatibility();

    await evaluateDeckCompatibility(DECK, {
      selectedFormat: "CommanderDraft",
      draftSetCodes: ["CMM"],
    });

    expect(adapterMock.checkDeckCompatibility).toHaveBeenCalledTimes(1);
    expect(adapterMock.checkDeckCompatibility).toHaveBeenCalledWith(
      expect.objectContaining({ draft_set_codes: ["CMM"] }),
    );
  });

  /**
   * The paired sibling, stated as an exact value rather than a bare negative:
   * constructed play passes no codes and the engine must see an empty list,
   * which is what its `#[serde(default)]` would have produced anyway.
   */
  it("sends an empty list when the caller passes no codes", async () => {
    const { evaluateDeckCompatibility } = await loadDeckCompatibility();

    await evaluateDeckCompatibility(DECK, { selectedFormat: "Commander" });

    expect(adapterMock.checkDeckCompatibility).toHaveBeenCalledWith(
      expect.objectContaining({ draft_set_codes: [] }),
    );
  });

  /**
   * Hostile fixture: the engine owns set-code matching, so the client forwards
   * the token exactly as the view published it. A client that upper-cased
   * would send something other than the literal here.
   */
  it("forwards a lowercase set code unchanged", async () => {
    const { evaluateDeckCompatibility } = await loadDeckCompatibility();

    await evaluateDeckCompatibility(DECK, {
      selectedFormat: "CommanderDraft",
      draftSetCodes: ["cmm"],
    });

    expect(adapterMock.checkDeckCompatibility).toHaveBeenCalledWith(
      expect.objectContaining({ draft_set_codes: ["cmm"] }),
    );
  });

  /**
   * The cache-separation row the comment in `compatibilityCacheKey` names.
   * The 1 -> 2 transition is the in-test positive reach guard for the "still
   * twice" negative that follows it: without it, a row where nothing was ever
   * cached would satisfy the negative too.
   */
  it("separates cached verdicts on the drafted set codes", async () => {
    const { evaluateDeckCompatibility } = await loadDeckCompatibility();

    await evaluateDeckCompatibility(DECK, {
      selectedFormat: "CommanderDraft",
      draftSetCodes: ["CMM"],
    });
    expect(adapterMock.checkDeckCompatibility).toHaveBeenCalledTimes(1);

    // Same deck, same format, different codes — a miss, because the key names
    // the codes.
    await evaluateDeckCompatibility(DECK, {
      selectedFormat: "CommanderDraft",
      draftSetCodes: [],
    });
    expect(adapterMock.checkDeckCompatibility).toHaveBeenCalledTimes(2);

    // Back to the first codes — a hit, so the two verdicts are held apart
    // rather than one having overwritten the other.
    await evaluateDeckCompatibility(DECK, {
      selectedFormat: "CommanderDraft",
      draftSetCodes: ["CMM"],
    });
    expect(adapterMock.checkDeckCompatibility).toHaveBeenCalledTimes(2);
  });

  /**
   * `summary_only` stays out of the key on purpose: a summary request reads a
   * full request's cached verdict. The final different-codes call is this row's
   * positive reach guard — it shows the count can still move.
   */
  it("lets a summary request read the full request's cached verdict", async () => {
    const { evaluateDeckCompatibility } = await loadDeckCompatibility();

    await evaluateDeckCompatibility(DECK, {
      selectedFormat: "CommanderDraft",
      draftSetCodes: ["CMM"],
    });
    expect(adapterMock.checkDeckCompatibility).toHaveBeenCalledTimes(1);

    await evaluateDeckCompatibility(DECK, {
      selectedFormat: "CommanderDraft",
      draftSetCodes: ["CMM"],
      summaryOnly: true,
    });
    expect(adapterMock.checkDeckCompatibility).toHaveBeenCalledTimes(1);

    await evaluateDeckCompatibility(DECK, {
      selectedFormat: "CommanderDraft",
      draftSetCodes: ["CLB", "CMM"],
      summaryOnly: true,
    });
    expect(adapterMock.checkDeckCompatibility).toHaveBeenCalledTimes(2);
  });
});
