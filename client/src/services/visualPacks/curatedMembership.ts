import basicLandArtCatalog from "../../../../crates/engine/data/basic-land-art-v1.json";
import { selectedPrinting, selectSubmissionPrinting } from "../artSelection.ts";
import { deriveImageUrl } from "../scryfall.ts";
import type { PrintingEntry } from "../scryfall.ts";
// Type-only, matching `artSelection.ts`: no runtime dependency is created from
// `services/` back into `hooks/`.
import type { SourcePrinting } from "../../hooks/useCardImage.ts";
import type { ArtChainEntry, CardArtOverride } from "../../stores/preferencesStore.ts";
import { canonicalDescriptors, englishDescriptors } from "./browser/descriptors.ts";
import type { CanonicalCardIdentity, DescriptorFace, ScryfallAssetDescriptor } from "./browser/descriptors.ts";
import { decodeCandidateKey } from "./candidateKeys.ts";
import { catalogRoot, packId } from "./types.ts";
import type { AssetKey, CatalogRoot, PackId } from "./types.ts";

/**
 * The subset of a `scryfall-data.json` entry the planner reads.
 *
 * Declared structurally rather than imported because `ScryfallDataEntry` is
 * private to `services/scryfall.ts`. Note what is absent: these entries carry
 * no printing `id`, so images from this entry use the `canonical_card` asset form.
 */
export interface CuratedCardEntry {
  readonly oracle_id: string;
  readonly name: string;
  /** Lowercased face names in Scryfall's `card_faces` order. */
  readonly face_names: readonly string[];
  readonly faces: readonly ImageFace[];
}

/** A stored face's image URLs. Both are nullable in the generated data. */
interface ImageFace {
  readonly normal?: string | null;
  readonly art_crop?: string | null;
}

/** A printing a saved deck names via its `(SET) NUM` annotation. */
export interface CuratedDeckPrinting {
  readonly oracleId: string;
  readonly source: SourcePrinting;
}

export interface CuratedMembershipInput {
  /** The pack the descriptors belong to. Curated packs include catalog choices. */
  readonly packId: PackId;
  /** The `scryfall-data.json` map, keyed by oracle id AND by lowercased name. */
  readonly cards: Readonly<Record<string, CuratedCardEntry>>;
  /** The `scryfall-printings.json` map, keyed by oracle id. */
  readonly printings: Readonly<Record<string, PrintingEntry[]>>;
  readonly artChain: ArtChainEntry[];
  readonly artOverrides: Record<string, CardArtOverride>;
  /**
   * When supplied, plans only these Oracle identities. Omitting this keeps the
   * curated pack's all-card membership semantics unchanged.
   */
  readonly includedOracleIds?: ReadonlySet<string>;
  readonly deckPrintings?: readonly CuratedDeckPrinting[];
}

export interface CuratedMembership {
  /** Ascending by `assetKey`, so the list is a function of its content. */
  readonly descriptors: readonly ScryfallAssetDescriptor[];
  /** SHA-256 over each descriptor's asset key, source URL, and sorted unique candidate keys. */
  readonly membershipDigest: CatalogRoot;
}

/** `scryfall-data.json` prefixes token keys; tokens resolve through
 *  `tokenCandidateGroups`, so their images would download and never resolve. */
const TOKEN_KEY_PREFIX = "token:";
const CURATED_PACK = packId("curated");

/**
 * The rung URLs one stored face offers.
 *
 * Neither `scryfall-data.json` nor `scryfall-printings.json` stores `small`;
 * it is derived from `normal`, exactly as `scryfall.ts`'s own face resolver
 * does. Both stored URLs are nullable — the printings generator writes an
 * explicit null when Scryfall has no image for a face — so each is admitted
 * only when present, and `small` only when there is a `normal` to derive from.
 */
function rungUrls(face: ImageFace | undefined): Record<string, string> {
  const images: Record<string, string> = {};
  if (face?.normal) {
    images.normal = face.normal;
    images.small = deriveImageUrl(face.normal, "small");
  }
  if (face?.art_crop) images.art_crop = face.art_crop;
  return images;
}

function descriptorFaces(entry: CuratedCardEntry, faces: readonly ImageFace[]): DescriptorFace[] {
  return faces.map((face, faceIndex) => ({
    name: entry.face_names[faceIndex] ?? entry.name,
    images: rungUrls(face),
  }));
}

