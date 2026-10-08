import { VisualPackBackendError } from "./backend.ts";
import { planCuratedMembership } from "./curatedMembership.ts";
import type { CuratedDeckPrinting, CuratedMembership } from "./curatedMembership.ts";
import type { ScryfallAssetDescriptor } from "./browser/descriptors.ts";
import { packId } from "./types.ts";
import type { CatalogRoot } from "./types.ts";
import { loadPrintingsData, loadScryfallData, resolveOracleIdSync } from "../scryfall.ts";
import { listSavedDeckNames, loadSavedDeck, STORAGE_KEY_PREFIX } from "../../constants/storage.ts";
import {
  usePreferencesStore,
  type ArtChainEntry,
  type CardArtOverride,
} from "../../stores/preferencesStore.ts";

/**
 * What a membership was planned from, and therefore what a later call must
 * match to be served it.
 *
 * Built BEFORE the plan it keys, and handed to `planMembership` so that the
 * plan can re-stamp `deckText` at the moment it actually reads the decks — see
 * `planMembership`, which is where the two samples differ and why.
 */
interface CuratedPlanKey {
  artChain: ArtChainEntry[];
  artOverrides: Record<string, CardArtOverride>;
  deckText: string;
}

/**
 * The one membership this module is holding on to, and the preferences it was
 * planned from.
 *
 * SINGLE-ENTRY on purpose. A curated membership is ~105,000 descriptors at
 * roughly 500 bytes of retained object per row, so a map keyed by preference
 * would grow tens of MiB per distinct art chain a user tries — a cache whose
 * miss is 130-330 ms must not trade that for unbounded heap.
 *
 * The PROMISE is cached rather than the resolved value, so two callers that
 * overlap (the panel resolving a selector while an estimate is already in
 * flight) share one plan instead of racing two. A rejected plan evicts itself:
 * a transient card-data failure must not be remembered as the answer.
 */
interface CuratedPlan {
  key: CuratedPlanKey;
  membership: Promise<CuratedMembership>;
}

let cachedPlan: CuratedPlan | null = null;

/**
 * The saved decks as their STORED TEXT, in `listSavedDeckNames`' alphabetical
 * order.
 *
 * This is the memo key's third component and it is a raw-string compare BY
 * DECISION, over the two alternatives:
 *
 * - NOT the parsed decks. `loadSavedDeck` runs `JSON.parse` +
 *   `repairParsedDeck` + `projectSavedDeckSpecialSlots` on every call, so it
 *   returns a fresh object every time and has no identity to key on. Keying on
 *   one would miss on every single call — a memo that is useless rather than
 *   stale, which is the failure that hides: nothing goes red, the 130-330 ms
 *   plan simply runs four times per install again.
 * - NOT a digest. `sha256Hex` is `crypto.subtle`, so an awaited key would make
 *   the cache check asynchronous, and `planCuratedPack` deliberately answers
 *   SYNCHRONOUSLY on a hit — that is what lets two overlapping callers share
 *   one in-flight plan instead of both missing in the await window.
 *
 * The text is exact (a byte the parser ignores is a spurious miss at worst,
 * never a stale hit), parse-free on a hit, and retains a few hundred KiB at
 * most beside a membership that already retains tens of MiB.
 *
 * BODIES ONLY — the deck names are not part of the key. The membership is a
 * function of what the decks CONTAIN, so a rename must not force a re-plan, and
 * the list's own shape is still covered: adding or deleting a deck changes the
 * array's length, and the alphabetical order fixes each body's position. A name
 * was in this key until a mutation probe could not find a single test that
 * depended on it, which is what a line that cannot be wrong looks like.
 *
 * Serialized through `JSON.stringify` rather than joined on a separator so the
 * encoding is unambiguous: a deck body is arbitrary stored text and can contain
 * whatever character a hand-rolled separator picked, which would let two
 * different deck sets flatten to one string — the one direction that turns a
 * spurious miss into a stale hit.
 */
function savedDeckText(): string {
  return JSON.stringify(listSavedDeckNames().map((name) => localStorage.getItem(STORAGE_KEY_PREFIX + name)));
}

