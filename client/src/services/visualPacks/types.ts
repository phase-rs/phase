export type Brand<T, Name extends string> = T & { readonly __brand: Name };

export type AssetKey = Brand<string, "AssetKey">;
export type CandidateKey = Brand<string, "CandidateKey">;
export type CandidateKind =
  | "localized_printing" | "localized_alias"
  | "english_printing" | "english_alias"
  | "oracle" | "oracle_alias"
  | "oracle_face" | "source_printing" | "name_face"
  | "token_reference" | "token_alias"
  | "card_back" | "mana_symbol" | "set_icon";
export type PackId = Brand<string, "PackId">;
export type CatalogRoot = Brand<string, "CatalogRoot">;
export type InstalledRevision = Brand<string, "InstalledRevision">;
export type OperationId = Brand<string, "OperationId">;

const LOWER_HEX_64 = /^[0-9a-f]{64}$/;
const LOWER_HEX_32 = /^[0-9a-f]{32}$/;
const DECIMAL = /^(0|[1-9][0-9]*)$/;
const PACK_ID = /^(complete|core|curated|deck_library|printing:[a-z0-9]{3,6}|locale:(de|es|fr|it|pt):[a-z0-9]{3,6})$/;
const ASSET_KEY = /^asset:v1:(canonical_card|exact_printing|localized_printing|token|card_back|mana_symbol|set_icon):[A-Za-z0-9_-]+$/;
const CANDIDATE_KEY = /^candidate:v1:(localized_printing|localized_alias|english_printing|english_alias|oracle|oracle_alias|oracle_face|source_printing|name_face|token_reference|token_alias|card_back|mana_symbol|set_icon):([A-Za-z0-9_-]+)$/;
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
  validateCandidateVariant(variant, rung, token);
}

function validateCandidateVariant(variant: unknown, rung: unknown, token = false): void {
  if (variant === "full_card") {
    if (rung !== "small" && rung !== "normal") throw new Error("invalid visual variant/rung");
  } else if (variant === "art_crop") {
    if (rung !== "art_crop" || token) throw new Error("invalid visual variant/rung");
  } else {
    throw new Error("invalid visual variant/rung");
  }
}

function validateSemanticCandidateVisual(tuple: readonly unknown[], identityCount: number): void {
  if (tuple.length !== identityCount + 2) throw new Error("candidate tuple has wrong axis count");
  const [variant, rung] = tuple.slice(identityCount);
  validateCandidateVariant(variant, rung);
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
    case "oracle_face":
      validateCandidateIdentity(tuple[0]);
      validateCandidateText(tuple[1]);
      validateSemanticCandidateVisual(tuple, 2);
      break;
    case "source_printing":
      if (typeof tuple[0] !== "string" || !CANDIDATE_SET.test(tuple[0])) throw new Error("invalid set code");
      validateCandidateText(tuple[1]);
      validateCandidateText(tuple[2]);
      validateSemanticCandidateVisual(tuple, 3);
      break;
    case "name_face":
      validateCandidateText(tuple[0]);
      validateCandidateText(tuple[1]);
      validateSemanticCandidateVisual(tuple, 2);
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
  | { kind: "complete"; rootSha256: CatalogRoot }
  // The curated pack IS its membership: the digest names both the selection
  // and the catalog root that selection's packs and objects are stored at, so
  // a changed preference reads as an ordinary root change.
  | { kind: "curated"; membershipDigest: CatalogRoot }
  | { kind: "deck_library"; membershipDigest: CatalogRoot };

/** The curated arm of `InstallSelector`, named so a resolver can return it
 *  without widening to the union and making every caller re-narrow to read the
 *  digest it exists to carry. */
export type CuratedInstallSelector = Extract<InstallSelector, { kind: "curated" }>;

/** The deck-library arm of `InstallSelector`, named for selector resolvers. */
export type DeckLibraryInstallSelector = Extract<InstallSelector, { kind: "deck_library" }>;

export type StartRequest =
  | { kind: "install"; selector: InstallSelector; objectEstimate: number }
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
  /**
   * What the browser says about this origin's storage, read at the same instant
   * as the rest of this summary.
   *
   * Here as well as on `InstallEstimate` because the two answer different
   * questions for different people. The estimate's copy is about a download the
   * user has not committed to yet, and is reachable only by running one; this
   * copy is about the disk they are ALREADY using, which is the question a
   * panel whose subject is managing offline storage has to be able to answer
   * without first asking the user to price a new install.
   *
   * Reading it prompts nobody. `storageOutlook` calls `estimate()` and
   * `currentPersistence` calls `persisted()`; both are queries. `persist()` —
   * the one method the Storage Standard defines as REQUESTING permission — is
   * reached only from `requestPersistence`, which only `reserveStorage` calls,
   * and only when a user has started an operation.
   *
   * Every figure inside is nullable and `persistence` has an `unsupported`
   * arm, because a browser may decline to say. A display layer renders what is
   * there and omits what is not; it does not fill a null in with a zero, and it
   * does not turn these into a verdict — `InstallEstimate.headroom` is what a
   * verdict looks like, and the engine computes it.
   */
  storage: StorageOutlook;
}

