import { beforeEach, describe, expect, it, vi } from "vitest";

import type { PrintingEntry } from "../../scryfall.ts";
import { STORAGE_KEY_PREFIX } from "../../../constants/storage.ts";
import type { DeckEntry } from "../../deckParser.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import type { ArtChainEntry } from "../../../stores/preferencesStore.ts";
import { planCuratedMembership } from "../curatedMembership.ts";
import { planCuratedPack } from "../curatedPack.ts";

/**
 * The card data the planner reads, swapped per test.
 *
 * Partial mock: `curatedMembership.ts` also imports `deriveImageUrl` from this
 * module at runtime, so replacing the whole module would break the very
 * planner under test.
 */
const data = vi.hoisted(() => ({
  cards: null as unknown,
  printings: null as unknown,
  /** Held open by the window test below, so the plan can be parked exactly
   *  where production parks it: inside `loadScryfallData`. */
  gate: Promise.resolve(),
}));

vi.mock("../../scryfall.ts", async (importOriginal) => ({
  ...await importOriginal<typeof import("../../scryfall.ts")>(),
  loadScryfallData: async () => {
    await data.gate;
    return data.cards;
  },
  loadPrintingsData: async () => data.printings,
}));

/** The real planner, counted. This is the 130-330 ms the memo exists to skip;
 *  nothing cheaper is a proxy for it, because the ~76 MB card maps behind it
 *  are module-level memoized promises and are already resident. */
vi.mock("../curatedMembership.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../curatedMembership.ts")>();
  return { ...actual, planCuratedMembership: vi.fn(actual.planCuratedMembership) };
});

const BOLT = "11111111-abcd-4111-8111-111111111111";
const GIANT = "22222222-abcd-4222-8222-222222222222";

function url(token: string, size: string): string {
  return `https://cards.scryfall.io/${size}/front/a/b/${token}.jpg`;
}

function imageFace(token: string) {
  return { normal: url(token, "normal"), art_crop: url(token, "art_crop") };
}

function printing(id: string, set: string, releasedAt: string): PrintingEntry {
  return {
    id,
    set,
    set_name: set.toUpperCase(),
    collector_number: "1",
    released_at: releasedAt,
    border_color: "black",
    frame_effects: [],
    full_art: false,
    faces: [imageFace(id)],
  };
}