/**
 * Every printing the user's saved decks pin, as the planner's `deckPrintings`.
 *
 * ONLY `main` and `sideboard` are `DeckEntry[]` and therefore the only slots
 * carrying `sourcePrinting`. `commander`, `sticker_sheets`, `planar_deck`,
 * `scheme_deck` and `signature_spell` are `string[]` — names with no printing
 * identity — and `companion` is a bare `string` that
 * `projectSavedDeckSpecialSlots` folds into `sideboard` as `{count: 1, name}`
 * for a non-commander format, again with no `sourcePrinting`. So a commander's
 * or a companion's art cannot be pinned by the deck it belongs to, and nothing
 * here should be read as claiming otherwise.
 *
 * `sourcePrinting` is itself optional: a plain "4 Lightning Bolt" list pins
 * nothing, and only a list carrying `(SET) NUM` annotations contributes at all.
 * An empty result is the common case, not a failure.
 *
 * `resolveOracleIdSync` is the app's single name-to-oracle-id rule — annotation
 * stripping, diacritic folding ("Eowyn" for "Éowyn"), and the front-face
 * fallback that makes a decklist's "Fire // Ice" resolve — and it is reused
 * rather than restated. It reads `scryfall-data.json` through the module global
 * that `loadScryfallData` assigns, and answers `null` until that assignment has
 * happened; this runs only from `planMembership`, AFTER its `await
 * loadScryfallData()`, so by then the global is set. That is an ordering fact
 * about the one call site, not an assumption about the loader.
 */
function deckPrintings(): CuratedDeckPrinting[] {
  const pinned: CuratedDeckPrinting[] = [];
  for (const name of listSavedDeckNames()) {
    const deck = loadSavedDeck(name);
    if (!deck) continue;
    for (const entry of [...deck.main, ...deck.sideboard]) {
      if (!entry.sourcePrinting) continue;
      const oracleId = resolveOracleIdSync(entry.name);
      if (oracleId) pinned.push({ oracleId, source: entry.sourcePrinting });
    }
  }
  return pinned;
}

/**
 * Plan the curated pack from the app's own card data and stored art
 * preferences — the single authority for what "curated" contains.
 *
 * The backend installs exactly these descriptors and the settings UI compares
 * this digest against the installed pack's root to report drift, so both must
 * come from here: two independent assemblies of the planner's input would
 * disagree the moment one of them gained a source the other lacked.
 *
 * It lives outside `browser/scryfallBulk.ts` so that a UI module can import it
 * without pulling the bulk JSONL scanner along: the backend is reached through
 * a dynamic `import()` in `services/platform.ts`, which code-splits that
 * scanner out of the eager bundle, and a static import of the planner from the
 * settings panel would drag it back in.
 *
 * MEMOIZED against the preferences it reads. One install plans four or more
 * times today — the panel resolving a selector, the estimate, `start()`'s
 * conflict guard, and `run()`'s own descriptor pass — and `create()` re-runs
 * every pending record on each app launch, so this is load-bearing rather than
 * an optimisation. The card data behind it is already resident: `loadScryfallData`
 * and `loadPrintingsData` are module-level memoized promises, so the cost being
 * avoided here is `planCuratedMembership` itself.
 *
 * The key has THREE components: the identity of the two preference values, and
 * the saved decks' stored text (see `savedDeckText`, which explains why the
 * third one cannot be an identity). Decks are a membership input — a deck's
 * `(SET) NUM` annotation pins a printing the art chain would not otherwise
 * select — so a key covering only the preferences would return the cached
 * membership after a deck edit, silently, until something else moved.
 *
 * The preference half is the IDENTITY of the two values, which is sound because
 * every writer replaces them: the chain mutators build a new array
 * (`[...artChain, entry]`, `.filter`, `.slice()`), the override mutators build
 * a new object (spread, rest-spread, `{}`), `resetAllPreferences` calls a
 * FUNCTION returning a fresh literal, and the persist layer's `merge` and
 * `persist.rehydrate()` both install freshly parsed objects. `addArtChainEntry`
 * returning `state` unchanged on a duplicate keeps identity AND contents, which
 * is the correct hit. A
 * new reference holding equal contents is a MISS, which costs one re-plan and
 * yields the same digest; there is no reference-equal path to changed
 * contents, so a stale hit cannot happen.
 *
 * The key deliberately does NOT cover the card data, and that is an assumption
 * about a different module rather than something checked here: `loadScryfallData`
 * and `loadPrintingsData` memoize one fetch each at module scope, so within a
 * session the maps are fetched once and never replaced. A moved source URL
 * therefore arrives with a fresh map in a fresh tab, where this cache starts
 * empty. Nothing in `src/` resets those promises, so the assumption holds — but
 * a test that edits the maps IN PLACE is invisible to this key and must force a
 * miss itself (see `curatedDelta.test.ts`), and any future in-session reload of
 * card data would have to invalidate here as well.
 */
