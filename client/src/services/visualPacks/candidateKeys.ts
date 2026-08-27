import { candidateKey } from "./types.ts";
import type { CandidateKey, CandidateKind, VisualImageRung } from "./types.ts";

export { decodeCandidateKey } from "./types.ts";
export type { CandidateKind } from "./types.ts";

export type VisualVariant = "full_card" | "art_crop";
export interface CandidateGroup { keys: CandidateKey[]; rung: VisualImageRung }

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

function unique(values: string[]): string[] {
  return [...new Set(values)];
}

function aliases(values: string[] | undefined): string[] {
  return unique((values ?? []).map(alias));
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