export type CatalogStatus =
  | { status: "empty" }
  | { status: "invalid" }
  | { status: "ready"; summary: CatalogSummary };

/**
 * Median encoded size of ONE image at each rung of the image ladder, in bytes,
 * sampled live from Scryfall's CDN in August 2026 with **n = 6 per rung**.
 *
 * n = 6 makes these order-of-magnitude constants and nothing finer. They exist
 * so the panel can tell a ~50 MB download apart from a ~6 GB one BEFORE the
 * user commits to it. They are not a per-card measurement, and no figure
 * derived from them may be presented as one.
 *
 * Values are KiB (x1024), matching how the samples were recorded. At this
 * precision the choice of 1000 vs 1024 is well inside the sampling error and
 * is fixed here only so the arithmetic is reproducible.
 */
export const IMAGE_RUNG_MEDIAN_BYTES: Readonly<Record<VisualImageRung, number>> = {
  small: 17 * 1024,
  normal: 103 * 1024,
  art_crop: 73 * 1024,
};

const RUNG_MEDIANS = Object.values(IMAGE_RUNG_MEDIAN_BYTES);
const MEAN_IMAGE_BYTES = RUNG_MEDIANS.reduce((total, bytes) => total + bytes, 0) / RUNG_MEDIANS.length;
const CHEAPEST_IMAGE_BYTES = Math.min(...RUNG_MEDIANS);

/**
 * The expected download size, in bytes, of `imageRecords` images.
 *
 * An install is counted in image RECORDS — one per (face, rung) — and that
 * count carries no rung breakdown, so every record is weighted by the MEAN of
 * the three rung medians. That weighting is exact only when the records divide
 * evenly across the ladder, and for a pack planned from this app's own card
 * data they do, EXACTLY: MEASURED over `scryfall-data.json` (35,055 distinct
 * non-token faces) and `scryfall-printings.json` (90,831 faces), every face
 * that carries a `normal` URL also carries an `art_crop`, none carries
 * `art_crop` alone, and `small` is derived from `normal` — so each face
 * contributes either the full three-rung ladder or nothing.
 *
 * The bulk selectors take their rungs from Scryfall's own `image_uris` instead,
 * which is NOT measured here; a set of records skewed entirely onto one rung
 * would be overstated ~3.8x (all `small`) or understated ~1.6x (all `normal`).
 *
 * `core` and `printing` also contribute a card back and a set icon, which are
 * not card art at all. One and one per pack against tens of thousands of card
 * images, so they are counted at the same weight rather than modelled.
 */
export function estimatedImageBytes(imageRecords: number): number {
  return Math.round(imageRecords * MEAN_IMAGE_BYTES);
}