export function planCuratedPack(): Promise<CuratedMembership> {
  const { artChain, artOverrides } = usePreferencesStore.getState();
  const deckText = savedDeckText();
  const cached = cachedPlan;
  if (
    cached
    && cached.key.artChain === artChain
    && cached.key.artOverrides === artOverrides
    && cached.key.deckText === deckText
  ) return cached.membership;
  // The key object is built first and handed to the plan, which overwrites
  // `deckText` with the sample taken beside its own deck read. What is compared
  // above is therefore always a description of the decks the cached membership
  // was actually planned from.
  const key: CuratedPlanKey = { artChain, artOverrides, deckText };
  const entry: CuratedPlan = { key, membership: planMembership(artChain, artOverrides, key) };
  cachedPlan = entry;
  // Evict a failure rather than serve it forever. The `catch` is on a branch
  // of the promise, not on the one returned, so the caller still sees the
  // rejection.
  void entry.membership.catch(() => { if (cachedPlan === entry) cachedPlan = null; });
  return entry.membership;
}

async function planMembership(
  artChain: ArtChainEntry[],
  artOverrides: Record<string, CardArtOverride>,
  key: CuratedPlanKey,
): Promise<CuratedMembership> {
  try {
    const [cards, printings] = await Promise.all([loadScryfallData(), loadPrintingsData()]);
    // No detail: `network` is already a complete sentence in all seven
    // locales, and any prose composed here would render verbatim, in English,
    // underneath the translated one.
    if (!cards || !printings) throw new VisualPackBackendError("network");
    // The decks are read HERE, after the await, because `resolveOracleIdSync`
    // needs the loader to have resolved — and the await is a real one: a cold
    // start fetches ~76 MB, so seconds can pass, not a microtask.
    //
    // So the text `planCuratedPack` sampled before that wait describes the
    // decks as they were BEFORE it, not the ones read below. Filing this plan
    // under that older text would be a stale hit waiting for a revert: decks
    // D1 -> D2 inside the window, membership(D2) filed under text(D1), decks
    // reverted to D1, and every later call matches text(D1) and is served
    // membership(D2) for the life of the tab.
    //
    // The two statements below are one synchronous block with no await between
    // them, so nothing can interleave: whatever `deckPrintings` read is what
    // `savedDeckText` describes, and stamping it onto the key makes the stored
    // key describe the decks actually planned. The revert above is then a MISS.
    const pinned = deckPrintings();
    key.deckText = savedDeckText();
    return await planCuratedMembership({
      packId: packId("curated"),
      cards,
      printings,
      artChain,
      artOverrides,
      deckPrintings: pinned,
    });
  } catch (error) {
    if (error instanceof VisualPackBackendError) throw error;
    // Classify the rest DELIBERATELY rather than let the backend guess.
    //
    // `errorKind` ends in `return "storage"`, which is RETRYABLE, and the
    // curated arm of `forEachScryfallAsset` sits ABOVE the try that wraps the
    // bulk path — so an unclassified throw from here reaches `run()` naked,
    // reads as transient, and leaves the operation record `downloading`. That
    // state offers Resume as the only enabled control while disabling every
    // durable mutation, and `create()`'s pending loop re-runs the record on
    // every launch, so the settings panel stays wedged across restarts.
    //
    // This contains the CLASS, without a claim about what produced it: a throw
    // that is not already a backend error is a defect in this planner or in
    // the data it reads, both of which are deterministic, so re-running cannot
    // reach a different outcome. `internal` says exactly that and is not
    // retryable, so the record terminates and the panel stays usable. The
    // transient failures raised above as `VisualPackBackendError` are
    // deliberately NOT captured — they must stay resumable.
    throw new VisualPackBackendError("internal", error instanceof Error ? error.message : undefined);
  }
}

/**
 * The curated membership a selector names, refusing one it does not name.
 *
 * The digest is the pack's catalog root, so installing a membership whose
 * digest is not the selector's would store those objects under a root they do
 * not hash to. This is the curated counterpart of the `complete` selector's
 * `rootSha256` conflict guard, and it fires when preferences change between
 * the estimate the user accepted and the install they started.
 *
 * The refusal is STRUCTURALLY non-retryable: the selector stores only a
 * digest, and a digest is not invertible, so once preferences move there is no
 * way to reconstruct the membership it named. Callers must therefore surface
 * `conflict` through something other than a retry affordance — `start()`
 * rejects the request outright, and `run()` terminates an operation record
 * that hits it rather than leaving it resumable.
 */
export async function curatedDescriptors(digest: CatalogRoot): Promise<readonly ScryfallAssetDescriptor[]> {
  const membership = await planCuratedPack();
  if (membership.membershipDigest !== digest) throw new VisualPackBackendError("conflict");
  return membership.descriptors;
}
