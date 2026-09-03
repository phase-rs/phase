import "fake-indexeddb/auto";

import { IDBFactory } from "fake-indexeddb";
import { openDB } from "idb";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { STORAGE_KEY_PREFIX } from "../../../../constants/storage.ts";
import { usePreferencesStore, type ArtChainEntry, type CardArtOverride } from "../../../../stores/preferencesStore.ts";
import type { DeckEntry } from "../../../deckParser.ts";
import { CARD_BACK_URL, loadScryfallData, type PrintingEntry } from "../../../scryfall.ts";
import { assetKey, packId } from "../../types.ts";
import type { AssetKey, CatalogRoot, CuratedDrift, InstallSelector, OperationId, ProgressEvent, ResolutionResponse } from "../../types.ts";
import type { ScryfallAssetDescriptor } from "../descriptors.ts";
import { ScryfallBrowserVisualPackBackend } from "../scryfallBackend.ts";
import { planCuratedMembership } from "../../curatedMembership.ts";
import { planCuratedPack } from "../../curatedPack.ts";

// Step 4's subject is the DELTA: what a second curated install at a second
// digest downloads, keeps, and throws away. Every assertion below is therefore
// about the difference between two installs, never about one.
//
// The harness mirrors `curatedSelector.test.tsx` rather than importing from it
// — a test file is not a module other tests may depend on — but the fixture is
// deliberately larger in one dimension: Lightning Bolt has THREE printings, so
// three DISTINCT memberships exist. Two are not enough. The abandoned-root case
// needs an install to be stranded at a digest that a LATER, DIFFERENT digest
// then supersedes, and with only two memberships the third install would land
// back on the installed root and be short-circuited to `{status:"healthy"}`
// before any sweep could run.

const BULK_INDEX_URL = "https://api.scryfall.com/bulk-data";
const BULK_DOWNLOAD_URL = "https://data.scryfall.io/all-cards.jsonl.gz";
const BULK_RECORD = {
  type: "all_cards",
  updated_at: "2026-08-01T00:00:00.000Z",
  jsonl_download_uri: BULK_DOWNLOAD_URL,
  compressed_size: 2_400_000_000,
};

const BOLT = "11111111-abcd-4111-8111-111111111111";
const GIANT = "22222222-abcd-4222-8222-222222222222";
const TOKEN_ORACLE = "33333333-abcd-4333-8333-333333333333";
const EOWYN = "44444444-abcd-4444-8444-444444444444";

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
const BOLT_MID = printing("cccccccc-cccc-4ccc-8ccc-cccccccccccc", "mh1", "2019-06-14");
const BOLT_OLD = printing("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "lea", "1993-08-05");

function cardEntry(oracleId: string, name: string, faceName: string) {
  return {
    oracle_id: oracleId,
    name,
    face_names: [faceName],
    faces: [imageFace(oracleId)],
    mana_cost: "",
    cmc: 0,
    type_line: "",
    colors: [],
    color_identity: [],
    keywords: [],
  };
}

const BOLT_ENTRY = cardEntry(BOLT, "Lightning Bolt", "lightning bolt");
const GIANT_ENTRY = cardEntry(GIANT, "Giant Growth", "giant growth");
/** A card whose only name key carries a diacritic, which is how
 *  `scryfall-data.json` really stores this class — and the class this repo has
 *  a history of losing to ASCII-only folding. */
const EOWYN_ENTRY = cardEntry(EOWYN, "Éowyn, Fearless Knight", "éowyn, fearless knight");

const CARDS = {
  [BOLT]: BOLT_ENTRY,
  "lightning bolt": BOLT_ENTRY,
  [GIANT]: GIANT_ENTRY,
  "giant growth": GIANT_ENTRY,
  [EOWYN]: EOWYN_ENTRY,
  "éowyn, fearless knight": EOWYN_ENTRY,
  [`token:${TOKEN_ORACLE}`]: cardEntry(TOKEN_ORACLE, "Soldier", "soldier"),
};
/**
 * Éowyn's two printings, arranged so that only a DECK can tell them apart.
 *
 * `EOWYN_MH1` is `printings[0]` (so `newest` picks it), the oldest by
 * `released_at` (so `oldest` picks it too), and in `mh1` (so `MIDDLE` picks it
 * as well). Every chain this file installs under therefore selects the same
 * Éowyn art, which keeps her out of every drift count below; only the
 * `source_printing` entry of `SOURCE_THEN_NEWEST`, handed the deck's `(SLD) 1`
 * annotation, reaches `EOWYN_SLD`.
 */
const EOWYN_MH1 = printing("dddddddd-dddd-4ddd-8ddd-dddddddddddd", "mh1", "2019-06-14");
const EOWYN_SLD = printing("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee", "sld", "2024-02-02");
// `newest` returns `printings[0]` by ARRAY order, so the order here is what
// makes the three chains below select three different printings.
const PRINTINGS: Record<string, PrintingEntry[]> = {
  [BOLT]: [BOLT_NEW, BOLT_MID, BOLT_OLD],
  [EOWYN]: [EOWYN_MH1, EOWYN_SLD],
};

const NEWEST: ArtChainEntry[] = [{ type: "newest" }];
const OLDEST: ArtChainEntry[] = [{ type: "oldest" }];
const MIDDLE: ArtChainEntry[] = [{ type: "set", setCode: "mh1", label: "MH1" }];
/**
 * A chain that consults a deck's `(SET) NUM` annotation first and falls back
 * to `newest`.
 *
 * This is what makes a deck test discriminating rather than merely non-empty.
 * `selectedPrintings` asks `selectedPrinting` once with NO source and once per
 * deck source: with no source the `source_printing` entry matches nothing and
 * the chain falls through to `newest`, while with the deck's source it resolves
 * the annotated printing. The deck's contribution is therefore a descriptor the
 * preferences alone could never produce.
 *
 * A FACTORY, unlike the three constants above: the plan memo keys on the
 * identity of the stored `artChain`, so a fresh array is a guaranteed miss.
 */
const SOURCE_THEN_NEWEST = (): ArtChainEntry[] => [{ type: "source_printing" }, { type: "newest" }];

/**
 * How many image records every chain in this file agrees on.
 *
 * Giant Growth is absent from `PRINTINGS`, so it is canonical under any chain,
 * and Éowyn's two printings are arranged (see above) so that `NEWEST`, `OLDEST`
 * and `MIDDLE` all land on `EOWYN_MH1`. Three rungs each. Only Lightning Bolt's
 * three rungs follow the chain, which is what every delta below measures.
 */
const CHAIN_INVARIANT_ASSETS = 6;

/** An in-memory `Cache`, holding only what this backend asks of one. */
class MemoryCache {
  readonly entries = new Map<string, { body: Uint8Array; type: string }>();

  async put(request: string, response: Response): Promise<void> {
    this.entries.set(request, {
      body: new Uint8Array(await response.arrayBuffer()),
      type: response.headers.get("Content-Type") ?? "application/octet-stream",
    });
  }

  async match(request: string): Promise<Response | undefined> {
    const entry = this.entries.get(request);
    return entry ? new Response(entry.body, { headers: { "Content-Type": entry.type } }) : undefined;
  }

  async delete(request: string): Promise<boolean> {
    return this.entries.delete(request);
  }
}

let cache = new MemoryCache();
let requested: string[] = [];
/** A total image outage: `fetchImage` rejects a non-200 as `network`. */
let failImages = false;
/** Holds the matching image fetches open indefinitely, so one backend can be
 *  parked mid-install while a second one runs to completion against the same
 *  database. Releasing it lets the parked install finish. */
let held: { matches: (source: string) => boolean; gate: Promise<void>; release: () => void } | null = null;

function holdImages(matches: (source: string) => boolean): () => void {
  let release = (): void => undefined;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  held = { matches, gate, release };
  return release;
}
/** How many card images may still be served before the outage begins. Lets a
 *  test strand a PARTIAL membership at a digest rather than an empty one. */
let imageBudget = Number.POSITIVE_INFINITY;
let imagesServed = 0;

function jsonResponse(value: unknown): Response {
  return new Response(JSON.stringify(value), { status: 200, headers: { "Content-Type": "application/json" } });
}

function imageResponse(source: string): Response {
  // URL-derived bytes, so content-addressed paths differ per asset the way real
  // images do — and so two URLs deliberately served the SAME bytes land on one
  // shared cache entry, which is what the sharing tests need.
  return new Response(new TextEncoder().encode(source), { status: 200, headers: { "Content-Type": "image/jpeg" } });
}

const fetchStub = vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
  const source = String(input);
  requested.push(source);
  if (source === BULK_INDEX_URL) return jsonResponse({ data: [BULK_RECORD] });
  if (source === "/scryfall-data.json") return jsonResponse(CARDS);
  if (source === "/scryfall-printings.json") return jsonResponse(PRINTINGS);
  // The `core` pack's only image, served with the SAME bytes as Bolt's newest
  // normal rung. Content addressing then gives both packs one cache entry, so
  // "an image another pack still uses" is a real shared path rather than a
  // claim about one.
  if (source === CARD_BACK_URL) return imageResponse(url(BOLT_NEW.id, "normal"));
  if (source.startsWith("https://cards.scryfall.io/")) {
    if (held?.matches(source)) await held.gate;
    if (failImages || imagesServed >= imageBudget) return new Response("", { status: 503 });
    imagesServed += 1;
    return imageResponse(source);
  }
  throw new Error(`unexpected fetch: ${source}`);
});