/**
 * The smallest `imageRecords` images could plausibly weigh: every one of them
 * at the cheapest rung on the ladder.
 *
 * This is NOT a guarantee — it is the most optimistic reading of the same six
 * samples `estimatedImageBytes` averages, and a real `small` image can be
 * under its own median. It exists so that a decision whose cost of being WRONG
 * is blocking a user has a different, deliberately generous threshold from one
 * whose cost is only showing a warning. Nothing may refuse a user on
 * `estimatedImageBytes`; a refusal needs a figure no reading of these
 * constants can get under.
 */
export function minimumImageBytes(imageRecords: number): number {
  return Math.round(imageRecords * CHEAPEST_IMAGE_BYTES);
}

/** Whether the browser will keep this origin's storage under disk pressure.
 *  `best_effort` is the default and means the whole pack may be evicted. */
export type StoragePersistence = "persisted" | "best_effort" | "unsupported";

/**
 * What the browser will say about this origin's storage.
 *
 * Every figure is nullable because `navigator.storage` is absent in some
 * browsers and private windows and may reject in others, and a download that
 * would otherwise succeed must not be blocked by not knowing.
 */
export interface StorageOutlook {
  usageBytes: number | null;
  quotaBytes: number | null;
  /** `quota - usage`, or null when either is unavailable. */
  availableBytes: number | null;
  persistence: StoragePersistence;
}

/** Whether an install fits the storage the browser reports. `unknown` means the
 *  browser would not say, NOT that it fits. */
export type StorageHeadroom = "sufficient" | "insufficient" | "unknown";

/**
 * The two figures a pre-flight storage refusal was actually decided on.
 *
 * Carried on the error rather than left for the panel to recompute. The floor
 * comes from `minimumImageBytes` and the space from a `navigator.storage`
 * reading taken at that instant; a display layer that re-derived either would
 * be deriving engine state, and would disagree with the gate the moment the
 * estimate's reading and the start-time reading differ.
 *
 * Bytes, unformatted: which unit and which locale to render them in is the one
 * part of this the display layer does know.
 */
export interface StorageRefusal {
  /** `minimumImageBytes` of the request — the cheapest reading of the model. */
  requiredBytes: number;
  /** `quota - usage` as the browser reported it when the gate ran. */
  availableBytes: number;
}

/**
 * How far the installed curated pack has drifted from the membership the
 * user's preferences and saved decks name RIGHT NOW.
 *
 * THREE categories, because two cannot express the case that matters most. A
 * regeneration of `scryfall-data.json` moves a `sourceUrl` under an unchanged
 * `assetKey`: the membership digest differs — so the pack is out of date and
 * Sync must be offered — while both asset-key sets are IDENTICAL. An
 * add/remove-only diff renders that as "0 to add, 0 to remove" beside an
 * enabled Sync button, which reads as a bug in the panel.
 *
 * An installed row whose `sourceUrl` is absent counts as `refresh`. Such a row
 * was written before the field existed and cannot say which URL its bytes came
 * from, so `installObject` will never reuse it and the sync WILL fetch it — the
 * count and the download agree, which is what makes this figure usable as the
 * pre-flight storage gate's subject.
 *
 * `add + refresh` is therefore the sync's download, up to what a row can know:
 * these are ROW facts, while reuse is decided against the CACHE, so the figure
 * overstates where another pack's row donates the same content and understates
 * where a cache entry was evicted out from under a surviving row (see
 * `curatedFetchCount`, which documents both directions and why neither is
 * guarded). `remove` is what the sync UNREFERENCES, not what it frees:
 * `completePack` deletes the ROWS, and `sweepUnreferenced` deletes a cache
 * entry only once no row in ANY pack still names that content-addressed path,
 * so bytes shared with `complete` or a `printing` pack stay on disk.
 *
 * Whatever their error bars, neither the panel nor the gate should ever use the
 * size of the whole membership for a pack that is already installed.
 */
export interface CuratedDrift {
  /** The digest of the membership planned from preferences and decks now. */
  membershipDigest: CatalogRoot;
  /** The installed curated pack's root, or null when none is installed.
   *  For curated the pack root IS its membership digest, so `installedDigest
   *  !== membershipDigest` is the whole of drift DETECTION — the counts below
   *  exist only to say how much. */
  installedDigest: CatalogRoot | null;
  /** Planned asset keys with no installed row. */
  add: number;
  /** Installed asset keys the current membership no longer names. */
  remove: number;
  /** Asset keys in both whose installed `sourceUrl` is not the planned one. */
  refresh: number;
}

