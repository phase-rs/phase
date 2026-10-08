import type {
  CatalogStatus,
  CatalogSummary,
  CatalogScanProgress,
  CuratedDrift,
  CuratedInstallSelector,
  DeckLibraryDrift,
  DeckLibraryInstallSelector,
  InstallEstimate,
  InstallSelector,
  OperationId,
  OperationStatus,
  ProgressEvent,
  RemovalMode,
  RemovalResponse,
  RemovalSelector,
  ResolutionKey,
  ResolutionResponse,
  RevisionEvent,
  StartRequest,
  StartResponse,
  StorageRefusal,
  VerificationMode,
  VerificationResponse,
  VisualPackErrorKind,
} from "./types.ts";

export class VisualPackBackendError extends Error {
  /**
   * The diagnostic text a caller supplied, or null when it supplied none.
   *
   * Kept apart from `message` because the two answer different questions.
   * `message` must always say something in a stack trace, so it falls back to
   * a developer-facing default; the panel renders `detail` verbatim beneath a
   * translated sentence, and that default is untranslated English prose which
   * would then appear under it in all seven languages. A `kind` on its own is
   * already a complete, translatable message — a null here says exactly that
   * and is not an absence of information.
   */
  readonly detail: string | null;

  constructor(public readonly kind: VisualPackErrorKind, detail?: string) {
    super(detail || `visual-pack backend ${kind}`);
    this.name = "VisualPackBackendError";
    this.detail = detail ?? null;
  }
}

/**
 * A pre-flight refusal, with the figures it was decided on.
 *
 * The payload rides ALONGSIDE the message rather than inside it. `detail` IS
 * `message` — the base constructor passes it straight to `super` — so anything
 * written there is prose, and prose composed in this layer is prose the panel
 * cannot translate. The message here is left to the base default so it stays a
 * developer-facing string in a stack trace, and the two numbers a user is owed
 * are typed fields instead.
 */
export class VisualPackStorageRefusalError extends VisualPackBackendError {
  constructor(public readonly refusal: StorageRefusal) {
    super("insufficient_storage");
    this.name = "VisualPackStorageRefusalError";
  }
}

export interface VisualPackBackend {
  catalogStatus(): Promise<CatalogStatus>;
  /**
   * The curated selector for the preferences stored right now.
   *
   * Every other selector is composed from what the user typed into the panel,
   * so the panel can build it. This one is a membership digest: computing it
   * means planning the whole membership from the card data and the art
   * preferences, which is engine work, and a display layer that did it would
   * be deriving state rather than rendering it. Worse, it would be a SECOND
   * assembly of the planner's input beside the one `start()` and `run()` use,
   * and the two would disagree the moment either gained a source the other
   * lacked — the panel would then show a digest the backend never installs.
   */
  curatedSelector(): Promise<CuratedInstallSelector>;
  /**
   * The planned curated membership against the one on disk.
   *
   * A superset of `curatedSelector()` in what it RETURNS — `membershipDigest`
   * is on both — but not in what it costs: this one also reads every installed
   * curated `objects` row to say how far the two have diverged. The two stay
   * separate because their callers differ: a first install needs only the
   * digest, and paying for a diff against a pack that is not installed would
   * be work with no answer to give.
   *
   * `null` is UNMEASURED, and it is a normal answer rather than a failure: the
   * membership behind a measurement is planned from card data that may not be
   * loaded, and loading it is tens of megabytes. A caller renders nothing for a
   * null — it must never be read as "no drift", which is a measurement this did
   * not make. `curatedSelector()` carries no such arm: it is only ever called
   * because a user chose the curated option, so paying to load is what they
   * asked for.
   */
  curatedDrift(): Promise<CuratedDrift | null>;
  /** The current deck-library membership selector. This is intentionally not
   * part of `InstallSelector` until its lifecycle exists. */
  deckLibrarySelector(): Promise<DeckLibraryInstallSelector>;
  /**
   * The planned deck-library membership against its installed rows, if card
   * data is resident. `null` means unmeasured and must not trigger a load.
   */
  deckLibraryDrift(): Promise<DeckLibraryDrift | null>;
  /**
   * Reconcile the already-installed deck-library membership without creating
   * an opt-in or requesting persistent storage. Background callers use this
   * after a deck or art-preference input changes.
   */
  reconcileDeckLibrary(): Promise<void>;
  refreshCatalog(): Promise<CatalogSummary>;
  catalogSummary(): Promise<CatalogSummary>;
  estimateInstall(selector: InstallSelector, onProgress?: (progress: CatalogScanProgress) => void): Promise<InstallEstimate>;
  start(request: StartRequest): Promise<StartResponse>;
  cancel(operationId: OperationId): Promise<OperationStatus>;
  operationStatus(operationId: OperationId): Promise<OperationStatus>;
  remove(selector: RemovalSelector, mode: RemovalMode): Promise<RemovalResponse>;
  verify(mode: VerificationMode): Promise<VerificationResponse>;
  resolve(keys: ResolutionKey[]): Promise<ResolutionResponse>;
  subscribeProgress(listener: (event: ProgressEvent) => void): Promise<() => void>;
  subscribeRevision(listener: (event: RevisionEvent) => void): Promise<() => void>;
}

/**
 * Optional lifecycle control for automatic deck-library reconciliation.
 *
 * Manual/resolution-only backends intentionally need not implement this: the
 * scheduler must have this capability before it can dispatch background work.
 */
export interface DeckLibraryBackgroundLifecycle {
  setDeckLibraryBackgroundPaused(paused: boolean): Promise<void>;
  prepareDeckLibraryForOffline(): Promise<DeckLibraryPreparationResult>;
}

/** The installed Deck Catalog's state after an awaited background reconciliation. */
export type DeckLibraryPreparationResult = "not-installed" | "ready";

export function isDeckLibraryBackgroundLifecycle(
  backend: VisualPackBackend,
): backend is VisualPackBackend & DeckLibraryBackgroundLifecycle {
  return "setDeckLibraryBackgroundPaused" in backend
    && typeof backend.setDeckLibraryBackgroundPaused === "function"
    && "prepareDeckLibraryForOffline" in backend
    && typeof backend.prepareDeckLibraryForOffline === "function";
}
