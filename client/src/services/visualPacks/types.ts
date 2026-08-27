export type Brand<T, Name extends string> = T & { readonly __brand: Name };

export type AssetKey = Brand<string, "AssetKey">;
export type CandidateKey = Brand<string, "CandidateKey">;
export type CandidateKind =
  | "localized_printing" | "localized_alias"
  | "english_printing" | "english_alias"
  | "oracle" | "oracle_alias"
  | "token_reference" | "token_alias"
  | "card_back" | "mana_symbol" | "set_icon";
export type PackId = Brand<string, "PackId">;
export type CatalogRoot = Brand<string, "CatalogRoot">;
export type InstalledRevision = Brand<string, "InstalledRevision">;
export type OperationId = Brand<string, "OperationId">;

const LOWER_HEX_64 = /^[0-9a-f]{64}$/;
const LOWER_HEX_32 = /^[0-9a-f]{32}$/;
const DECIMAL = /^(0|[1-9][0-9]*)$/;
const PACK_ID = /^(complete|core|printing:[a-z0-9]{3,6}|locale:(de|es|fr|it|pt):[a-z0-9]{3,6})$/;
const ASSET_KEY = /^asset:v1:(canonical_card|exact_printing|localized_printing|token|card_back|mana_symbol|set_icon):[A-Za-z0-9_-]+$/;
const CANDIDATE_KEY = /^candidate:v1:(localized_printing|localized_alias|english_printing|english_alias|oracle|oracle_alias|token_reference|token_alias|card_back|mana_symbol|set_icon):([A-Za-z0-9_-]+)$/;
const CANDIDATE_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const CANDIDATE_SET = /^[a-z0-9]{3,6}$/;
const CANDIDATE_LOCALE = /^(de|es|fr|it|pt)$/;

function branded<T extends string>(value: string, pattern: RegExp, name: string): T {
  if (!pattern.test(value)) throw new Error(`invalid ${name}`);
  return value as T;
}

function decodeBase64url(value: string): Uint8Array {
  if (value.length % 4 === 1) throw new Error("invalid CandidateKey payload");
  const padded = value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
  const binary = atob(padded);
  return Uint8Array.from(binary, (unit) => unit.charCodeAt(0));
}

function encodeBase64url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function wellFormedCandidateText(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const unit = value.charCodeAt(index);
    if (unit >= 0xd800 && unit <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (next < 0xdc00 || next > 0xdfff) return false;
      index += 1;
    } else if (unit >= 0xdc00 && unit <= 0xdfff) {
      return false;
    }
  }
  return true;
}

function validateCandidateText(value: unknown): asserts value is string {
  if (
    typeof value !== "string"
    || !value
    || !wellFormedCandidateText(value)
    || value.normalize("NFC") !== value
  ) {
    throw new Error("candidate text must be nonempty well-formed NFC");
  }
}

function validateCandidateIdentity(value: unknown): asserts value is string {
  if (typeof value !== "string" || !CANDIDATE_UUID.test(value)) {
    throw new Error("candidate identity must be a lowercase UUID");
  }
}

function validateCandidateVisual(
  tuple: readonly unknown[],
  identityCount: number,
  token = false,
): void {
  if (tuple.length !== identityCount + 3) throw new Error("candidate tuple has wrong axis count");
  const [faceIndex, variant, rung] = tuple.slice(identityCount);
  if (!Number.isSafeInteger(faceIndex) || (faceIndex as number) < 0) {
    throw new Error("invalid face index");
  }
  if (variant === "full_card") {
    if (rung !== "small" && rung !== "normal") throw new Error("invalid visual variant/rung");
  } else if (variant === "art_crop") {
    if (rung !== "art_crop" || token) throw new Error("invalid visual variant/rung");
  } else {
    throw new Error("invalid visual variant/rung");
  }
}

