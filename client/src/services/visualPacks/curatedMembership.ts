import { selectedPrinting } from "../artSelection.ts";
import { deriveImageUrl } from "../scryfall.ts";
import type { PrintingEntry } from "../scryfall.ts";
// Type-only, matching `artSelection.ts`: no runtime dependency is created from
// `services/` back into `hooks/`.
import type { SourcePrinting } from "../../hooks/useCardImage.ts";
import type { ArtChainEntry, CardArtOverride } from "../../stores/preferencesStore.ts";
import { canonicalDescriptors, englishDescriptors } from "./browser/descriptors.ts";
import type { CanonicalCardIdentity, DescriptorFace, ScryfallAssetDescriptor } from "./browser/descriptors.ts";
import { decodeCandidateKey } from "./candidateKeys.ts";
import { catalogRoot } from "./types.ts";
import type { AssetKey, CatalogRoot, PackId } from "./types.ts";

/**
 * The subset of a `scryfall-data.json` entry the planner reads.
 *
 * Declared structurally rather than imported because `ScryfallDataEntry` is
 * private to `services/scryfall.ts`. Note what is absent: these entries carry
 * no printing `id`, which is why a card with no selected printing can only be
 * expressed in the `canonical_card` asset form.
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
  /**
   * The pack the descriptors belong to. Supplied by the caller rather than
   * hardcoded: the `curated` pack id is not admissible until the selector
   * lands, and the planner has no other reason to know its own pack.
   */
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
  /** sha256 over the sorted `"<assetKey>\t<sourceUrl>"` lines. */
  readonly membershipDigest: CatalogRoot;
}

/** `scryfall-data.json` prefixes token keys; tokens resolve through
 *  `tokenCandidateGroups`, so their images would download and never resolve. */
const TOKEN_KEY_PREFIX = "token:";

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
 * It is NOT needed to keep a deck card's plain renders on canonical art, and
 * this is an ACCEPTED DIVERGENCE rather than an oversight. Under an EMPTY
 * chain a deck card plans exactly one printing: the no-source answer is null
 * (no override, no chain, no source), the deck context yields the deck's
 * printing, and the exclusivity gate below therefore emits no canonical form.
 * That card's battlefield, hand and search renders resolve through the oracle
 * group onto the deck printing's `exact_printing` descriptor, so offline they
 * show the deck's art where online they would show canonical. Emitting both
 * forms to close the gap would be worse, not better: they would share this
 * card's oracle-group candidate keys and `sortMatches` orders
 * `canonical_card:` first, so canonical would win `sources[0]` for the DECK
 * render too — breaking the half of the feature that motivated it. Of the
 * three available behaviors this is the best one.
 *
 * Nothing is added on top of those answers. Two extras were tried and removed:
 * the chain winner beside a pinned `artOverride`, and a deck's own printing
 * beside whatever the chain resolves for that deck render. Neither is ever
 * displayed — an override suppresses the chain entirely, and `useCardImage`
 * consults a deck source only through its `else if (sourcePrinting)` branch,
 * reachable solely when `artChain` is empty because it follows
 * `else if (artChain.length > 0)`, or through a chain that itself names
 * `source_printing`; both are already what the call below returns. Each extra
 * would cost bytes on a feature whose whole purpose is fewer bytes, and would
 * put a second `exact_printing` under one card's oracle-group candidate keys —
 * which makes `unambiguousCompanion` see two candidates and drop rung pairing,
 * and lets `sortMatches` order `canonical_card:` ahead of `exact_printing:` so
 * the wrong art lands at `sources[0]`. Both `artChain` and `artOverrides` are
 * digest inputs, so un-pinning an override or switching chains changes the
 * digest, reports drift, and lets the delta sync fetch what is newly
 * displayed. That is what the drift mechanism is for.
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

/** Only one exact printing may answer broad oracle/name semantic lookup for a
 * card. Source-specific identity remains on every English descriptor. */
function withoutBroadSemanticCandidates(descriptor: ScryfallAssetDescriptor): ScryfallAssetDescriptor {
  return {
    ...descriptor,
    candidateKeys: descriptor.candidateKeys.filter((key) => {
      const [kind] = decodeCandidateKey(key);
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
  const printings = selectedPrintings(
    oracleId,
    input.printings[oracleId] ?? [],
    input,
    sources,
  );
  const selected = printings.map((printing) => ({
    printing,
    descriptors: englishDescriptors(input.packId, {
      id: printing.id,
      oracleId: candidateOracleId,
      set: printing.set,
      collector: printing.collector_number,
      name: entry.name,
      faces: descriptorFaces(entry, printing.faces),
    }),
  }));
  const primary = selected.find(({ descriptors }) => descriptors.length > 0)?.printing.id;
  const exact = selected.flatMap(({ printing, descriptors }) =>
    printing.id === primary ? descriptors : descriptors.map(withoutBroadSemanticCandidates));
  // A card that produced NO `exact_printing` descriptor falls back to the
  // canonical form, and one that produced any never emits it. The trigger is
  // stated as "emitted nothing" rather than "selected nothing" because the
  // invariant being protected is emission: the two forms share this card's
  // oracle-group candidate keys, so a card reachable by BOTH returns two
  // matches for every oracle-group lookup — and `sortMatches` orders by
  // assetKey within a pack, where `canonical_card:` sorts before
  // `exact_printing:`. The canonical art would become `sources[0]` and the
  // WRONG printing would render, not merely a degraded rung pairing. A card
  // emitting no `exact_printing` descriptor cannot be reachable by both, so
  // this tests the invariant directly instead of through a proxy.
  //
  // Two situations reach it. No printing was selected: the card is absent from
  // `scryfall-printings.json`, or the chain found no winner. Or a printing was
  // selected and stores no image — `scryfall-printings.json` holds 696 such
  // printings, null on `art_crop` as well as `normal` across every face, and
  // 211 cards have no renderable printing at all. `faceImages` skips every rung
  // for those, so the older `printings.length === 0` gate left them
  // contributing nothing at all while the renderer, whose `overrideUrl` is null
  // for the same reason, falls through to canonical art — 211 cards needing the
  // network in an offline-play feature. All 211 do carry an image in
  // `scryfall-data.json`, which is what the canonical form serves. Three more
  // cards own image-less and renderable printings both, and fall back only when
  // the chain picks an image-less one — which, measured under a `newest` chain,
  // is all three of them.
  //
  // A card with SEVERAL selected printings, only some of them image-less, still
  // emits the exact form alone. Its image-less render context falls back to
  // canonical art online, and the one-form rule above is what forbids caching
  // it; that residual is the price of the rule, not an oversight.
  if (exact.length === 0) {
    const canonical: CanonicalCardIdentity = {
      oracleId: candidateOracleId,
      name: entry.name,
      faces: descriptorFaces(entry, entry.faces),
    };
    return canonicalDescriptors(input.packId, canonical);
  }
  return exact;
}

async function sha256Hex(value: string): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value)));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/**
 * Plan the curated pack: one art per card under the user's configured art
 * chain, plus the printings their overrides and decklists pin.
 *
 * The digest covers each descriptor's source URL as well as its asset key. A
 * `canonical_card` key names no printing, so the bytes behind it are whatever
 * `scryfall-data.json` currently supplies — a pipeline regeneration can move
 * the URL under an unchanged key. Digesting keys alone would leave that
 * invisible and pin stale art with no path to a refresh.
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
  const lines = descriptors.map((value) => `${value.assetKey}\t${value.sourceUrl}\n`).join("");
  return { descriptors, membershipDigest: catalogRoot(await sha256Hex(lines)) };
}
