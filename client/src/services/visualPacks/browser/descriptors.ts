import { cardCandidateGroups, semanticCardCandidateGroups } from "../candidateKeys.ts";
import type { VisualVariant } from "../candidateKeys.ts";
import { assetKey } from "../types.ts";
import type { AssetKey, CandidateKey, PackId, VisualImageRung, VisualPackMedia } from "../types.ts";

/**
 * One downloadable image, named by the asset key it is stored under and by the
 * candidate keys a render-time lookup may arrive with.
 */
export interface ScryfallAssetDescriptor {
  readonly packId: PackId;
  readonly assetKey: AssetKey;
  readonly candidateKeys: readonly CandidateKey[];
  readonly sourceUrl: string;
  readonly media: VisualPackMedia;
}

/** A card face as the descriptor builders see it: a display name plus the
 *  source URLs available for it, keyed by rung. */
export interface DescriptorFace {
  readonly name: string;
  readonly images: Record<string, string>;
}

/**
 * A card identified only by its oracle id. `scryfall-data.json` entries carry
 * no printing `id`, so this is all a planner working from that map has.
 */
export interface CanonicalCardIdentity {
  readonly oracleId: string;
  readonly name: string;
  readonly faces: readonly DescriptorFace[];
}

/** A card identified down to one specific printing. */
export interface CardIdentity extends CanonicalCardIdentity {
  readonly id: string;
  readonly set: string;
  readonly collector: string;
}

/**
 * The three images every card face contributes, in the order the packs store
 * them. `small` and `normal` are the two rungs of the `full_card` variant;
 * `art_crop` is both its own variant and its own rung.
 */
const IMAGE_LADDER: readonly (readonly [VisualVariant, VisualImageRung])[] = [
  ["full_card", "small"],
  ["full_card", "normal"],
  ["art_crop", "art_crop"],
];

interface FaceImage {
  readonly face: DescriptorFace;
  readonly faceIndex: number;
  readonly variant: VisualVariant;
  readonly rung: VisualImageRung;
  readonly url: string;
}

/**
 * Every face x ladder rung for which a source URL actually exists.
 *
 * The absent-URL skip is load-bearing rather than defensive: Scryfall omits
 * image variants for some printings, and `scryfall-printings.json` stores an
 * explicit null for those faces.
 */
function faceImages(card: CanonicalCardIdentity): FaceImage[] {
  return card.faces.flatMap((face, faceIndex) =>
    IMAGE_LADDER.flatMap(([variant, rung]) => {
      const url = face.images[rung];
      return url ? [{ face, faceIndex, variant, rung, url }] : [];
    }));
}

export function descriptor(
  selectedPack: PackId,
  card: CardIdentity,
  faceIndex: number,
  variant: VisualVariant,
  rung: VisualImageRung,
  sourceUrl: string,
  candidates: CandidateKey[],
): ScryfallAssetDescriptor {
  const asset = assetKey(`asset:v1:exact_printing:${card.id}-${faceIndex}-${variant}-${rung}`);
  return { packId: selectedPack, assetKey: asset, candidateKeys: candidates, sourceUrl, media: "image/jpeg" };
}

/** Descriptors for one English printing, keyed by that printing's id. */
export function englishDescriptors(selectedPack: PackId, card: CardIdentity): ScryfallAssetDescriptor[] {
  return faceImages(card).map(({ face, faceIndex, variant, rung, url }) =>
    descriptor(selectedPack, card, faceIndex, variant, rung, url, cardCandidateGroups({
      englishPrintingId: card.id,
      oracleId: card.oracleId,
      englishAliases: [card.name, face.name],
      oracleAliases: [card.name, face.name],
      faceIndex,
      variant,
      rung,
    }).flatMap((group) => group.keys).concat(semanticCardCandidateGroups({
      oracleId: card.oracleId,
      sourceSetCode: card.set,
      sourceCollectorNumber: card.collector,
      cardName: card.name,
      faceName: face.name,
      variant,
      rung,
    }).flatMap((group) => group.keys))));
}

/**
 * Descriptors for a card whose printing is unknown — the `canonical_card`
 * asset form.
 *
 * Because no printing id is available, the candidate keys are deliberately
 * limited to the oracle group. That mirrors what `useCardImage` requests when
 * no stored preference resolves to a printing: it passes an empty
 * `englishPrintingId`, so `cardCandidateGroups` emits the oracle group alone.
 * Emitting the english group here would produce keys no render ever asks for.
 */
export function canonicalDescriptors(
  selectedPack: PackId,
  card: CanonicalCardIdentity,
): ScryfallAssetDescriptor[] {
  return faceImages(card).map(({ face, faceIndex, variant, rung, url }) => ({
    packId: selectedPack,
    assetKey: assetKey(`asset:v1:canonical_card:${card.oracleId}-${faceIndex}-${variant}-${rung}`),
    candidateKeys: cardCandidateGroups({
      oracleId: card.oracleId,
      oracleAliases: [card.name, face.name],
      faceIndex,
      variant,
      rung,
    }).flatMap((group) => group.keys).concat(semanticCardCandidateGroups({
      oracleId: card.oracleId,
      cardName: card.name,
      faceName: face.name,
      variant,
      rung,
    }).flatMap((group) => group.keys)),
    sourceUrl: url,
    media: "image/jpeg" as const,
  }));
}