function validateCandidateTuple(kind: CandidateKind, tuple: readonly unknown[]): void {
  switch (kind) {
    case "localized_printing":
      if (!CANDIDATE_LOCALE.test(tuple[0] as string)) throw new Error("invalid locale");
      validateCandidateIdentity(tuple[1]);
      validateCandidateVisual(tuple, 2);
      break;
    case "localized_alias":
      if (!CANDIDATE_LOCALE.test(tuple[0] as string)) throw new Error("invalid locale");
      validateCandidateText(tuple[1]);
      validateCandidateVisual(tuple, 2);
      break;
    case "english_printing":
    case "oracle":
      validateCandidateIdentity(tuple[0]);
      validateCandidateVisual(tuple, 1);
      break;
    case "english_alias":
    case "oracle_alias":
      validateCandidateText(tuple[0]);
      validateCandidateVisual(tuple, 1);
      break;
    case "token_alias":
      validateCandidateText(tuple[0]);
      validateCandidateVisual(tuple, 1, true);
      break;
    case "token_reference": {
      validateCandidateText(tuple[0]);
      const match = /^(printing|oracle):([^:]+):/.exec(tuple[0]);
      if (!match) throw new Error("invalid token reference");
      validateCandidateIdentity(match[2]);
      validateCandidateVisual(tuple, 1, true);
      break;
    }
    case "card_back":
      if (tuple.length !== 0) throw new Error("card back tuple must be empty");
      break;
    case "mana_symbol":
      if (tuple.length !== 1) throw new Error("mana symbol tuple has wrong axis count");
      validateCandidateText(tuple[0]);
      break;
    case "set_icon":
      if (tuple.length !== 1 || typeof tuple[0] !== "string" || !CANDIDATE_SET.test(tuple[0])) {
        throw new Error("invalid set code");
      }
      break;
  }
}

function parseCandidateKey(value: string): readonly [CandidateKind, readonly unknown[]] {
  const match = CANDIDATE_KEY.exec(value);
  if (!match) throw new Error("invalid CandidateKey");
  const bytes = decodeBase64url(match[2]);
  const payload = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  if (!payload.endsWith("\n") || payload.slice(0, -1).includes("\n")) {
    throw new Error("invalid CandidateKey payload");
  }
  const decoded: unknown = JSON.parse(payload.slice(0, -1));
  if (!Array.isArray(decoded) || decoded.length !== 2 || decoded[0] !== match[1] || !Array.isArray(decoded[1])) {
    throw new Error("invalid CandidateKey payload");
  }
  const canonical = new TextEncoder().encode(`${JSON.stringify(decoded)}\n`);
  if (encodeBase64url(canonical) !== match[2]) throw new Error("noncanonical CandidateKey");
  const kind = decoded[0] as CandidateKind;
  validateCandidateTuple(kind, decoded[1]);
  return [kind, decoded[1]];
}

export const assetKey = (value: string): AssetKey => branded(value, ASSET_KEY, "AssetKey");
export const candidateKey = (value: string): CandidateKey => {
  parseCandidateKey(value);
  return value as CandidateKey;
};
export const decodeCandidateKey = (value: string): readonly [CandidateKind, readonly unknown[]] =>
  parseCandidateKey(value);
export const packId = (value: string): PackId => branded(value, PACK_ID, "PackId");
export const catalogRoot = (value: string): CatalogRoot => branded(value, LOWER_HEX_64, "CatalogRoot");
export const operationId = (value: string): OperationId => branded(value, LOWER_HEX_32, "OperationId");
export const installedRevision = (value: string): InstalledRevision =>
  branded(value, DECIMAL, "InstalledRevision");

export function compareRevisions(left: InstalledRevision, right: InstalledRevision): number {
  const a = BigInt(left);
  const b = BigInt(right);
  return a < b ? -1 : a > b ? 1 : 0;
}

export type VisualPackMedia = "image/jpeg" | "image/svg+xml";
export type VisualImageRung = "small" | "normal" | "art_crop";

export type InstallSelector =
  | { kind: "core" }
  | { kind: "printing"; set: string }
  | { kind: "locale"; language: string; set: string }
  | { kind: "complete"; rootSha256: CatalogRoot };