/**
 * Every printing the app could display for this card under stored preferences.
 *
 * `selectedPrinting` is the single authority for "which printing", and this
 * reduces to that one question asked once per render context the card appears
 * in: with no deck source — every battlefield, hand, search and deck-builder
 * render — and once per distinct printing a decklist names.
 *
 * The no-source context is load-bearing for two reasons. Most cards are in no
 * deck at all, so `sources` is empty and it is the only context there is. And
 * a chain naming `source_printing` makes the two genuinely diverge: without a
 * source that entry matches nothing and the chain falls through to its next
 * entry, while with one it resolves the deck's printing — two different arts,
 * both displayed, at different sites.
 *
 * With an empty chain and an annotated deck card, the deck's printing becomes
 * the ordinary offline image. Its Oracle candidates let battlefield, hand,
 * and search renders resolve even though their online image is canonical.
 *
 * Preference and deck contexts select the printings themselves. Catalog
 * choices are added separately after the primary art is decided.
 */
function orderedSourceContexts(sources: readonly SourcePrinting[]): SourcePrinting[] {
  return [...sources].sort((left, right) => {
    const leftSet = left.setCode.toLowerCase();
    const rightSet = right.setCode.toLowerCase();
    const leftCollector = left.collectorNumber.toLowerCase();
    const rightCollector = right.collectorNumber.toLowerCase();
    return leftSet < rightSet ? -1 : leftSet > rightSet ? 1
      : leftCollector < rightCollector ? -1 : leftCollector > rightCollector ? 1
        : left.setCode < right.setCode ? -1 : left.setCode > right.setCode ? 1
          : left.collectorNumber < right.collectorNumber ? -1 : left.collectorNumber > right.collectorNumber ? 1 : 0;
  });
}

/**
 * Stored/no-source selection is the planner-primary authority. Deck-source
 * contexts follow in a case-insensitive stable order with original-value ties
 * so the emitted fallback never depends on saved-deck or map iteration order.
 */
function selectedPrintings(
  oracleId: string,
  printings: PrintingEntry[],
  input: CuratedMembershipInput,
  sources: readonly SourcePrinting[],
): PrintingEntry[] {
  const { artChain, artOverrides } = input;
  const byId = new Map<string, PrintingEntry>();
  for (const source of [undefined, ...orderedSourceContexts(sources)]) {
    const printing = selectedPrinting(oracleId, printings, artChain, artOverrides, source);
    // Source contexts remain distinct through selection because collector
    // annotations can be case-sensitive. Only successful printing identity is
    // deduplicated, after the authority has resolved it.
    if (printing) byId.set(printing.id, printing);
  }
  return [...byId.values()];
}

/** Keep source identity while limiting secondary render art to non-semantic
 * lookups and supplemental art to exact lookups. */
function withoutBroadSemanticCandidates(
  descriptor: ScryfallAssetDescriptor,
  scope: "secondary_render" | "exact_only",
): ScryfallAssetDescriptor {
  return {
    ...descriptor,
    candidateKeys: descriptor.candidateKeys.filter((key) => {
      const [kind] = decodeCandidateKey(key);
      if (scope === "exact_only") return kind === "english_printing" || kind === "source_printing";
      return kind !== "oracle_face" && kind !== "name_face";
    }),
  };
}

function cardDescriptors(
  entry: CuratedCardEntry,
  input: CuratedMembershipInput,
  sources: readonly SourcePrinting[],
): ScryfallAssetDescriptor[] {
  // The app keys these two things on DIFFERENT forms of the oracle id, so the
  // planner must too. Preference and printing lookups use the id exactly as
  // stored: `getCardPrintings` indexes `scryfall-printings.json`, whose
  // generator writes the id unmodified, and `PrintingPickerModal` writes
  // `artOverrides` under that same value. Candidate keys use the lowercased
  // form, because `scryfall.ts` stamps `CardImageAsset.semantic.oracleId` with
  // `.toLowerCase()` before `useCardImage` builds its lookup groups from it.
  // Today every stored id is already lowercase, so the two coincide; keying
  // both on one form would silently rely on that.
  const oracleId = entry.oracle_id;
  const candidateOracleId = oracleId.toLowerCase();
  const printings = input.printings[oracleId] ?? [];
  const describePrinting = (printing: PrintingEntry) =>
    englishDescriptors(input.packId, {
      id: printing.id,
      oracleId: candidateOracleId,
      set: printing.set,
      collector: printing.collector_number,
      name: entry.name,
      faces: descriptorFaces(entry, printing.faces),
    });
  const selected = selectedPrintings(oracleId, printings, input, sources).map((printing) => ({
    printing,
    descriptors: describePrinting(printing),
  }));
  const primary = selected.find(({ descriptors }) => descriptors.length > 0)?.printing.id;
  const descriptors = selected.flatMap(({ printing, descriptors }) =>
    printing.id === primary
      ? descriptors
      : descriptors.map((value) => withoutBroadSemanticCandidates(value, "secondary_render")));
  // A selected printing without images contributes no rungs. Use canonical
  // art if none of the selected contexts produced an image.
  if (primary === undefined) {
    const canonical: CanonicalCardIdentity = {
      oracleId: candidateOracleId,
      name: entry.name,
      faces: descriptorFaces(entry, entry.faces),
    };
    descriptors.push(...canonicalDescriptors(input.packId, canonical));
  }

  const lands = basicLandArtCatalog.lands;
  if (!Object.prototype.hasOwnProperty.call(lands, candidateOracleId)) return descriptors;

  // Keep keys on any printing already supplied by a selected context.
  const seen = new Set(selected.filter((value) => value.descriptors.length > 0).map(({ printing }) => printing.id));
  const addExactPrinting = (printing: PrintingEntry) => {
    if (seen.has(printing.id)) return;
    seen.add(printing.id);
    descriptors.push(...describePrinting(printing).map((value) => withoutBroadSemanticCandidates(value, "exact_only")));
  };
  // Submission can bind a basic to a printing ordinary rendering does not
  // select (an import over a general preference, or a preference past a stale
  // pin), so every pack carries it for exact lookups.
  for (const source of [undefined, ...sources]) {
    const printing = selectSubmissionPrinting(oracleId, printings, input.artChain, input.artOverrides, source);
    if (printing) addExactPrinting(printing);
  }
  if (input.packId !== CURATED_PACK) return descriptors;
  for (const choice of lands[candidateOracleId as keyof typeof lands].choices) {
    if (seen.has(choice.scryfall_id)) continue;
    const printing = printings.find((row) =>
      row.id === choice.scryfall_id && row.set === choice.set && row.collector_number === choice.collector_number);
    if (printing) addExactPrinting(printing);
  }
  return descriptors;
}

