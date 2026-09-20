import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { beforeAll, describe, expect, it, vi } from "vitest";

import init, { evaluate_deck_compatibility_js, load_card_database } from "@wasm/engine";
import type { ParsedDeck } from "../deckParser";

/**
 * CR 903.13f(3) end to end: the real `evaluateDeckCompatibility` builds the
 * request, the real engine answers it, and the wire token `draft_set_codes` is
 * the only thing that varies between the rows.
 *
 * A wire-key typo in client/src/services/deckCompatibility.ts ALONE needs no
 * integration lane: it reds the rows of
 * client/src/services/__tests__/deckCompatibility.test.ts that assert the
 * literal snake_case key. What only this file catches is a CONSISTENTLY
 * misspelled key — one the client and its own assertions share. Rename
 * `draft_set_codes` across `DeckCompatibilityRequest`, `buildRequest` and
 * `compatibilityCacheKey` AND across those assertions: that file stays green
 * and so does client/src/components/draft/__tests__/LimitedDeckBuilder.test.tsx
 * (which asserts the camelCase option, not the wire key), while the row below
 * reds with the production refusal string. The engine's
 * `DeckCompatibilityRequest` in crates/engine/src/game/deck_validation.rs
 * carries no `#[serde(deny_unknown_fields)]` and marks `draft_set_codes`
 * `#[serde(default)]`, so the misspelled field is dropped, the real one
 * defaults to empty, and the request still deserializes into a verdict.
 *
 * Integration lane — no CI job runs this file. Run it by hand with
 * `cd client && npx vitest run --config vitest.integration.config.ts
 * --coverage.enabled=false
 * src/services/__tests__/deckCompatibility.integration.test.ts`. Both inputs it
 * needs are gitignored build outputs, so the suite self-skips when they are
 * absent.
 */

const WASM_PATH = resolve(__dirname, "../../wasm/engine_wasm_bg.wasm");
const CARD_DATA_PATH = resolve(__dirname, "../../../public/card-data.json");
const INPUTS_PRESENT = existsSync(WASM_PATH) && existsSync(CARD_DATA_PATH);

const adapterStub = vi.hoisted(() => ({
  cardDbLoaded: true,
  checkDeckCompatibility: vi.fn(),
}));

vi.mock("../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => adapterStub,
}));

/**
 * Commanders INSIDE the 60, per CR 903.13f(1) ("at least 60 cards") and
 * CR 903.13f(2) (any number of same-name cards from the pool).
 *
 * Neither legend is printed in Commander Masters — `jq -r '.data.cards[].name'
 * data/mtgjson/sets/CMM.json | grep -cE 'Isamaru, Hound of Konda|Odric,
 * Lunarch Marshal'` prints 0, where that same pipeline prints 1 for `The
 * Prismatic Piper` — and that is the point: CR 903.13f(3) conditions on what
 * the DRAFT contained, never on a card's printing, and the engine deliberately
 * does not consult printings.
 *
 * Neither carries a keyword, so neither has a printed Partner of its own and a
 * pairing can only come from the grant:
 * `jq -c '.["isamaru, hound of konda"].keywords' client/public/card-data.json`
 * prints `[]`, likewise `.["odric, lunarch marshal"]`, where a printed Partner
 * appears in that array as `{"Partner":{"type":"Generic"}}` — compare
 * `.["thrasios, triton hero"].keywords`.
 */
const FIRST_COMMANDER = "Isamaru, Hound of Konda";
const SECOND_COMMANDER = "Odric, Lunarch Marshal";
const DECK: ParsedDeck = {
  main: [
    { name: FIRST_COMMANDER, count: 1 },
    { name: SECOND_COMMANDER, count: 1 },
    { name: "Plains", count: 58 },
  ],
  sideboard: [],
  commander: [FIRST_COMMANDER, SECOND_COMMANDER],
};

function partnerReason(reasons: readonly string[]): string | undefined {
  return reasons.find((reason) => /partner/i.test(reason));
}

describe.skipIf(!INPUTS_PRESENT)("deck compatibility — CR 903.13f(3) over the real engine", () => {
  beforeAll(async () => {
    const bytes = await readFile(WASM_PATH);
    const module = await WebAssembly.compile(bytes);
    await init({ module_or_path: module });
    load_card_database(await readFile(CARD_DATA_PATH, "utf8"));
    adapterStub.checkDeckCompatibility.mockImplementation(
      async (request: unknown) => evaluate_deck_compatibility_js(request),
    );
  }, 300_000);

  /**
   * The empty-codes row is the negative control AND the positive reach guard
   * for the granting row: it asserts this deck is refused with a Partner
   * reason when no draft is behind it — point that row's `draftSetCodes` at
   * ["CMM"] and it reds under the command this file's header names — so the
   * granting row's clean verdict is the grant and not an unrelated pass.
   */
  it("grants Partner for a CMM draft and withholds it otherwise", async () => {
    const { evaluateDeckCompatibility } = await import("../deckCompatibility");

    const noDraft = await evaluateDeckCompatibility(DECK, {
      selectedFormat: "CommanderDraft",
      draftSetCodes: [],
    });
    expect(noDraft.selected_format_compatible).not.toBe(true);
    expect(partnerReason(noDraft.selected_format_reasons)).toBeDefined();

    const cmmDraft = await evaluateDeckCompatibility(DECK, {
      selectedFormat: "CommanderDraft",
      draftSetCodes: ["CMM"],
    });
    expect(cmmDraft.selected_format_reasons).toEqual([]);
    expect(cmmDraft.selected_format_compatible).toBe(true);

    // Separates "a set code is present" from "THIS set code concedes": the
    // grant is a property of the named set, not of the list being non-empty.
    const unknownSet = await evaluateDeckCompatibility(DECK, {
      selectedFormat: "CommanderDraft",
      draftSetCodes: ["NOT_A_SET"],
    });
    expect(unknownSet.selected_format_compatible).not.toBe(true);
    expect(partnerReason(unknownSet.selected_format_reasons)).toBeDefined();
  }, 300_000);
});