export type StartRequest =
  | { kind: "install"; selector: InstallSelector }
  | { kind: "repair"; packIds: PackId[] }
  | { kind: "resume"; operationId: OperationId };

export type RemovalSelector =
  | { kind: "packs"; packIds: PackId[] }
  | { kind: "complete"; rootSha256: CatalogRoot }
  | { kind: "all_installed" };
export type RemovalMode = "reject_dependents" | "cascade_dependents";
export type VerificationMode = "metadata" | "full";
export type ResolutionKey =
  | { kind: "asset"; key: AssetKey }
  | { kind: "candidate"; key: CandidateKey };

export interface InstalledPack {
  packId: PackId;
  catalogRoot: CatalogRoot;
}

export interface CatalogSummary {
  catalogRoot: CatalogRoot;
  epoch: number;
  selectorCount: number;
  shardCount: number;
  installedRevision: InstalledRevision;
  installedPacks: InstalledPack[];
}

export type CatalogStatus =
  | { status: "empty" }
  | { status: "invalid" }
  | { status: "ready"; summary: CatalogSummary };

export interface InstallEstimate {
  catalogRoot: CatalogRoot;
  installedRevision: InstalledRevision;
  selector: string;
  packIds: PackId[];
  assetRecords: string;
  uniqueObjects: string;
  logicalImageBytes: string;
  uniqueImageBytes: string;
  shardCount: string;
  shardBytes: string;
}

export type StartResponse =
  | { status: "healthy" }
  | { status: "started"; operationId: OperationId; catalogRoot: CatalogRoot };

export interface OperationStatus {
  operationId: OperationId;
  catalogRoot: CatalogRoot;
  kind: "install" | "repair";
  state: "downloading" | "cancel_requested" | "finalizing" | "completed" | "cancelled";
  packTotal: number;
  packsPromoted: number;
  objectTotal: number;
  objectsPromoted: number;
  completedRevision: InstalledRevision | null;
}

export interface RemovalResponse {
  removed: InstalledPack[];
  revision: InstalledRevision;
  cleanupIssues: Array<{
    kind: "malformed_entry" | "unsafe_entry" | "remove_failed" | "catalog_state";
  }>;
}

export interface VerificationResponse {
  revision: InstalledRevision;
  issues: Array<{
    kind:
      | "missing_root_witness"
      | "invalid_root_witness"
      | "receipt_metadata"
      | "missing_shard"
      | "invalid_shard"
      | "missing_object"
      | "invalid_object_metadata"
      | "corrupt_object"
      | "dependency_drift"
      | "projection_drift";
  }>;
}

export interface ResolvedAsset {
  packId: PackId;
  assetKey: AssetKey;
  catalogRoot: CatalogRoot;
  url: string;
  media: VisualPackMedia;
}

export interface ResolutionEntry {
  ordinal: number;
  key: ResolutionKey;
  matches: ResolvedAsset[];
}

export interface ResolutionResponse {
  revision: InstalledRevision;
  entries: ResolutionEntry[];
}

export interface ProgressEvent {
  phase: "started" | "running" | "completed" | "cancelled" | "failed";
  operation: OperationStatus;
  error: VisualPackErrorKind | null;
}

export interface RevisionEvent {
  cause: "install" | "repair" | "remove";
  operationId: OperationId | null;
  catalogRoot: CatalogRoot | null;
  revision: InstalledRevision;
}

export type VisualPackErrorKind =
  | "unsupported_shell"
  | "unauthorized"
  | "unavailable"
  | "invalid_input"
  | "conflict"
  | "cancelled"
  | "network"
  | "storage"
  | "trust"
  | "emit"
  | "internal";

export interface ImageRungs {
  small?: string;
  normal?: string;
}

export type CardImageSource =
  | {
      kind: "installed";
      src: string;
      rungs?: ImageRungs;
      assetKey: AssetKey;
      packId: PackId;
      catalogRoot: CatalogRoot;
    }
  | { kind: "remote"; src: string; rungs?: ImageRungs }
  | { kind: "fallback"; src: null };