/** The deck-library equivalent of `CuratedDrift`.
 *
 * The planned digest comes from the shared deck-catalog membership planner;
 * the installed digest is the deck-library pack root, if that pack exists.
 * Its row counts retain the same add/remove/refresh meaning as curated drift.
 */
export interface DeckLibraryDrift {
  membershipDigest: CatalogRoot;
  installedDigest: CatalogRoot | null;
  add: number;
  remove: number;
  refresh: number;
}

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
  /**
   * What this selection will cost to download, in bytes.
   *
   * A number rather than one of the pre-rendered strings above, because the
   * decision it supports is "50 MB or 6 GB?" and only the display layer knows
   * which unit to render that in.
   *
   * `logicalImageBytes` / `uniqueImageBytes` above stay `"unknown"` and are NOT
   * kept because they report a measurement: they have exactly one write site,
   * which hardcodes `"unknown"`, and nothing has ever populated them. Their
   * labels are wrong in two different ways — `en` alone hedges with "(known
   * after download)", a promise the code does not keep, while the other six
   * read plainly ("Logische Bildbytes", "Octets d'image logique"), naming a
   * measurement with no hedge at all. They are left untouched only so that a
   * projection is never mistaken for a measurement by overwriting a field
   * whose label claims to be one; deciding their fate (populate or delete,
   * with their seven locales) is open work.
   */
  estimatedImageBytes: number;
  storage: StorageOutlook;
  /**
   * `estimatedImageBytes` against `storage.availableBytes`, decided here
   * rather than in the UI: the frontend renders engine-computed state and does
   * not derive it.
   *
   * `insufficient` is a WARNING and not a veto. The projection behind it comes
   * from six CDN samples per rung, so it is an order-of-magnitude figure, and
   * blocking a user on it would deny an install that a ±30% error makes
   * perfectly feasible — with no override anywhere to undo that. Running out
   * of quota mid-download is the milder failure: it is classified `storage`,
   * `storage` is retryable, so the operation stays resumable, and a resume
   * skips every object already cached. The UI must therefore render this
   * beside `estimatedImageBytes` and `storage.availableBytes` and let the user
   * decide. `start()` refuses only the hopeless case — see `reserveStorage`.
   */
  headroom: StorageHeadroom;
}

export interface CatalogScanProgress {
  compressedBytesRead: number;
  compressedBytesTotal: number;
  recordsScanned: number;
  assetRecords: number;
}

export type StartResponse =
  | { status: "healthy" }
  /** `persistence` is the outcome of the grant requested as part of starting.
   *  Required rather than optional so a second `started` site cannot omit it
   *  and silently report a pack as evictable when it is not, or the reverse. */
  | { status: "started"; operationId: OperationId; catalogRoot: CatalogRoot; persistence: StoragePersistence };

export interface OperationStatus {
  operationId: OperationId;
  catalogRoot: CatalogRoot;
  kind: "install" | "repair";
  state: "downloading" | "cancel_requested" | "finalizing" | "completed" | "cancelled";
  packTotal: number;
  packsPromoted: number;
  objectTotal: number;
  objectEstimate: number | null;
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
  /**
   * The unclassified bucket. `errorKind` ends in `return "storage"`, so this is
   * what an error nothing recognised is reported as, alongside a genuine
   * `QuotaExceededError` from a write that was attempted and failed.
   */
  | "storage"
  /**
   * A pre-flight refusal: nothing was attempted and nothing was written,
   * because the space the browser reports cannot hold even the cheapest
   * reading of the download. Distinct from `storage` because the sentence a
   * user needs is a different one — and because sharing a kind with the
   * catch-all would make that sentence fire for unrecognised errors too.
   * Carries a `StorageRefusal` (see `VisualPackStorageRefusalError`).
   */
  | "insufficient_storage"
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