async function sha256Hex(value: string): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/**
 * Plan ordinary art under the user's preferences and deck contexts.
 * Include exact deck-submission printings in both pack types and exact
 * catalog alternatives in curated packs.
 *
 * The digest covers each descriptor's source URL as well as its asset key. A
 * `canonical_card` key names no printing, so the bytes behind it are whatever
 * `scryfall-data.json` currently supplies — a pipeline regeneration can move
 * the URL under an unchanged key. Digesting keys alone would leave that
 * invisible and pin stale art with no path to a refresh. The digest also
 * identifies installed lookup metadata for every pack, so changing which
 * image answers a candidate can refresh the pack with cached bytes.
 */
export async function planCuratedMembership(
  input: CuratedMembershipInput,
): Promise<CuratedMembership> {
  // A resolver can return the stored identity's original casing while the
  // data map is deduplicated case-insensitively below. Fold the optional
  // boundary once so a deck-scoped caller has the same matching semantics as
  // the existing source-printing path.
  const includedOracleIds = input.includedOracleIds === undefined
    ? undefined
    : new Set([...input.includedOracleIds].map((oracleId) => oracleId.toLowerCase()));
  // Case-folded on both sides: whether a caller's oracle id arrived from the
  // engine or from a `scryfall-data` entry, the same card must match.
  const sourcesByOracleId = new Map<string, SourcePrinting[]>();
  for (const { oracleId, source } of input.deckPrintings ?? []) {
    const key = oracleId.toLowerCase();
    const existing = sourcesByOracleId.get(key);
    if (existing) existing.push(source);
    else sourcesByOracleId.set(key, [source]);
  }

  // `scryfall-data.json` keys each card by its oracle id AND by one or two
  // name forms, unevenly. Iterating keys would emit most cards two or three
  // times over, so distinctness is tracked on the oracle id itself.
  const seen = new Set<string>();
  const byAssetKey = new Map<AssetKey, ScryfallAssetDescriptor>();
  for (const [key, entry] of Object.entries(input.cards)) {
    if (key.startsWith(TOKEN_KEY_PREFIX)) continue;
    const oracleId = entry.oracle_id.toLowerCase();
    if (seen.has(oracleId)) continue;
    seen.add(oracleId);
    if (includedOracleIds && !includedOracleIds.has(oracleId)) continue;
    for (const value of cardDescriptors(entry, input, sourcesByOracleId.get(oracleId) ?? [])) {
      byAssetKey.set(value.assetKey, value);
    }
  }

  const descriptors = [...byAssetKey.values()].sort((a, b) =>
    a.assetKey < b.assetKey ? -1 : a.assetKey > b.assetKey ? 1 : 0);
  const digestInput = JSON.stringify(descriptors.map(({ assetKey, sourceUrl, candidateKeys }) =>
    [assetKey, sourceUrl, [...new Set(candidateKeys)].sort()]));
  return { descriptors, membershipDigest: catalogRoot(await sha256Hex(digestInput)) };
}