const DATABASE = "phase-visual-packs-scryfall-v1";

interface StoredObject {
  id: string;
  root: CatalogRoot;
  packId: string;
  assetKey: AssetKey;
  path: string;
  sourceUrl?: string;
}

/**
 * The backend's own private methods, as an interposition seam.
 *
 * Two of the properties below have no public seam at all: whether a donor scan
 * happened is invisible to fetch counts, and the window between `completePack`
 * and the sweep contains no cache or fetch call to hook. The methods themselves
 * are the only points at which either can be observed, so they are replaced on
 * the prototype and restored in the test's own `finally`.
 */
type BackendInternals = Record<string, (this: unknown, ...args: never[]) => unknown>;

function internals(): BackendInternals {
  return ScryfallBrowserVisualPackBackend.prototype as unknown as BackendInternals;
}

/** Only card-image requests. `loadScryfallData`/`loadPrintingsData` memoize
 *  their resolved maps for the lifetime of the module, so the data files are
 *  fetched at most once per FILE and a raw request count cannot discriminate
 *  anything about a second install. */
function imageRequests(): string[] {
  return requested.filter((entry) => entry.startsWith("https://cards.scryfall.io/"));
}

async function objectRows(): Promise<StoredObject[]> {
  const database = await openDB(DATABASE);
  const rows = await database.getAll("objects") as StoredObject[];
  database.close();
  return rows;
}

function pathOf(rows: readonly StoredObject[], key: AssetKey): string {
  const row = rows.find((entry) => entry.assetKey === key);
  if (!row) throw new Error(`no installed row for ${key}`);
  return row.path;
}

/** The bytes actually behind a cached path, as text. `imageResponse` encodes
 *  the source URL, so this reads back WHICH image a row is really serving —
 *  the difference between reusing stale bytes and re-fetching moved ones. */
function cachedText(path: string): string | null {
  const entry = cache.entries.get(path);
  return entry ? new TextDecoder().decode(entry.body) : null;
}

/**
 * Rewrite every `objects` row into the shape the build BEFORE this step wrote:
 * no `sourceUrl` at all. Returns the count so a caller can prove the rows it is
 * about to test against actually existed.
 */
async function dropStoredSourceUrls(): Promise<number> {
  const database = await openDB(DATABASE);
  const rows = await database.getAll("objects") as StoredObject[];
  for (const row of rows) {
    const legacy: StoredObject = { ...row };
    delete legacy.sourceUrl;
    await database.put("objects", legacy);
  }
  database.close();
  return rows.length;
}

/**
 * Move Giant Growth's `normal` face URL, as a regeneration of
 * `scryfall-data.json` does. The map is mutated in place because
 * `loadScryfallData` memoizes its RESOLVED value, so re-serving the fetch would
 * change nothing; every key carrying this oracle id is updated because the JSON
 * round-trip gives the oracle-id key and the name key independent objects.
 */
async function moveGiantNormal(token: string): Promise<string> {
  const cards = await loadScryfallData();
  if (!cards) throw new Error("card data unavailable");
  const next = url(token, "normal");
  for (const entry of Object.values(cards)) {
    if (entry.oracle_id === GIANT) entry.faces[0].normal = next;
  }
  return next;
}

async function curatedSelector(): Promise<{ selector: InstallSelector; descriptors: readonly ScryfallAssetDescriptor[] }> {
  const membership = await planCuratedPack();
  return {
    selector: { kind: "curated", membershipDigest: membership.membershipDigest },
    descriptors: membership.descriptors,
  };
}