const BOLT_NEW = printing("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "m20", "2019-07-12");
const BOLT_OLD = printing("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "lea", "1993-08-05");

function cardEntry(oracleId: string, name: string, faceName: string) {
  return { oracle_id: oracleId, name, face_names: [faceName], faces: [imageFace(oracleId)] };
}

const BOLT_ENTRY = cardEntry(BOLT, "Lightning Bolt", "lightning bolt");
const GIANT_ENTRY = cardEntry(GIANT, "Giant Growth", "giant growth");

const CARDS = {
  [BOLT]: BOLT_ENTRY,
  "lightning bolt": BOLT_ENTRY,
  [GIANT]: GIANT_ENTRY,
  "giant growth": GIANT_ENTRY,
};
// Two printings for Bolt, released 26 years apart, so "newest" and "oldest"
// genuinely select different art: an invalidation assertion that only counted
// calls would pass even if the second plan produced the same membership.
const PRINTINGS: Record<string, PrintingEntry[]> = { [BOLT]: [BOLT_NEW, BOLT_OLD] };

const NEWEST = (): ArtChainEntry[] => [{ type: "newest" }];
const OLDEST = (): ArtChainEntry[] => [{ type: "oldest" }];
/**
 * A chain that consults a deck's `(SET) NUM` annotation first and falls back
 * to `newest` — the production shape of a preference set under which a saved
 * deck can matter at all.
 *
 * Kept for the deck tests below even though nothing here reads a deck's
 * CONTENT: this file mocks `loadScryfallData`, so the module global
 * `resolveOracleIdSync` consults is never assigned and `deckPrintings` resolves
 * no names. The subject here is the memo KEY — whether a deck edit misses — and
 * that is measured on planner call counts. What a pinned `(SET) NUM` actually
 * contributes to a membership is `browser/__tests__/curatedDelta.test.ts`'s
 * subject, where the real loader runs against a stubbed `/scryfall-data.json`.
 */
const SOURCE_THEN_NEWEST = (): ArtChainEntry[] => [{ type: "source_printing" }, { type: "newest" }];

/** A deck as it is really stored: whatever `DeckBuilder` last wrote, verbatim.
 *  `loadSavedDeck` re-parses and re-repairs this on every call, which is why
 *  the memo cannot key on what it returns. */
function saveDeck(name: string, main: DeckEntry[], sideboard: DeckEntry[] = []): void {
  localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify({ main, sideboard }));
}

const plan = vi.mocked(planCuratedMembership);

/**
 * Preferences no previous test can have planned under.
 *
 * The memo is module-level state by design — the panel and the backend must
 * share one entry — and it keys on the IDENTITY of the two preference values,
 * so a fresh array and a fresh object are a guaranteed miss even when their
 * contents repeat. That is what starts each test from a cold cache without
 * reaching into the module for a reset hook that production has no use for.
 */
function coldStart(artChain: ArtChainEntry[] = NEWEST()): void {
  usePreferencesStore.setState({ artChain, artOverrides: {} });
}

describe("curated plan memo", () => {
  beforeEach(() => {
    data.cards = CARDS;
    data.printings = PRINTINGS;
    data.gate = Promise.resolve();
    localStorage.clear();
    plan.mockClear();
  });

  it("plans once for repeated calls at unchanged preferences", async () => {
    coldStart();

    const first = await planCuratedPack();
    const second = await planCuratedPack();

    // One install asks four or more times over — the panel's selector, the
    // estimate, `start()`'s conflict guard, `run()`'s descriptor pass — and
    // `create()` asks again for every pending record on each app launch.
    expect(plan).toHaveBeenCalledTimes(1);
    expect(second).toBe(first);
  });

  it("shares one plan between callers that overlap", async () => {
    coldStart();

    // Not awaited in between: the promise is cached, not the resolved value,
    // so a second caller arriving before the first settles joins it instead of
    // starting a second plan.
    const [first, second] = await Promise.all([planCuratedPack(), planCuratedPack()]);

    expect(plan).toHaveBeenCalledTimes(1);
    expect(second).toBe(first);
  });

  it("re-plans when the art chain changes", async () => {
    coldStart();
    const first = await planCuratedPack();

    usePreferencesStore.setState({ artChain: OLDEST() });
    const second = await planCuratedPack();

    expect(plan).toHaveBeenCalledTimes(2);
    // The digest, not just the call count: a memo that re-ran the planner but
    // returned the first membership would satisfy the count alone.
    expect(second.membershipDigest).not.toBe(first.membershipDigest);
  });

  it("re-plans when a per-card art override changes", async () => {
    coldStart();
    const first = await planCuratedPack();

    usePreferencesStore.setState({
      artOverrides: { [BOLT]: { scryfallId: BOLT_OLD.id, setCode: "lea", collectorNumber: "1" } },
    });
    const second = await planCuratedPack();

    expect(plan).toHaveBeenCalledTimes(2);
    expect(second.membershipDigest).not.toBe(first.membershipDigest);
  });

  it("re-plans when a saved deck changes", async () => {
    saveDeck("Burn", [
      { count: 4, name: "Lightning Bolt", sourcePrinting: { setCode: "M20", collectorNumber: "1" } },
    ]);
    coldStart(SOURCE_THEN_NEWEST());
    const first = await planCuratedPack();

    saveDeck("Burn", [
      { count: 4, name: "Lightning Bolt", sourcePrinting: { setCode: "LEA", collectorNumber: "1" } },
    ]);
    const second = await planCuratedPack();

    // Nothing about the PREFERENCES moved here, so a key covering only those
    // two would have served the first membership back.
    expect(plan).toHaveBeenCalledTimes(2);
    // The membership OBJECT, not its digest: the loader is mocked in this file
    // and never assigns the module global `resolveOracleIdSync` reads, so a
    // deck resolves no card names here and cannot move a digest. What that
    // deck's `(SET) NUM` annotation actually puts in a membership is
    // `curatedDelta.test.ts`'s subject, under the real loader; this file's
    // subject is the KEY, and identity is what proves a second plan was served
    // rather than the cached first one.
    expect(second).not.toBe(first);
  });

  it("re-plans when a deck is added and again when it is deleted", async () => {
    coldStart(SOURCE_THEN_NEWEST());
    const empty = await planCuratedPack();

    saveDeck("Burn", [
      { count: 4, name: "Lightning Bolt", sourcePrinting: { setCode: "LEA", collectorNumber: "1" } },
    ]);
    const added = await planCuratedPack();
    localStorage.removeItem(STORAGE_KEY_PREFIX + "Burn");
    const removed = await planCuratedPack();

    // The deck SET, not just a deck's contents: a key built from the stored
    // bodies alone would miss the appearance and disappearance of a whole deck.
    expect(plan).toHaveBeenCalledTimes(3);
    expect(added).not.toBe(empty);
    expect(removed).not.toBe(added);
  });

  it("files a plan under the decks it read, not the ones sampled before the await", async () => {
    saveDeck("Burn", [
      { count: 4, name: "Lightning Bolt", sourcePrinting: { setCode: "M20", collectorNumber: "1" } },
    ]);
    coldStart(SOURCE_THEN_NEWEST());

    // Park the plan exactly where production parks it: inside the card-data
    // load. That await is a real one — a cold start fetches ~76 MB, so seconds
    // can pass — and the decks are read on the far side of it.
    let release = (): void => undefined;
    data.gate = new Promise<void>((resolve) => { release = resolve; });
    const inFlight = planCuratedPack();
    saveDeck("Burn", [
      { count: 4, name: "Lightning Bolt", sourcePrinting: { setCode: "LEA", collectorNumber: "1" } },
    ]);
    release();
    await inFlight;

    // The revert — an undo in the deck builder, or cloud sync rewriting
    // localStorage. The decks are byte-for-byte what they were when the key was
    // SAMPLED, and nothing else has moved.
    saveDeck("Burn", [
      { count: 4, name: "Lightning Bolt", sourcePrinting: { setCode: "M20", collectorNumber: "1" } },
    ]);
    await planCuratedPack();

    // Filed under the text sampled BEFORE the await, this revert is a hit, and
    // the membership planned from the OTHER deck set is served for the life of
    // the tab. Stamping the key beside the deck read makes it a miss.
    expect(plan).toHaveBeenCalledTimes(2);
  });

  it("plans once for repeated calls while the saved decks are unchanged", async () => {
    saveDeck("Burn", [
      { count: 4, name: "Lightning Bolt", sourcePrinting: { setCode: "LEA", collectorNumber: "1" } },
    ]);
    coldStart(SOURCE_THEN_NEWEST());

    const first = await planCuratedPack();
    const second = await planCuratedPack();

    // The other direction, and the failure that hides: `loadSavedDeck` returns
    // a fresh object on every call, so a key that reached for the PARSED decks
    // would miss here every time — a memo that is useless rather than stale,
    // with nothing going red to say so.
    expect(plan).toHaveBeenCalledTimes(1);
    expect(second).toBe(first);
  });

  it("reports missing card data as a translatable kind with no prose", async () => {
    coldStart();
    data.cards = null;

    // `detail` is what the panel renders verbatim beneath the translated
    // sentence. `network` already has that sentence in all seven locales, so
    // an English phrase here would appear untranslated under a translated one.
    await expect(planCuratedPack()).rejects.toMatchObject({ kind: "network", detail: null });
    expect(plan).not.toHaveBeenCalled();
  });

  it("does not remember a failed plan as the answer", async () => {
    coldStart();
    data.cards = null;
    await expect(planCuratedPack()).rejects.toMatchObject({ kind: "network" });

    data.cards = CARDS;
    const membership = await planCuratedPack();

    // A cached rejection would make one transient card-data failure permanent
    // for the life of the tab, with no preference change available to clear it.
    expect(membership.descriptors.length).toBeGreaterThan(0);
  });
});
