import { candidateKey } from "./types.ts";
import type { CandidateKey, CandidateKind, VisualImageRung } from "./types.ts";

export { decodeCandidateKey } from "./types.ts";
export type { CandidateKind } from "./types.ts";

export type VisualVariant = "full_card" | "art_crop";
export interface CandidateGroup { keys: CandidateKey[]; rung: VisualImageRung }

/** Version of descriptor-to-candidate-key projection, independent of candidate:v1. */
export const CARD_CANDIDATE_PROJECTION_VERSION = 2;

function normalizeRung(rung: VisualImageRung | "large"): VisualImageRung {
  return rung === "large" ? "normal" : rung;
}

function visualTuple(
  faceIndex: number,
  variant: VisualVariant,
  rung: VisualImageRung | "large",
): readonly [number, VisualVariant, VisualImageRung] {
  return [faceIndex, variant, normalizeRung(rung)];
}

function base64url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function encodeCandidateKey(kind: CandidateKind, tuple: readonly unknown[]): CandidateKey {
  const payload = new TextEncoder().encode(`${JSON.stringify([kind, tuple])}\n`);
  return candidateKey(`candidate:v1:${kind}:${base64url(payload)}`);
}

function alias(value: string): string {
  return value.toLowerCase().normalize("NFC");
}

function isWellFormedText(value: string): boolean {
  return new TextDecoder("utf-8", { fatal: true }).decode(new TextEncoder().encode(value)) === value;
}

function semanticAlias(value: string): string {
  if (!isWellFormedText(value)) throw new Error("semantic candidate text must be well-formed");
  return alias(value);
}

function semanticText(value: string): string {
  if (!value || !isWellFormedText(value) || value.normalize("NFC") !== value) {
    throw new Error("semantic candidate text must be nonempty well-formed NFC");
  }
  return value;
}

function unique<T extends string>(values: readonly T[]): T[] {
  return [...new Set(values)];
}

function aliases(values: string[] | undefined): string[] {
  return unique((values ?? []).map(alias));
}

export interface SemanticCardCandidateIntent {
  oracleId?: string;
  sourceSetCode?: string;
  sourceCollectorNumber?: string;
  cardName: string;
  faceName: string;
  variant: VisualVariant;
  rung: VisualImageRung | "large";
}

/**
 * Caller-owned card identities available before Scryfall exposes a printing
 * id or a face index. Their order is the resolution priority for Deck Catalog
 * descriptors, and deliberately remains separate from the older Scryfall-key
 * groups above.
 */
export function semanticCardCandidateGroups(intent: SemanticCardCandidateIntent): CandidateGroup[] {
  const rung = normalizeRung(intent.rung);
  const cardName = semanticAlias(intent.cardName);
  const faceName = semanticAlias(intent.faceName);
  const hasSet = intent.sourceSetCode !== undefined;
  const hasCollector = intent.sourceCollectorNumber !== undefined;
  if (hasSet !== hasCollector) throw new Error("source printing requires set and collector number");

  const groups: CandidateGroup[] = [];
  if (hasSet && hasCollector) {
    const setCode = intent.sourceSetCode!.toLowerCase();
    const collectorNumber = semanticText(intent.sourceCollectorNumber!);
    groups.push({
      keys: unique([encodeCandidateKey("source_printing", [setCode, collectorNumber, faceName, intent.variant, rung])]),
      rung,
    });
  }
  if (intent.oracleId !== undefined) {
    groups.push({
      keys: unique([encodeCandidateKey("oracle_face", [intent.oracleId, faceName, intent.variant, rung])]),
      rung,
    });
  }
  groups.push({
    keys: unique([encodeCandidateKey("name_face", [cardName, faceName, intent.variant, rung])]),
    rung,
  });
  return groups;
}

export interface CardCandidateIntent {
  language?: string;
  englishPrintingId?: string;
  oracleId?: string;
  localizedAliases?: string[];
  englishAliases?: string[];
  oracleAliases?: string[];
  faceIndex: number;
  variant: VisualVariant;
  rung: VisualImageRung | "large";
}

export function cardCandidateGroups(intent: CardCandidateIntent): CandidateGroup[] {
  const visual = visualTuple(intent.faceIndex, intent.variant, intent.rung);
  const groups: CandidateGroup[] = [];
  if (intent.language && intent.language !== "en" && intent.englishPrintingId) {
    const keys = [
      encodeCandidateKey("localized_printing", [intent.language, intent.englishPrintingId, ...visual]),
      ...aliases(intent.localizedAliases).map((value) =>
        encodeCandidateKey("localized_alias", [intent.language, value, ...visual])),
    ];
    groups.push({ keys, rung: visual[2] });
  }
  if (intent.englishPrintingId) {
    const keys = [
      encodeCandidateKey("english_printing", [intent.englishPrintingId, ...visual]),
      ...aliases(intent.englishAliases).map((value) =>
        encodeCandidateKey("english_alias", [value, ...visual])),
    ];
    groups.push({ keys, rung: visual[2] });
  }
  if (intent.oracleId) {
    const keys = [
      encodeCandidateKey("oracle", [intent.oracleId, ...visual]),
      ...aliases(intent.oracleAliases).map((value) =>
        encodeCandidateKey("oracle_alias", [value, ...visual])),
    ];
    groups.push({ keys, rung: visual[2] });
  }
  return groups;
}

export interface TokenCandidateIntent {
  scryfallId?: string;
  oracleId?: string;
  faceName?: string;
  presetId?: string;
  faceIndex: number;
  rung: "small" | "normal" | "large";
}

export function tokenCandidateGroups(intent: TokenCandidateIntent): CandidateGroup[] {
  const visual = visualTuple(intent.faceIndex, "full_card", intent.rung);
  const face = (intent.faceName ?? "").toLowerCase().normalize("NFC");
  const reference = intent.scryfallId
    ? `printing:${intent.scryfallId}:${face}`
    : intent.oracleId
      ? `oracle:${intent.oracleId}:${face}`
      : null;
  const groups: CandidateGroup[] = [];
  if (reference) groups.push({ keys: [encodeCandidateKey("token_reference", [reference, ...visual])], rung: visual[2] });
  if (intent.presetId) {
    const presetAlias = `preset:${intent.presetId}`;
    groups.push({ keys: [encodeCandidateKey("token_alias", [presetAlias, ...visual])], rung: visual[2] });
  }
  return groups;
}

export const cardBackCandidate = (): CandidateKey => encodeCandidateKey("card_back", []);
export function manaSymbolCandidate(symbol: string): CandidateKey {
  return encodeCandidateKey("mana_symbol", [symbol]);
}
export function setIconCandidate(setCode: string): CandidateKey {
  return encodeCandidateKey("set_icon", [setCode]);
}