/** The `failed` progress events one backend instance emits, from this point on. */
async function failures(backend: ScryfallBrowserVisualPackBackend): Promise<ProgressEvent[]> {
  const events: ProgressEvent[] = [];
  await backend.subscribeProgress((event) => {
    if (event.phase === "failed") events.push(event);
  });
  return events;
}

async function settle(backend: ScryfallBrowserVisualPackBackend, operation: OperationId): Promise<void> {
  await vi.waitFor(async () => {
    expect((await backend.operationStatus(operation)).state).toBe("completed");
  }, { timeout: 5000 });
}

async function startCurated(backend: ScryfallBrowserVisualPackBackend): Promise<{
  digest: CatalogRoot;
  descriptors: readonly ScryfallAssetDescriptor[];
  operation: OperationId;
}> {
  const { selector, descriptors } = await curatedSelector();
  const response = await backend.start({ kind: "install", selector, objectEstimate: descriptors.length });
  if (response.status !== "started") throw new Error("curated install did not start");
  return {
    digest: (selector as { membershipDigest: CatalogRoot }).membershipDigest,
    descriptors,
    operation: response.operationId,
  };
}

async function installCurated(backend: ScryfallBrowserVisualPackBackend): Promise<{
  digest: CatalogRoot;
  descriptors: readonly ScryfallAssetDescriptor[];
  operation: OperationId;
}> {
  const started = await startCurated(backend);
  await settle(backend, started.operation);
  return started;
}

async function installCore(backend: ScryfallBrowserVisualPackBackend): Promise<void> {
  const response = await backend.start({ kind: "install", selector: { kind: "core" }, objectEstimate: 1 });
  if (response.status !== "started") throw new Error("core install did not start");
  await settle(backend, response.operationId);
}

/** Every installed object a render-time asset lookup can actually reach. */
async function resolvedAssets(
  backend: ScryfallBrowserVisualPackBackend,
  descriptors: readonly ScryfallAssetDescriptor[],
): Promise<ResolutionResponse["entries"][number]["matches"]> {
  const response = await backend.resolve(descriptors.map((value) => ({ kind: "asset", key: value.assetKey })));
  return response.entries.flatMap((entry) => entry.matches);
}

/** A deck as it is really stored: whatever `DeckBuilder` last wrote, verbatim.
 *  `loadSavedDeck` re-parses and re-repairs this on every call, which is why
 *  the plan memo cannot key on what it returns. */
function saveDeck(name: string, main: DeckEntry[], sideboard: DeckEntry[] = []): void {
  localStorage.setItem(STORAGE_KEY_PREFIX + name, JSON.stringify({ main, sideboard }));
}

/** Every `exact_printing` asset key in a membership, so an assertion can name
 *  the printing that appeared rather than only how many did. */
function exactKeys(membership: { descriptors: readonly { assetKey: string }[] }): string[] {
  return membership.descriptors
    .map((value) => value.assetKey)
    .filter((key) => key.startsWith("asset:v1:exact_printing:"));
}

/** The membership an explicit set chain produces, planned straight from the
 *  fixtures. Names the printing a deck is supposed to pin INDEPENDENTLY of the
 *  deck, so a deck assertion is about that printing rather than about a count. */
async function membershipForSet(setCode: string): Promise<{ descriptors: readonly ScryfallAssetDescriptor[] }> {
  return planCuratedMembership({
    packId: packId("curated"),
    cards: CARDS,
    printings: PRINTINGS,
    artChain: [{ type: "set", setCode, label: setCode.toUpperCase() }],
    artOverrides: {},
  });
}

/** The descriptors a second membership adds to a first — by (assetKey,
 *  sourceUrl), which is exactly the identity content reuse matches on. */
function addedBy(
  next: readonly ScryfallAssetDescriptor[],
  previous: readonly ScryfallAssetDescriptor[],
): ScryfallAssetDescriptor[] {
  return next.filter((value) =>
    !previous.some((old) => old.assetKey === value.assetKey && old.sourceUrl === value.sourceUrl));
}

/**
 * `curatedDrift()` with its unmeasured arm asserted away.
 *
 * It answers `null` when the card data behind a membership plan is not
 * resident, so that a passive read cannot trigger a 76 MB load. That cannot be
 * the case in this file: every test here has already planned a membership
 * through `installCurated` or `curatedSelector`, which loads it. Asserted
 * rather than `!`-ed, so a change that breaks that assumption fails by name
 * instead of as a property access on null.
 */
async function measuredDrift(backend: ScryfallBrowserVisualPackBackend): Promise<CuratedDrift> {
  const drift = await backend.curatedDrift();
  if (!drift) throw new Error("curated drift was not measured");
  return drift;
}

describe("curated delta install", () => {
  beforeEach(() => {
    globalThis.indexedDB = new IDBFactory();
    cache = new MemoryCache();
    requested = [];
    failImages = false;
    imageBudget = Number.POSITIVE_INFINITY;
    imagesServed = 0;
    held = null;
    fetchStub.mockClear();
    vi.stubGlobal("fetch", fetchStub);
    vi.stubGlobal("caches", { open: async () => cache } as unknown as CacheStorage);
    usePreferencesStore.setState({ artChain: NEWEST, artOverrides: {} });
  });

  afterEach(() => {
    held?.release();
    held = null;
    vi.unstubAllGlobals();
  });

  it("downloads only the assets a new digest adds, and still finishes", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const first = await installCurated(backend);
    expect(imageRequests()).toHaveLength(first.descriptors.length);
    const donorRows = await objectRows();
    requested = [];

    usePreferencesStore.setState({ artChain: OLDEST });
    const second = await installCurated(backend);

    expect(second.digest).not.toBe(first.digest);
    // Part of the membership moves and part does not: Bolt's three exact rungs
    // follow the chain to a different printing, while the chain-invariant ones
    // are untouched. Without content reuse the whole membership is fetched
    // again.
    const added = addedBy(second.descriptors, first.descriptors);
    expect(added).toHaveLength(3);
    expect(imageRequests()).toHaveLength(3);
    expect(new Set(imageRequests())).toEqual(new Set(added.map((value) => value.sourceUrl)));

    // A fetch count cannot see any of what follows, because the reused assets
    // never call `fetchImage`. `markComplete` writes the objects ROW as well as
    // the counter, so a reuse path that skipped it would produce a pack that is
    // silently that many assets short — and this step's own sweep would then
    // find those cache entries referenced by no row and delete the bytes too.
    //
    // The user-facing property first, so the two assertions do not mask each
    // other under probing: every asset of the new membership, the reused ones
    // included, resolves AFTER the sweep that ran inside this install.
    const reused = second.descriptors.filter((value) => !added.includes(value));
    expect(reused).toHaveLength(CHAIN_INVARIANT_ASSETS);
    const matches = await resolvedAssets(backend, second.descriptors);
    expect(matches).toHaveLength(second.descriptors.length);
    expect(reused.every((value) => matches.some((match) => match.assetKey === value.assetKey))).toBe(true);

    // ...and each reused key resolves to the DONOR's entry, not merely to some
    // entry. "It resolves" is satisfied by adopting the wrong donor, which is
    // the failure mode `contentId`'s source-URL half exists to prevent, so the
    // path and the bytes behind it are both asserted against the row the first
    // install actually wrote.
    for (const value of reused) {
      const match = matches.find((entry) => entry.assetKey === value.assetKey);
      expect(match?.url).toBe(pathOf(donorRows, value.assetKey));
      expect(cachedText(match!.url)).toBe(value.sourceUrl);
    }

    // Then the accounting. MEASURED: losing this alone does NOT hang the
    // operation — `finish()` never compares the two counters, so the record
    // still reaches `completed` and the panel simply reports a figure that is
    // permanently short.
    const status = await backend.operationStatus(second.operation);
    expect(status.state).toBe("completed");
    expect(status.objectTotal).toBe(second.descriptors.length);
    expect(status.objectsPromoted).toBe(status.objectTotal);
  });

  it("re-fetches an asset whose source URL moved under an unchanged key", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const first = await installCurated(backend);
    const before = await moveGiantNormal(GIANT);
    try {
      const moved = await moveGiantNormal("44444444-abcd-4444-8444-444444444444");
      // `moveGiantNormal` edits the card map IN PLACE, which the plan memo
      // cannot see: it keys on the identity of the two preference values, and
      // in production the card data is fetched once per session and never
      // mutated, so a URL that moves arrives with a fresh map in a fresh tab.
      // A fresh but CONTENT-EQUAL `artOverrides` forces the miss without
      // perturbing the membership, so the only thing that differs between the
      // two plans below is still the moved URL.
      usePreferencesStore.setState({ artOverrides: {} });
      requested = [];

      const second = await installCurated(backend);

      // The discriminating shape: these keys are IDENTICAL across the two
      // memberships and only the URL behind them moved. A `canonical_card:` key
      // names no printing, so its bytes are whatever the card data currently
      // supplies — reuse keyed on the asset key alone would adopt the
      // superseded image and serve stale art for ever, with the digest already
      // changed and no path back.
      const added = addedBy(second.descriptors, first.descriptors);
      expect(added).toHaveLength(2);
      expect(added.every((value) => first.descriptors.some((old) => old.assetKey === value.assetKey))).toBe(true);
      expect(new Set(imageRequests())).toEqual(new Set(added.map((value) => value.sourceUrl)));

      // Giant's art_crop rung shares the key AND the URL, so it is reused.
      const crop = assetKey(`asset:v1:canonical_card:${GIANT}-0-art_crop-art_crop`);
      expect(imageRequests()).not.toContain(url(GIANT, "art_crop"));
      expect(second.descriptors.some((value) => value.assetKey === crop)).toBe(true);

      // Stronger than the fetch count: read back WHICH image the moved key is
      // serving. Reuse would leave the old URL's bytes under the new row.
      const normal = assetKey(`asset:v1:canonical_card:${GIANT}-0-full_card-normal`);
      const match = (await resolvedAssets(backend, second.descriptors)).find((value) => value.assetKey === normal);
      expect(match).toBeDefined();
      expect(cachedText(match!.url)).toBe(moved);
      expect(cachedText(match!.url)).not.toBe(before);
    } finally {
      await moveGiantNormal(GIANT);
    }
  });

  it("never reuses a row written before source URLs were recorded", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const first = await installCurated(backend);

    // Exactly what a row written by the build before this step looks like. Such
    // a row cannot say which URL its bytes came from, so it must never be
    // reused — which is what makes the field additive and the store
    // migration-free.
    expect(await dropStoredSourceUrls()).toBe(first.descriptors.length);
    usePreferencesStore.setState({ artChain: OLDEST });
    requested = [];

    const second = await installCurated(backend);

    // The same three assets the first test reuses are downloaded again here.
    expect(addedBy(second.descriptors, first.descriptors)).toHaveLength(3);
    expect(imageRequests()).toHaveLength(second.descriptors.length);
    expect(imageRequests()).toContain(url(GIANT, "art_crop"));
    expect(await resolvedAssets(backend, second.descriptors)).toHaveLength(second.descriptors.length);
  });

  it("keeps an image another pack still uses and deletes one only the dropped root used", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installCore(backend);
    const first = await installCurated(backend);
    const rows = await objectRows();

    const shared = first.descriptors.find((value) => value.sourceUrl === url(BOLT_NEW.id, "normal"));
    const solo = first.descriptors.find((value) => value.sourceUrl === url(BOLT_NEW.id, "art_crop"));
    expect(shared).toBeDefined();
    expect(solo).toBeDefined();
    const sharedPath = pathOf(rows, shared!.assetKey);
    const soloPath = pathOf(rows, solo!.assetKey);
    // Without this the survival assertion below would hold for the wrong
    // reason: the card back and this rung must genuinely be ONE cache entry.
    expect(pathOf(rows, assetKey("asset:v1:card_back:default"))).toBe(sharedPath);
    expect(soloPath).not.toBe(sharedPath);

    usePreferencesStore.setState({ artChain: OLDEST });
    await installCurated(backend);

    expect(cache.entries.has(sharedPath)).toBe(true);
    expect(cache.entries.has(soloPath)).toBe(false);
    const back = await backend.resolve([{ kind: "asset", key: assetKey("asset:v1:card_back:default") }]);
    expect(back.entries[0].matches).toHaveLength(1);
  });

  it("sweeps identically when a pack is removed rather than replaced", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installCore(backend);
    const first = await installCurated(backend);
    const rows = await objectRows();

    const shared = first.descriptors.find((value) => value.sourceUrl === url(BOLT_NEW.id, "normal"));
    const solo = first.descriptors.find((value) => value.sourceUrl === url(BOLT_NEW.id, "art_crop"));
    const sharedPath = pathOf(rows, shared!.assetKey);
    const soloPath = pathOf(rows, solo!.assetKey);
    expect(pathOf(rows, assetKey("asset:v1:card_back:default"))).toBe(sharedPath);

    // `remove()`'s sweep and the curated delta's sweep are one function. This
    // asserts the pre-existing caller keeps the pre-existing behaviour on the
    // same fixture the replacement path is asserted against above: shared
    // survives, sole-referenced goes.
    await backend.remove({ kind: "packs", packIds: [packId("curated")] }, "reject_dependents");

    expect(cache.entries.has(sharedPath)).toBe(true);
    expect(cache.entries.has(soloPath)).toBe(false);
    expect(await resolvedAssets(backend, first.descriptors)).toHaveLength(0);
    const back = await backend.resolve([{ kind: "asset", key: assetKey("asset:v1:card_back:default") }]);
    expect(back.entries[0].matches).toHaveLength(1);
  });

  it("clears rows and images stranded at a cancelled install's digest", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const failed = await failures(backend);
    const first = await installCurated(backend);

    // Strand a PARTIAL membership at a second digest. `markComplete` writes
    // rows during the download, so the reused ones and the one image served
    // before the outage all land at this digest while the packs row still names
    // the first — and cancelling leaves them there.
    usePreferencesStore.setState({ artChain: OLDEST });
    imagesServed = 0;
    imageBudget = 1;
    const stranded = await startCurated(backend);
    await vi.waitFor(() => { expect(failed).toHaveLength(1); }, { timeout: 5000 });
    await backend.cancel(stranded.operation);

    const stalled = await objectRows();
    const abandoned = stalled.filter((row) => row.root === stranded.digest);
    // Non-vacuity, and stated as a PROPERTY rather than as a named asset so the
    // assertion cannot start passing or failing for fixture-ordering reasons:
    // the stranded root must really hold rows, and exactly one of them must
    // reference an image that nothing else in the database does. Without that
    // one, "no cache entries left behind" would be unmeasurable — every other
    // path it holds is shared with a root that survives.
    expect(abandoned.length).toBeGreaterThan(0);
    const orphans = abandoned.filter((row) => stalled.filter((other) => other.path === row.path).length === 1);
    expect(orphans).toHaveLength(1);
    const orphanPath = orphans[0].path;

    usePreferencesStore.setState({ artChain: MIDDLE });
    imageBudget = Number.POSITIVE_INFINITY;
    const third = await installCurated(backend);
    expect(third.digest).not.toBe(first.digest);
    expect(third.digest).not.toBe(stranded.digest);

    // Every root but the installed one is gone, rows and images alike. Nothing
    // else could ever have reached those rows: their root names no pack, so
    // both the replace path and `remove()` filter right past them.
    const survivors = await objectRows();
    expect(new Set(survivors.map((row) => row.root))).toEqual(new Set([third.digest]));
    expect(survivors).toHaveLength(third.descriptors.length);
    expect(cache.entries.has(orphanPath)).toBe(false);
    // ...and the sweep is discriminating rather than indiscriminate: the shared
    // canonical images the stranded root also referenced are still here.
    expect(await resolvedAssets(backend, third.descriptors)).toHaveLength(third.descriptors.length);
  });

  it("does not leave a curated operation downloading when the planner throws", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { selector, descriptors } = await curatedSelector();
    const failed = await failures(backend);

    failImages = true;
    const started = await backend.start({ kind: "install", selector, objectEstimate: descriptors.length });
    if (started.status !== "started") throw new Error("curated install did not start");
    await vi.waitFor(() => { expect(failed).toHaveLength(1); }, { timeout: 5000 });
    // The over-reach control, in the same test: a transient failure must leave
    // the record resumable, which is the state the containment below must not
    // start claiming for everything.
    expect(failed[0].error).toBe("network");
    expect((await backend.operationStatus(started.operationId)).state).toBe("downloading");

    // An unexpected throw from inside the planner. The value is not the point
    // and the containment makes no claim about causes: what matters is that
    // something below `curatedDescriptors` throws an error that is not already
    // a backend error, which is the class that used to reach `run()` naked,
    // classify as the retryable `storage` default, and leave the record
    // `downloading` — Resume its only enabled control, every durable mutation
    // disabled, and `create()`'s pending loop re-failing on every launch.
    usePreferencesStore.setState({ artOverrides: null as unknown as Record<string, CardArtOverride> });
    failImages = false;

    await backend.start({ kind: "resume", operationId: started.operationId });
    await vi.waitFor(async () => {
      expect((await backend.operationStatus(started.operationId)).state).not.toBe("downloading");
    }, { timeout: 5000 });

    expect((await backend.operationStatus(started.operationId)).state).toBe("cancelled");
    expect(failed[1].error).toBe("internal");
  });

  it("does not scan for donors on a non-curated install", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    // A throwing stub rather than a counting one: a scan that happened would
    // fail the install outright, so this cannot pass by the assertion being
    // evaluated before the scan it is about.
    const scan = vi.fn(async (): Promise<never> => { throw new Error("donor scan on a non-curated install"); });
    const original = internals().adoptableContent;
    internals().adoptableContent = scan;
    const restore = () => { internals().adoptableContent = original; };
    try {
      await installCore(backend);
      expect(scan).not.toHaveBeenCalled();

      // Positive reach-guard: the SAME stub is reached by a curated install on
      // the same backend, so "core did not reach it" is a property of the pack
      // gate rather than of a seam that is dead in this fixture.
      const { selector, descriptors } = await curatedSelector();
      const started = await backend.start({ kind: "install", selector, objectEstimate: descriptors.length });
      expect(started.status).toBe("started");
      await vi.waitFor(() => { expect(scan).toHaveBeenCalled(); }, { timeout: 5000 });
    } finally {
      restore();
    }
  });

  it("adopts content from a pack that is not curated", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const first = await installCurated(backend);

    // A row belonging to a DIFFERENT pack at a DIFFERENT root, carrying the same
    // (assetKey, sourceUrl) identity as one curated descriptor. Its content
    // fields are copied from a row the backend really wrote, so only the three
    // fields that make it another pack's row are hand-built. This is what an
    // installed `complete`/`printing`/`locale` pack leaves behind, and
    // `adoptableContent` opens a plain cursor with no pack filter precisely so
    // that a curated install can reuse it.
    const shared = first.descriptors.find((value) => value.sourceUrl === url(GIANT, "art_crop"));
    expect(shared).toBeDefined();
    const source = (await objectRows()).find((row) => row.assetKey === shared!.assetKey);
    expect(source).toBeDefined();
    const foreignPack = packId("complete");
    const foreignRoot = "e".repeat(64) as CatalogRoot;
    const database = await openDB(DATABASE);
    await database.put("objects", { ...source, id: `${foreignRoot}:${foreignPack}:${shared!.assetKey}`, root: foreignRoot, packId: foreignPack });
    await database.put("packs", { id: foreignPack, packId: foreignPack, root: foreignRoot, dependencies: [], operationId: "foreign-operation" });
    database.close();

    // Drop the curated pack entirely, so nothing curated is left to adopt FROM
    // and any reuse below has to have crossed a pack boundary. The foreign row
    // is what keeps this one cache entry alive through `remove()`'s sweep —
    // asserted, because if it did not the reinstall would simply re-fetch.
    const donorPath = source!.path;
    await backend.remove({ kind: "packs", packIds: [packId("curated")] }, "reject_dependents");
    expect((await objectRows()).filter((row) => row.packId === packId("curated"))).toHaveLength(0);
    expect(cache.entries.has(donorPath)).toBe(true);
    requested = [];

    const second = await installCurated(backend);

    expect(second.digest).toBe(first.digest);
    expect(imageRequests()).toHaveLength(second.descriptors.length - 1);
    expect(imageRequests()).not.toContain(url(GIANT, "art_crop"));
    // The fetch count above is the assertion that discriminates adoption, and
    // it is the only one that can: paths are content-addressed, so a re-download
    // lands on the SAME path with the SAME bytes. MEASURED — pack-filtering
    // `adoptableContent` to curated rows fails the count (6 rather than 5) and
    // nothing below it. What these two add is that the row the adopt path wrote
    // is a usable one: it resolves, at the donor's entry, with the donor's
    // bytes, rather than pointing somewhere the cache cannot serve.
    const match = (await resolvedAssets(backend, second.descriptors))
      .find((entry) => entry.assetKey === shared!.assetKey && entry.packId === packId("curated"));
    expect(match?.url).toBe(donorPath);
    expect(cachedText(donorPath)).toBe(url(GIANT, "art_crop"));
  });

  it("keeps a membership another tab is still installing", async () => {
    // Two backend instances over ONE database, which is what two tabs are:
    // `loadVisualPackBackend` memoizes per tab, IndexedDB does not.
    const tabA = await ScryfallBrowserVisualPackBackend.create();
    const tabB = await ScryfallBrowserVisualPackBackend.create();
    await installCurated(tabA);

    // Tab A starts a second membership and parks on its Bolt images. Its
    // chain-invariant rows are adopted from the first membership and land
    // immediately: `markComplete` writes rows DURING the download, so a partial
    // membership is on disk at a root no packs row names — exactly the shape
    // the abandoned root sweep collects, and exactly the shape it must not
    // collect here.
    const release = holdImages((source) => source.includes(BOLT_OLD.id));
    usePreferencesStore.setState({ artChain: OLDEST });
    const parked = await startCurated(tabA);
    await vi.waitFor(async () => {
      expect((await objectRows()).filter((row) => row.root === parked.digest))
        .toHaveLength(CHAIN_INVARIANT_ASSETS);
    }, { timeout: 5000 });

    // Tab B installs a third membership and completes, running the sweep while
    // A's rows are on disk and A's record is still `downloading`.
    usePreferencesStore.setState({ artChain: MIDDLE });
    const other = await installCurated(tabB);
    expect(other.digest).not.toBe(parked.digest);

    release();
    await settle(tabA, parked.operation);

    // The user-facing property: A's pack is whole. Its own status is NOT a
    // witness for this and never can be — `objectsPromoted` counts
    // `operationObjects` completion flags, so it reads 6 of 6 either way; the
    // assertion below is on rows a render can actually reach.
    const status = await tabA.operationStatus(parked.operation);
    expect(status.state).toBe("completed");
    expect(status.objectsPromoted).toBe(status.objectTotal);
    expect(await resolvedAssets(tabA, parked.descriptors)).toHaveLength(parked.descriptors.length);
  });

  it("keeps a membership another tab promoted between the replace and the sweep", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installCurated(backend);

    // The window this guards is between `completePack` and the sweep, and there
    // is no cache or fetch seam inside it — so the seam is the sweep's own
    // entry. Interposing there lands the injection at the first instruction of
    // the window, where another tab's promotion would be visible to the sweep
    // and to nothing before it.
    const foreignRoot = "f".repeat(64) as CatalogRoot;
    const foreignKey = assetKey("asset:v1:canonical_card:foreign-0-full_card-normal");
    const foreignPath = "/visual-packs/v1/foreign-membership.jpg";
    let injected = false;
    const original = internals().collectCuratedGarbage;
    const restore = () => { internals().collectCuratedGarbage = original; };
    internals().collectCuratedGarbage = async function (this: unknown, ...args: never[]) {
      if (!injected) {
        injected = true;
        const database = await openDB(DATABASE);
        await database.put("objects", {
          id: `${foreignRoot}:${packId("curated")}:${foreignKey}`,
          root: foreignRoot,
          packId: packId("curated"),
          assetKey: foreignKey,
          candidateKeys: [],
          sourceUrl: url("foreign", "normal"),
          object: foreignRoot,
          byteLength: 3,
          media: "image/jpeg",
          path: foreignPath,
        });
        await database.put("packs", { id: packId("curated"), packId: packId("curated"), root: foreignRoot, dependencies: [], operationId: "foreign-operation" });
        database.close();
        cache.entries.set(foreignPath, { body: new TextEncoder().encode("bar"), type: "image/jpeg" });
      }
      return original.apply(this, args);
    };
    try {
      usePreferencesStore.setState({ artChain: OLDEST });
      await installCurated(backend);
      expect(injected).toBe(true);

      const survivors = (await objectRows()).filter((row) => row.root === foreignRoot);
      expect(survivors).toHaveLength(1);
      expect(cache.entries.has(foreignPath)).toBe(true);
    } finally {
      restore();
    }
  });

  // --- what a sync WOULD do, before it does it -----------------------------
  //
  // The install tests above measure the delta by watching an install run.
  // `curatedDrift` answers the same question ahead of time, which is what the
  // panel renders and what the pre-flight storage gate is allowed to refuse on.

  it("reports an uninstalled curated pack as all-add", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const { descriptors } = await curatedSelector();

    const drift = await measuredDrift(backend);

    // The first-install case, pinned so the delta work cannot quietly report
    // nothing to do where there is no pack at all.
    expect(drift.installedDigest).toBeNull();
    expect(drift).toMatchObject({ add: descriptors.length, remove: 0, refresh: 0 });
  });

  it("reports a moved source URL as a refresh, with identical asset keys", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const first = await installCurated(backend);

    const moved = await moveGiantNormal("moved");
    // The map is edited IN PLACE and the plan memo keys on preference identity
    // and deck text, neither of which moved — so the miss has to be forced.
    usePreferencesStore.setState({ artOverrides: {} });

    const drift = await measuredDrift(backend);

    // THE CASE TWO CATEGORIES CANNOT EXPRESS: the pack is out of date — the
    // digest says so — while both asset-key sets are identical. An
    // add/remove-only diff renders this as "0 to add, 0 to remove" beside an
    // enabled Sync.
    expect(drift.membershipDigest).not.toBe(first.digest);
    expect(drift.installedDigest).toBe(first.digest);
    const installedKeys = (await objectRows()).map((row) => row.assetKey).sort();
    const plannedKeys = (await curatedSelector()).descriptors.map((value) => value.assetKey).sort();
    expect(plannedKeys).toEqual(installedKeys);
    // `normal` moved and `small` is derived from it; `art_crop` did not.
    expect(drift).toMatchObject({ add: 0, remove: 0, refresh: 2 });
    expect(moved).toBe(url("moved", "normal"));
  });

  it("counts a row written before source URLs were recorded as a refresh", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const first = await installCurated(backend);
    expect(await dropStoredSourceUrls()).toBe(first.descriptors.length);
    usePreferencesStore.setState({ artChain: OLDEST });
    requested = [];

    const drift = await measuredDrift(backend);

    // Such a row cannot say which URL its bytes came from, so `installObject`
    // will never reuse it and the sync WILL fetch it. Counting it as unchanged
    // would under-report the download; counting it as an add would claim the
    // slot is not installed. It is a refresh, and it must not throw on the
    // absent field. The chain-invariant assets are exactly the ones that would
    // otherwise have been reported as unchanged.
    expect(drift).toMatchObject({ add: 3, remove: 3, refresh: CHAIN_INVARIANT_ASSETS });

    // The figure is only worth anything if it predicts the real download, so
    // run the sync and compare — the whole membership, which is exactly what
    // the no-source-URL reuse test above measures from the other end.
    await installCurated(backend);
    expect(imageRequests()).toHaveLength(drift.add + drift.refresh);
  });

  it("counts only the installed root's rows, not ones stranded at an abandoned digest", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const failed = await failures(backend);
    const installed = await installCurated(backend);

    // Strand a PARTIAL third membership. `markComplete` writes rows DURING the
    // download, so the adopted ones and the single image served before the
    // outage land at a root that no packs row names.
    usePreferencesStore.setState({ artChain: MIDDLE });
    imagesServed = 0;
    imageBudget = 1;
    const stranded = await startCurated(backend);
    await vi.waitFor(() => { expect(failed).toHaveLength(1); }, { timeout: 5000 });
    imageBudget = Number.POSITIVE_INFINITY;

    // Reach guard: the abandoned root must really hold a row whose asset key
    // the installed membership does not, or the filter under test has nothing
    // to filter and the counts below would come out right either way.
    const installedKeys = new Set(installed.descriptors.map((value) => value.assetKey));
    await vi.waitFor(async () => {
      const strays = (await objectRows())
        .filter((row) => row.root === stranded.digest && !installedKeys.has(row.assetKey));
      expect(strays).toHaveLength(1);
    }, { timeout: 5000 });

    usePreferencesStore.setState({ artChain: OLDEST });
    const drift = await measuredDrift(backend);

    // Bolt's three rungs move, Giant's three do not, and the stray is NOT a
    // fourth thing to remove: it belongs to an abandoned membership, which is
    // `collectCuratedGarbage`'s business. Counting it here would tell the user
    // their sync has work that has nothing to do with their pack.
    expect(drift.installedDigest).toBe(installed.digest);
    expect(drift).toMatchObject({ add: 3, remove: 3, refresh: 0 });
  });

  it("separates the assets a new chain adds from the ones it leaves alone", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    await installCurated(backend);
    usePreferencesStore.setState({ artChain: OLDEST });
    requested = [];

    const drift = await measuredDrift(backend);

    // The control for the two tests above: with every stored source URL intact
    // and none of them moved, `refresh` is zero. Without this, a diff that
    // reported everything as a refresh would satisfy both of them.
    expect(drift).toMatchObject({ add: 3, remove: 3, refresh: 0 });
    await installCurated(backend);
    expect(imageRequests()).toHaveLength(drift.add + drift.refresh);
  });

  // --- what the SAVED DECKS contribute to a membership ----------------------
  //
  // These live here, rather than beside the plan memo's own tests, because
  // `deckPrintings` resolves a deck's card names through `resolveOracleIdSync`
  // — which reads the module global `loadScryfallData` assigns. A file that
  // module-mocks the loader never sets it, so the resolver would answer `null`
  // for every name and a deck would silently pin nothing. Here the REAL loader
  // runs against a `/scryfall-data.json` served by the fetch stub, which is the
  // production ordering: `planMembership` awaits the loader before it reads a
  // single deck.
  describe("saved deck printings", () => {
    afterEach(() => {
      for (const name of ["Burn", "Plain", "Riders"]) localStorage.removeItem(STORAGE_KEY_PREFIX + name);
    });

    it("contributes nothing for a deck list that pins no printing", async () => {
      usePreferencesStore.setState({ artChain: SOURCE_THEN_NEWEST(), artOverrides: {} });
      const withoutDeck = await planCuratedPack();

      // A plain "4 Lightning Bolt" list, which is what most saved decks are:
      // `sourcePrinting` is optional and only a `(SET) NUM` annotation sets it.
      // The common case, and the one a fixture can silently fall into — hence
      // the two tests below pin the discriminating case explicitly.
      saveDeck("Plain", [{ count: 4, name: "Lightning Bolt" }]);
      usePreferencesStore.setState({ artChain: SOURCE_THEN_NEWEST(), artOverrides: {} });
      const withDeck = await planCuratedPack();

      expect(withDeck.membershipDigest).toBe(withoutDeck.membershipDigest);
    });

    it("plans the printing a saved deck pins, which the art chain alone would not select", async () => {
      usePreferencesStore.setState({ artChain: SOURCE_THEN_NEWEST(), artOverrides: {} });
      const withoutDeck = await planCuratedPack();

      saveDeck("Burn", [
        { count: 4, name: "Lightning Bolt", sourcePrinting: { setCode: "LEA", collectorNumber: "1" } },
      ]);
      usePreferencesStore.setState({ artChain: SOURCE_THEN_NEWEST(), artOverrides: {} });
      const withDeck = await planCuratedPack();

      // The deck's printing named INDEPENDENTLY, by planning the membership a
      // chain pinned to LEA produces. Asserting on a count, or on "more keys
      // than before", would pass for a deck that resolved to nothing and a
      // chain that happened to move.
      const lea = await membershipForSet("lea");
      expect(exactKeys(lea)).toHaveLength(3);

      for (const key of exactKeys(lea)) {
        expect(exactKeys(withDeck)).toContain(key);
        // The other half of the claim, and the half a single positive assertion
        // cannot make: without the deck these keys are unreachable, so their
        // presence is the DECK's doing and not the chain's.
        expect(exactKeys(withoutDeck)).not.toContain(key);
      }
      // ...and it ADDS to the chain's own answer rather than replacing it: the
      // no-source render context still resolves through `newest`.
      for (const key of exactKeys(withoutDeck)) expect(exactKeys(withDeck)).toContain(key);
      expect(exactKeys(withDeck)).toHaveLength(exactKeys(withoutDeck).length + exactKeys(lea).length);
    });

    it("resolves an accented deck name through the app's folded-name index", async () => {
      usePreferencesStore.setState({ artChain: SOURCE_THEN_NEWEST(), artOverrides: {} });
      const withoutDeck = await planCuratedPack();

      // Typed WITHOUT the accent, which is what a decklist pasted out of a text
      // file carries. Bare lowercasing yields "eowyn, fearless knight" and the
      // card is stored under "éowyn, ..."; only the folded-name index bridges
      // them, and that index exists only once the real loader has resolved.
      //
      // In the SIDEBOARD, which pins art exactly as the main deck does: the
      // builder renders those cards too, and both slots are `DeckEntry[]`. The
      // remaining slots — commander, companion, stickers, planar, scheme,
      // signature spell — carry no printing identity at all.
      saveDeck("Riders", [], [
        { count: 1, name: "Eowyn, Fearless Knight", sourcePrinting: { setCode: "SLD", collectorNumber: "1" } },
      ]);
      usePreferencesStore.setState({ artChain: SOURCE_THEN_NEWEST(), artOverrides: {} });
      const withDeck = await planCuratedPack();

      const sld = await membershipForSet("sld");
      // Only Éowyn has an SLD printing, so this names her pinned art and nothing
      // else — the assertions below are about that card, not about a count.
      expect(exactKeys(sld)).toHaveLength(3);
      for (const key of exactKeys(sld)) {
        expect(exactKeys(withDeck)).toContain(key);
        expect(exactKeys(withoutDeck)).not.toContain(key);
      }
    });
  });

  it("settles the in-flight downloads before a failure leaves the selector", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();
    const failed = await failures(backend);

    // Park the SECOND image fetch, whatever it turns out to be, and starve
    // every one after it: the first is served, the second waits at the gate,
    // and the rest 503. That records a `failure`, which `schedule` re-throws
    // on its next call — the unwind this test is about, with a task provably
    // still running. Counting fetches rather than naming a card keeps the
    // setup independent of the order descriptors happen to be visited in.
    let seen = 0;
    const release = holdImages(() => (seen += 1) === 2);
    imagesServed = 0;
    imageBudget = 1;
    const started = await startCurated(backend);

    // Reach guard: the membership must be big enough to have a third fetch to
    // fail on, or nothing is ever in flight at the unwind and the assertion
    // below would hold for the wrong reason.
    expect(started.descriptors.length).toBeGreaterThanOrEqual(3);
    await vi.waitFor(() => { expect(imageRequests().length).toBeGreaterThan(2); }, { timeout: 5000 });

    // THE INVARIANT. A failure must not surface while a download is still in
    // flight. `run()` terminates the record the moment it does, and a task
    // that outlives that write goes on to `markComplete` anyway — adding an
    // `objects` row at this root and incrementing `objectsPromoted` on an
    // operation the worker has already finished with, past the
    // `collectCuratedGarbage` that would have reclaimed it.
    await new Promise((resolve) => { setTimeout(resolve, 250); });
    expect(failed).toHaveLength(0);

    // Releasing the parked task is what lets the failure through.
    imageBudget = Number.POSITIVE_INFINITY;
    release();
    await vi.waitFor(() => { expect(failed).toHaveLength(1); }, { timeout: 5000 });
  });

  it("writes nothing more once cancel has returned", async () => {
    const backend = await ScryfallBrowserVisualPackBackend.create();

    // Park a download and wait until it is provably AT the gate, so a task is
    // certainly past the guard and inside `fetchImage` when the cancel lands.
    // `installObject` consults `signal` only there — its donor-reuse and
    // cache-hit paths never do — so such a task reaches `markComplete`
    // whatever the abort says. Waiting on a request COUNT instead would not
    // pin this down: the batch reaches the gate at a variable point, and the
    // assertion below then holds or fails on timing rather than on behaviour.
    let seen = 0;
    let parked = false;
    let heldSource: string | null = null;
    const release = holdImages((source) => {
      if (!source.startsWith("https://cards.scryfall.io/")) return false;
      if ((seen += 1) !== 2) return false;
      parked = true;
      heldSource = source;
      return true;
    });
    const started = await startCurated(backend);
    await vi.waitFor(() => { expect(parked).toBe(true); }, { timeout: 5000 });

    // Wait until the cancellation is durable before releasing the held image:
    // its promotion therefore proves the legacy non-Deck path is allowed to
    // finish while `cancel()` is still waiting for its worker.
    const pending = backend.cancel(started.operation);
    await vi.waitFor(async () => {
      expect((await backend.operationStatus(started.operation)).state).toBe("cancel_requested");
    }, { timeout: 5000 });
    if (!heldSource) throw new Error("held source missing");
    expect((await objectRows()).filter((row) =>
      row.root === started.digest && row.packId === packId("curated") && row.sourceUrl === heldSource,
    )).toHaveLength(0);
    release();
    const status = await pending;
    expect(status.state).toBe("cancelled");
    // Curated cancellation waits for the held non-Deck task to promote before
    // returning. The deck-library removal fence intentionally stays stricter.
    expect((await objectRows()).filter((row) =>
      row.root === started.digest && row.packId === packId("curated") && row.sourceUrl === heldSource,
    )).toHaveLength(1);

    // THE INVARIANT, asserted on the SNAPSHOT `cancel()` returns, because that
    // is the value the panel publishes its terminal outcome from. Once it is
    // handed over, nothing may still be promoting into the operation.
    await new Promise((resolve) => { setTimeout(resolve, 400); });
    const settledRecord = await backend.operationStatus(started.operation);
    expect(settledRecord.objectsPromoted).toBe(status.objectsPromoted);
  });
});
