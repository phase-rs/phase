import { useCallback, useEffect, useRef, useState } from "react";

import { loadVisualPackBackend } from "../../../services/platform.ts";
import {
  VisualPackBackendError,
  VisualPackStorageRefusalError,
  type VisualPackBackend,
} from "../../../services/visualPacks/backend.ts";
import {
  compareRevisions,
  packId,
  type CatalogSummary,
  type CatalogScanProgress,
  type CuratedDrift,
  type CuratedInstallSelector,
  type DeckLibraryDrift,
  type DeckLibraryInstallSelector,
  type InstallEstimate,
  type InstallSelector,
  type OperationStatus,
  type PackId,
  type ProgressEvent,
  type RemovalMode,
  type RemovalResponse,
  type RemovalSelector,
  type RevisionEvent,
  type StorageRefusal,
  type VerificationMode,
  type VerificationResponse,
  type VisualPackErrorKind,
} from "../../../services/visualPacks/types.ts";
import { getEffectiveOffline } from "../../../stores/connectivityStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";

export type ManagerAvailability =
  | { kind: "loading" }
  | { kind: "browser_unavailable" }
  | { kind: "unsupported_shell" }
  | { kind: "transient_failure"; error: VisualPackErrorKind }
  | { kind: "empty" }
  | { kind: "invalid" }
  | { kind: "ready" };

export type FrozenConfirmation =
  | { kind: "cascade"; selector: RemovalSelector }
  | { kind: "complete"; selector: RemovalSelector }
  | { kind: "all"; selector: RemovalSelector };

interface BoundEstimate {
  selector: InstallSelector;
  value: InstallEstimate;
}

interface BoundVerification {
  catalogRoot: CatalogSummary["catalogRoot"];
  installedRevision: CatalogSummary["installedRevision"];
  value: VerificationResponse;
}

interface ProgressOutcome {
  identity: string | null;
  failed: boolean;
  /**
   * A cancellation this panel WITNESSED — a `cancelled` progress event, or its
   * own `cancel()` call returning a cancelled status.
   *
   * Deliberately not the same fact as a `cancelled` operation RECORD, which is
   * what `terminal` below is set from. `run()` writes `state: "cancelled"` for
   * a non-retryable failure as well as for a user cancel, so a record read back
   * through `operationStatus()` says the operation ended and nothing about why.
   * This one is only ever set where the ending is known to be the user's.
   */
  cancelled: boolean;
  terminal: boolean;
}

export interface VisualPackManagerState {
  availability: ManagerAvailability;
  summary: CatalogSummary | null;
  /**
   * The curated selector as the backend resolved it, or null until the user
   * picks that option.
   *
   * Resolved on demand rather than alongside the summary because it costs a
   * membership plan, and most sessions never open this panel to install the
   * curated pack.
   */
  curatedSelector: CuratedInstallSelector | null;
  /** The deck-library selector as the backend resolved it after the user chose
   * that option. It is intentionally not planned on panel mount. */
  deckLibrarySelector: DeckLibraryInstallSelector | null;
  /**
   * How far the installed curated pack has drifted from what the stored
   * preferences name now, or null when there is nothing to say.
   *
   * Null covers three different situations and the panel must treat all three
   * the same way — say nothing: no curated pack is installed, the read has not
   * finished, or it failed. A drift claim the panel has not measured is a
   * claim about a multi-gigabyte download, so it is never guessed.
   */
  curatedDrift: CuratedDrift | null;
  /** The backend-owned deck-library membership delta, or null when unknown. */
  deckLibraryDrift: DeckLibraryDrift | null;
  estimate: BoundEstimate | null;
  estimateProgress: CatalogScanProgress | null;
  operation: OperationStatus | null;
  progress: ProgressEvent | null;
  verification: BoundVerification | null;
  removal: RemovalResponse | null;
  actionError: VisualPackErrorKind | null;
  /** Verbatim diagnostic text from the platform, for the `<code>` line. Null
   *  when the failure carries structured state instead — see `errorDetail`. */
  actionErrorDetail: string | null;
  /** The figures behind an `insufficient_storage` refusal, for the panel to
   *  format. Null for every other kind. */
  actionErrorRefusal: StorageRefusal | null;
  pendingActions: ReadonlySet<string>;
  durableMutationActive: boolean;
  confirmation: FrozenConfirmation | null;
  retry(): void;
  refresh(): void;
  resolveCuratedSelector(): void;
  resolveDeckLibrarySelector(): void;
  estimateInstall(selector: InstallSelector): void;
  install(selector: InstallSelector): void;
  cancel(): void;
  resume(): void;
  verify(mode: VerificationMode): void;
  repair(packIds: PackId[]): void;
  removeSelected(packIds: PackId[]): void;
  removeComplete(): void;
  removeAll(): void;
  confirmRemoval(): void;
  dismissConfirmation(): void;
}

const MUTATION_ACTIONS: ReadonlySet<string> = new Set(["install", "cancel", "resume", "repair", "remove"]);
const MAX_BUFFERED_START_OPERATIONS = 8;
const CURATED = packId("curated");
const DECK_LIBRARY = packId("deck_library");

export function hasPendingVisualPackMutation(pending: ReadonlySet<string>): boolean {
  return [...pending].some((entry) => MUTATION_ACTIONS.has(entry));
}

/**
 * What the panel may say about the installed curated pack.
 *
 * `unknown` is "say nothing", and it covers three states the display must treat
 * identically: no curated pack is installed, the drift has not been measured,
 * or measuring it failed. It is NEVER "no drift" — that is a measurement, and
 * this is its absence.
 */
export type LocalMembershipDriftState = "unknown" | "current" | "drifted";

/**
 * The single authority on what curated drift means, for every surface.
 *
 * Three sites spelled this comparison out independently and had already
 * diverged: the badge read `installedDigest !== membershipDigest`, which is
 * TRUE for `installedDigest: null` — the backend's way of saying nothing is
 * installed — while the selector read that same null as "nothing to report".
 * `InstallEstimate.headroom` lives on the engine so that a verdict has one
 * definition; this is the same discipline one layer down, and three hand-written
 * copies are what produced the disagreement.
 *
 * Compared against the digest the SUMMARY reports installed, not against
 * `drift.installedDigest`. For a curated pack the installed pack's
 * `catalogRoot` IS its membership digest, and the summary is the authority on
 * what is on disk right now, while a drift is a measurement that may have been
 * taken before an install or a removal moved it.
 */
export function localMembershipDriftState(
  summary: CatalogSummary,
  pack: PackId,
  drift: CuratedDrift | DeckLibraryDrift | null,
): LocalMembershipDriftState {
  const installed = summary.installedPacks.find((entry) => entry.packId === pack);
  if (!drift || !installed) return "unknown";
  return installed.catalogRoot === drift.membershipDigest ? "current" : "drifted";
}

function errorKind(error: unknown): VisualPackErrorKind {
  return error instanceof VisualPackBackendError ? error.kind : "internal";
}

/**
 * The verbatim line shown beneath the translated sentence.
 *
 * A `VisualPackBackendError` is asked for its `detail` rather than its
 * `message`, because those answer different questions: `message` falls back to
 * a developer-facing default so a stack trace is never blank, and rendering
 * that default would put untranslated English underneath a translated sentence
 * in a seven-language panel. Every kind already HAS a translated sentence, so
 * an error that supplied no detail has nothing further to say.
 *
 * That covers a storage refusal, whose figures ride on `refusal` instead, and
 * the curated planner's `network` — but as a rule rather than as a list, so a
 * kind added later cannot leak English by forgetting to join it.
 */
function errorDetail(error: unknown): string | null {
  if (error instanceof VisualPackBackendError) return error.detail;
  if (error instanceof Error) return error.message || null;
  return typeof error === "string" && error ? error : null;
}

function errorRefusal(error: unknown): StorageRefusal | null {
  return error instanceof VisualPackStorageRefusalError ? error.refusal : null;
}

function selectorIdentity(selector: InstallSelector): string {
  switch (selector.kind) {
    case "core":
      return "core";
    case "printing":
      return `printing:${selector.set}`;
    case "locale":
      return `locale:${selector.language}:${selector.set}`;
    case "complete":
      return `complete:${selector.rootSha256}`;
    case "curated":
      return `curated:${selector.membershipDigest}`;
    case "deck_library":
      return `deck_library:${selector.membershipDigest}`;
  }
}

/**
 * The name the backend stamps on `InstallEstimate.selector` — its pack id.
 *
 * For most selectors that is the identity string, but the two root-bearing
 * kinds carry a root the pack id does not, so their identity would never match
 * and their estimate would be discarded as stale rather than rendered.
 */
function signedSelectorName(selector: InstallSelector): string {
  return selector.kind === "complete" || selector.kind === "curated" || selector.kind === "deck_library"
    ? selector.kind
    : selectorIdentity(selector);
}

function operationIsTerminal(status: OperationStatus): boolean {
  return status.state === "completed" || status.state === "cancelled";
}

function progressIdentity(event: ProgressEvent): string {
  return `${event.operation.operationId}:${event.operation.catalogRoot}`;
}

function progressIsTerminal(event: ProgressEvent): boolean {
  return event.phase === "completed" || event.phase === "cancelled" || operationIsTerminal(event.operation);
}

function operationIsDurableMutation(status: OperationStatus | null): boolean {
  return status?.state === "downloading"
    || status?.state === "cancel_requested"
    || status?.state === "finalizing";
}

function operationRank(status: OperationStatus): number {
  switch (status.state) {
    case "downloading": return 0;
    case "cancel_requested": return 1;
    case "finalizing": return 2;
    case "completed":
    case "cancelled": return 3;
  }
}

export function useVisualPackManager(): VisualPackManagerState {
  const mountedRef = useRef(false);
  const backendRef = useRef<VisualPackBackend | null>(null);
  const backendLoadRef = useRef<Promise<VisualPackBackend | null> | null>(null);
  const summaryRef = useRef<CatalogSummary | null>(null);
  const operationRef = useRef<OperationStatus | null>(null);
  const progressRef = useRef<ProgressEvent | null>(null);
  const progressOutcomeRef = useRef<ProgressOutcome>({ identity: null, failed: false, cancelled: false, terminal: false });
  const startEventBufferRef = useRef({ active: false, events: new Map<string, ProgressEvent>() });
  const pendingRef = useRef(new Set<string>());
  const listenersRef = useRef<Array<() => void>>([]);
  const initializedRef = useRef(false);
  const requestRef = useRef({ initialize: 0, summary: 0, estimate: 0, verify: 0, curated: 0, deckLibrary: 0, curatedDrift: 0, deckLibraryDrift: 0 });

  const [availability, setAvailability] = useState<ManagerAvailability>({ kind: "loading" });
  const [summary, setSummary] = useState<CatalogSummary | null>(null);
  const [curatedSelector, setCuratedSelector] = useState<CuratedInstallSelector | null>(null);
  const [deckLibrarySelector, setDeckLibrarySelector] = useState<DeckLibraryInstallSelector | null>(null);
  const [curatedDrift, setCuratedDrift] = useState<CuratedDrift | null>(null);
  const [deckLibraryDrift, setDeckLibraryDrift] = useState<DeckLibraryDrift | null>(null);
  const [staleCuratedSelectorRetry, setStaleCuratedSelectorRetry] = useState(0);
  const [staleDeckLibrarySelectorRetry, setStaleDeckLibrarySelectorRetry] = useState(0);
  const [estimate, setEstimate] = useState<BoundEstimate | null>(null);
  const [estimateProgress, setEstimateProgress] = useState<CatalogScanProgress | null>(null);
  const [operation, setOperation] = useState<OperationStatus | null>(null);
  const [progress, setProgress] = useState<ProgressEvent | null>(null);
  const [verification, setVerification] = useState<BoundVerification | null>(null);
  const [removal, setRemoval] = useState<RemovalResponse | null>(null);
  const [actionError, setActionError] = useState<VisualPackErrorKind | null>(null);
  const [actionErrorDetail, setActionErrorDetail] = useState<string | null>(null);
  const [actionErrorRefusal, setActionErrorRefusal] = useState<StorageRefusal | null>(null);
  const [pendingActions, setPendingActions] = useState<ReadonlySet<string>>(new Set());
  const [confirmation, setConfirmation] = useState<FrozenConfirmation | null>(null);

  const clearActionError = useCallback(() => {
    setActionError(null);
    setActionErrorDetail(null);
    setActionErrorRefusal(null);
  }, []);
  const reportActionError = useCallback((error: unknown) => {
    setActionError(errorKind(error));
    setActionErrorDetail(errorDetail(error));
    setActionErrorRefusal(errorRefusal(error));
  }, []);

  const beginPending = useCallback((value: string): boolean => {
    if (pendingRef.current.has(value)) return false;
    if (MUTATION_ACTIONS.has(value) && hasPendingVisualPackMutation(pendingRef.current)) return false;
    pendingRef.current.add(value);
    setPendingActions(new Set(pendingRef.current));
    return true;
  }, []);
  // The rendered disabled state is only advisory: a click queued before an
  // offline render commits must not begin a backend operation or publish a
  // pending state. Network-starting paths share this imperative boundary.
  const beginOnlinePending = useCallback((value: string): boolean => {
    if (getEffectiveOffline()) return false;
    return beginPending(value);
  }, [beginPending]);
  const endPending = useCallback((value: string) => {
    pendingRef.current.delete(value);
    setPendingActions(new Set(pendingRef.current));
  }, []);

  const acceptSummary = useCallback((next: CatalogSummary): boolean => {
    const current = summaryRef.current;
    if (current && compareRevisions(next.installedRevision, current.installedRevision) < 0) return false;
    const changed = !current
      || current.catalogRoot !== next.catalogRoot
      || current.installedRevision !== next.installedRevision;
    summaryRef.current = next;
    setSummary(next);
    setAvailability({ kind: "ready" });
    if (changed) {
      requestRef.current.verify += 1;
      requestRef.current.estimate += 1;
      requestRef.current.curated += 1;
      requestRef.current.deckLibrary += 1;
      setCuratedSelector(null);
      setDeckLibrarySelector(null);
      setEstimate(null);
      setEstimateProgress(null);
      setVerification(null);
    }
    return true;
  }, []);

  const refreshSummary = useCallback(async (minimumRevision?: RevisionEvent["revision"]) => {
    const backend = backendRef.current;
    if (!backend) return;
    const generation = ++requestRef.current.summary;
    try {
      const next = await backend.catalogSummary();
      if (!mountedRef.current || generation !== requestRef.current.summary) return;
      if (minimumRevision && compareRevisions(next.installedRevision, minimumRevision) < 0) return;
      acceptSummary(next);
    } catch (error) {
      if (mountedRef.current && generation === requestRef.current.summary) {
        reportActionError(error);
      }
    }
  }, [acceptSummary, reportActionError]);

  const handleProgress = useCallback((event: ProgressEvent) => {
    if (!mountedRef.current) return;
    let selected = operationRef.current;
    if (
      operationIsDurableMutation(event.operation)
      && !startEventBufferRef.current.active
      && (!selected || ((progressOutcomeRef.current.failed || !operationIsDurableMutation(selected)) && progressIdentity(event) !== `${selected.operationId}:${selected.catalogRoot}`))
    ) {
      const identity = progressIdentity(event);
      operationRef.current = event.operation;
      progressRef.current = null;
      progressOutcomeRef.current = { identity, failed: false, cancelled: false, terminal: false };
      setOperation(event.operation);
      setProgress(null);
      selected = event.operation;
    }
    if (
      !selected
      || event.operation.operationId !== selected.operationId
      || event.operation.catalogRoot !== selected.catalogRoot
    ) {
      const buffer = startEventBufferRef.current;
      if (buffer.active) {
        const identity = progressIdentity(event);
        const buffered = buffer.events.get(identity);
        if (
          buffered
          && (
            buffered.phase === "failed"
            || progressIsTerminal(buffered)
            || operationRank(event.operation) < operationRank(buffered.operation)
          )
        ) return;
        if (!buffer.events.has(identity) && buffer.events.size >= MAX_BUFFERED_START_OPERATIONS) {
          const oldest = buffer.events.keys().next().value;
          if (oldest) buffer.events.delete(oldest);
        }
        buffer.events.set(identity, event);
      }
      return;
    }
    const identity = progressIdentity(event);
    const outcome = progressOutcomeRef.current;
    if (outcome.identity !== identity) {
      outcome.identity = identity;
      outcome.failed = false;
      outcome.cancelled = false;
      outcome.terminal = false;
    }
    // An ending this panel WITNESSED is final. A merely terminal status is not,
    // and the difference decides whether a stopped operation reads as the
    // user's cancel or as the failure it was: `run()` writes
    // `state: "cancelled"` for a non-retryable failure too, so a record read
    // back through `operationStatus()` cannot say which happened. The `failed`
    // event is the sole carrier of WHY, and on the resume path it arrives AFTER
    // `trackStarted` has read the terminated record and set `terminal` —
    // dropping it there rendered a conflict as a deliberate cancel, with no
    // diagnostic anywhere on screen.
    //
    // `completed` is excluded from the reopening because that ending is
    // unambiguous: `run()`'s catch leaves an already-completed record alone, so
    // it can emit `failed` carrying one, and a finished install must not flip.
    // A retryable failure leaves its record live. Its next authoritative
    // `started` event is a new run of THAT record, so it may reopen the
    // failure latch. Do this only after proving the event still names a live
    // operation: a late start must never revive a witnessed cancellation or a
    // completed/terminal record.
    const restartingAfterRetryableFailure = event.phase === "started"
      && outcome.failed
      && !outcome.cancelled
      && !outcome.terminal
      && operationIsDurableMutation(event.operation);
    const acceptingCancellationAfterRetryableFailure = event.phase === "cancelled"
      && outcome.failed
      && !outcome.terminal
      && operationIsDurableMutation(selected);
    if (outcome.cancelled) return;
    // Reconciliation can authoritatively cancel a previously failed but still
    // retryable operation when its membership is superseded. That terminal
    // event must replace the Resume state; ordinary running updates may not.
    if (outcome.failed && !restartingAfterRetryableFailure && !acceptingCancellationAfterRetryableFailure) return;
    if (outcome.terminal && (event.phase !== "failed" || event.operation.state === "completed")) return;
    if (operationRank(event.operation) < operationRank(selected)) return;
    if (restartingAfterRetryableFailure) {
      outcome.failed = false;
      outcome.terminal = false;
    }
    if (event.phase === "failed") outcome.failed = true;
    if (event.phase === "cancelled") outcome.cancelled = true;
    if (progressIsTerminal(event)) outcome.terminal = true;
    operationRef.current = event.operation;
    progressRef.current = event;
    setOperation(event.operation);
    setProgress(event);
    if (event.phase === "failed") {
      if (event.error) {
        setActionError(event.error);
        // All three move together or the kind and the payload describe
        // different failures: `actionErrorRefusal`'s contract is "null for
        // every other kind", and a progress event carries a kind with no error
        // object to source a payload from.
        setActionErrorDetail(null);
        setActionErrorRefusal(null);
      }
    } else {
      clearActionError();
    }
    if (operationIsTerminal(event.operation)) void refreshSummary(event.operation.completedRevision ?? undefined);
  }, [clearActionError, refreshSummary]);

  const beginStartEventBuffer = useCallback(() => {
    const buffer = startEventBufferRef.current;
    buffer.events.clear();
    buffer.active = true;
  }, []);

  const clearStartEventBuffer = useCallback(() => {
    const buffer = startEventBufferRef.current;
    buffer.active = false;
    buffer.events.clear();
  }, []);

  const adoptStartedOperation = useCallback((
    operationId: OperationStatus["operationId"],
    catalogRoot: OperationStatus["catalogRoot"],
    kind: OperationStatus["kind"],
  ) => {
    const pending: OperationStatus = {
      operationId,
      catalogRoot,
      kind,
      state: "downloading",
      packTotal: 0,
      packsPromoted: 0,
      objectTotal: 0,
      objectEstimate: null,
      objectsPromoted: 0,
      completedRevision: null,
    };
    const buffer = startEventBufferRef.current;
    const identity = `${operationId}:${catalogRoot}`;
    const buffered = buffer.events.get(identity) ?? null;
    buffer.active = false;
    buffer.events.clear();
    operationRef.current = pending;
    progressRef.current = null;
    progressOutcomeRef.current = { identity, failed: false, cancelled: false, terminal: false };
    setOperation(pending);
    setProgress(null);
    if (buffered) handleProgress(buffered);
  }, [handleProgress]);

  const handleRevision = useCallback((event: RevisionEvent) => {
    const current = summaryRef.current;
    if (!mountedRef.current || (current && compareRevisions(event.revision, current.installedRevision) <= 0)) return;
    void refreshSummary(event.revision);
  }, [refreshSummary]);

  const initialize = useCallback(async () => {
    const generation = ++requestRef.current.initialize;
    setAvailability({ kind: "loading" });
    clearActionError();
    try {
      const load = backendLoadRef.current ??= loadVisualPackBackend();
      let backend: VisualPackBackend | null;
      try {
        backend = await load;
      } catch (error) {
        if (backendLoadRef.current === load) backendLoadRef.current = null;
        throw error;
      }
      if (!mountedRef.current || generation !== requestRef.current.initialize) return;
      if (!backend) {
        setAvailability({ kind: "browser_unavailable" });
        return;
      }
      backendRef.current = backend;
      const status = await backend.catalogStatus();
      if (!mountedRef.current || generation !== requestRef.current.initialize) return;

      let progressUnlisten: (() => void) | null = null;
      let revisionUnlisten: (() => void) | null = null;
      try {
        progressUnlisten = await backend.subscribeProgress(handleProgress);
        if (!mountedRef.current || generation !== requestRef.current.initialize) {
          progressUnlisten();
          return;
        }
        revisionUnlisten = await backend.subscribeRevision(handleRevision);
        if (!mountedRef.current || generation !== requestRef.current.initialize) {
          progressUnlisten();
          revisionUnlisten();
          return;
        }
      } catch (error) {
        progressUnlisten?.();
        revisionUnlisten?.();
        throw error;
      }
      listenersRef.current.push(progressUnlisten, revisionUnlisten);
      initializedRef.current = true;
      switch (status.status) {
        case "ready":
          acceptSummary(status.summary);
          break;
        case "empty":
          setAvailability({ kind: "empty" });
          break;
        case "invalid":
          setAvailability({ kind: "invalid" });
          break;
      }
    } catch (error) {
      if (!mountedRef.current || generation !== requestRef.current.initialize) return;
      const kind = errorKind(error);
      setAvailability(kind === "unsupported_shell" ? { kind } : { kind: "transient_failure", error: kind });
    }
  }, [acceptSummary, clearActionError, handleProgress, handleRevision]);

  useEffect(() => {
    const requests = requestRef.current;
    const listeners = listenersRef.current;
    mountedRef.current = true;
    void initialize();
    return () => {
      mountedRef.current = false;
      requests.initialize += 1;
      requests.summary += 1;
      requests.estimate += 1;
      requests.verify += 1;
      requests.curated += 1;
      requests.deckLibrary += 1;
      requests.curatedDrift += 1;
      requests.deckLibraryDrift += 1;
      for (const unlisten of listeners.splice(0)) unlisten();
    };
  }, [initialize]);

  const retry = useCallback(() => {
    if (!initializedRef.current) void initialize();
  }, [initialize]);

  // Two of the three values `planCuratedPack()` memoizes on, read as VALUES
  // rather than through `getState()` so a change re-runs the effect below.
  //
  // NOT for an art rule edited on the Visual tab: that tab and this one are
  // mutually exclusive branches of the same modal, so editing a rule there has
  // already unmounted this panel. What earns the dependency is
  // `ResetAllFooter` -> `resetAllPreferences`, which renders on EVERY tab and
  // replaces both of these references while this panel is mounted — without the
  // dependency the panel would go on reporting drift against art rules the user
  // has just cleared.
  const artChain = usePreferencesStore((state) => state.artChain);
  const artOverrides = usePreferencesStore((state) => state.artOverrides);
  const curatedInstalled = summary?.installedPacks.some((entry) => entry.packId === CURATED) ?? false;
  const deckLibraryInstalled = summary?.installedPacks.some((entry) => entry.packId === DECK_LIBRARY) ?? false;
  const installedRevisionValue = summary?.installedRevision;

  // The two local memberships both depend on art selection. A changed rule
  // invalidates selectors and estimates even when the resulting digest happens
  // to be the same: the backend, not this display layer, owns whether its
  // planner has freshened the underlying descriptor set. If a prior resolver
  // is still holding its pending slot, its eventual result is generation-bound
  // below; changing each pack's retry token lets its selected radio ask exactly
  // once more after that pack's slot releases.
  useEffect(() => {
    requestRef.current.curated += 1;
    requestRef.current.deckLibrary += 1;
    requestRef.current.estimate += 1;
    setCuratedSelector(null);
    setDeckLibrarySelector(null);
    setEstimate(null);
    setEstimateProgress(null);
    setStaleCuratedSelectorRetry((value) => value + 1);
    setStaleDeckLibrarySelectorRetry((value) => value + 1);
  }, [artChain, artOverrides]);

  /**
   * Recompute curated drift whenever the membership it compares against moves.
   *
   * Gated on a curated pack BEING INSTALLED, and that gate is what makes it
   * affordable: `curatedDrift()` plans the whole membership from the card data
   * and then reads every installed curated row, and a session that never
   * installed one is buying an answer it has no question for. A user who HAS
   * installed one has already committed to a multi-gigabyte download, so
   * telling them it has gone stale is worth a plan they effectively already
   * paid for. `planCuratedPack()` memoizes on THREE values — these two plus a
   * serialization of every saved deck's stored text — so a hit is cheap but not
   * free: the third component is a `localStorage` read and a `JSON.stringify`
   * on every call, hit or miss.
   *
   * It RECOMPUTES; it never downloads. A preference toggle producing a delta is
   * the whole point, and the delta is what the user then presses Sync on.
   *
   * THIS READ NEVER FETCHES. `curatedDrift()` answers `null` rather than loading
   * the card data behind a membership plan — 76 MB across two files — and a
   * null renders as "say nothing".
   *
   * Scoped to this read, and not a claim about mounting: mounting DOES reach
   * `loadVisualPackBackend()` -> `create()`, whose pending loop calls `run()`
   * for every `downloading` or `finalizing` record, which resumes an interrupted
   * download and plans a membership on the way. That is a user-initiated
   * operation continuing, which is correct, and it is why the sentence has to
   * be about the effect rather than about the mount.
   * `curatedSelector` is a dependency for that reason: resolving a selector is
   * a user-initiated action that goes through the same planner and therefore
   * loads that data, so its arrival is the moment a previously unmeasurable
   * drift becomes free to measure. The panel asks again; the backend, which
   * owns the question, decides again.
   *
   * A failure leaves the drift null and reports NOTHING. Nobody asked for this
   * read, and an alert beside the install controls would be attributed to
   * whatever the user did last; the same failure is surfaced WITH an error on
   * the path a user does ask for, `resolveCuratedSelector`.
   *
   * The saved decks are the third input `planCuratedPack()` keys on and are not
   * watched, and that costs nothing reachable: they are edited in the deck
   * builder, which is not interactive behind this modal, so a deck cannot
   * change while this panel is mounted. `savedDeckText()` reads `localStorage`
   * directly and there is no deck store in `src/stores/` to subscribe to
   * anyway. Were one to change, the digest the backend installs would still be
   * right — `start()` replans — so the worst case is a stale display, not a
   * divergence.
   */
  useEffect(() => {
    const backend = backendRef.current;
    if (!backend || !curatedInstalled) {
      setCuratedDrift(null);
      return;
    }
    const generation = ++requestRef.current.curatedDrift;
    void backend.curatedDrift().then(
      (drift) => {
        if (mountedRef.current && generation === requestRef.current.curatedDrift) setCuratedDrift(drift);
      },
      () => {
        if (mountedRef.current && generation === requestRef.current.curatedDrift) setCuratedDrift(null);
      },
    );
  }, [artChain, artOverrides, curatedInstalled, curatedSelector, installedRevisionValue]);

  useEffect(() => {
    const backend = backendRef.current;
    if (!backend || !deckLibraryInstalled) {
      setDeckLibraryDrift(null);
      return;
    }
    const generation = ++requestRef.current.deckLibraryDrift;
    void backend.deckLibraryDrift().then(
      (drift) => {
        if (mountedRef.current && generation === requestRef.current.deckLibraryDrift) setDeckLibraryDrift(drift);
      },
      () => {
        if (mountedRef.current && generation === requestRef.current.deckLibraryDrift) setDeckLibraryDrift(null);
      },
    );
  }, [artChain, artOverrides, deckLibraryInstalled, deckLibrarySelector, installedRevisionValue]);

  const refresh = useCallback(async () => {
    const backend = backendRef.current;
    if (!backend || !beginOnlinePending("refresh")) return;
    clearActionError();
    const generation = ++requestRef.current.summary;
    try {
      const next = await backend.refreshCatalog();
      if (mountedRef.current && generation === requestRef.current.summary) acceptSummary(next);
    } catch (error) {
      if (mountedRef.current && generation === requestRef.current.summary) reportActionError(error);
    } finally {
      if (mountedRef.current) endPending("refresh");
    }
  }, [acceptSummary, beginOnlinePending, clearActionError, endPending, reportActionError]);

  /**
   * Ask the backend what "curated" currently means.
   *
   * Its own pending key, deliberately not one of `MUTATION_ACTIONS`: resolving
   * a digest reads preferences and card data and writes nothing, so it must
   * not disable Install or Remove while it runs.
   */
  const resolveCuratedSelector = useCallback(async () => {
    const backend = backendRef.current;
    if (!backend || !beginOnlinePending("curated")) return;
    clearActionError();
    const generation = ++requestRef.current.curated;
    try {
      const selector = await backend.curatedSelector();
      if (mountedRef.current && generation === requestRef.current.curated) setCuratedSelector(selector);
    } catch (error) {
      if (mountedRef.current && generation === requestRef.current.curated) reportActionError(error);
    } finally {
      if (mountedRef.current && generation !== requestRef.current.curated) setStaleCuratedSelectorRetry((value) => value + 1);
      if (mountedRef.current) endPending("curated");
    }
  // This identity changes only when a stale Curated request releases its slot,
  // so PackSelector gets one fresh selected-radio attempt without letting a
  // Deck-library completion or failure retry Curated.
  }, [beginOnlinePending, clearActionError, endPending, reportActionError, staleCuratedSelectorRetry]);

  const resolveDeckLibrarySelector = useCallback(async () => {
    const backend = backendRef.current;
    if (!backend || !beginOnlinePending("deck_library")) return;
    clearActionError();
    const generation = ++requestRef.current.deckLibrary;
    try {
      const selector = await backend.deckLibrarySelector();
      if (mountedRef.current && generation === requestRef.current.deckLibrary) setDeckLibrarySelector(selector);
    } catch (error) {
      if (mountedRef.current && generation === requestRef.current.deckLibrary) reportActionError(error);
    } finally {
      if (mountedRef.current && generation !== requestRef.current.deckLibrary) setStaleDeckLibrarySelectorRetry((value) => value + 1);
      if (mountedRef.current) endPending("deck_library");
    }
  // Equivalent retry release for Deck library, independently of Curated.
  }, [beginOnlinePending, clearActionError, endPending, reportActionError, staleDeckLibrarySelectorRetry]);

  const recoverDeckLibraryConflict = useCallback((selectionGeneration: number, estimateGeneration: number): boolean => {
    if (selectionGeneration !== requestRef.current.deckLibrary) return false;
    // The rejected selector names a membership the planner has since replaced.
    // Forget only this local membership so PackSelector's selected-radio effect
    // can resolve the current digest and estimate it. It must never retry the
    // install itself: that remains an explicit user action.
    requestRef.current.deckLibrary += 1;
    setDeckLibrarySelector(null);
    setEstimate((current) => current?.selector.kind === "deck_library" ? null : current);
    // A later Curated or bulk estimate owns the only estimate slot while the
    // old start rejects. Do not invalidate its request or clear its progress:
    // only the estimate that produced this deck selection may be superseded.
    if (estimateGeneration === requestRef.current.estimate) {
      requestRef.current.estimate += 1;
      setEstimateProgress(null);
    }
    return true;
  }, []);

  const estimateInstall = useCallback(async (selector: InstallSelector) => {
    const backend = backendRef.current;
    const current = summaryRef.current;
    if (!backend || !current) return;
    // The generation bump follows the slot check, and the order is load-bearing:
    // bumping first would let a request this function then REFUSES invalidate
    // the one already running, so the in-flight estimate's own `finally` would
    // no longer recognise itself and would leave the scan-progress section on
    // screen for ever. A refused request must change nothing.
    if (!beginOnlinePending("estimate")) return;
    const generation = ++requestRef.current.estimate;
    const deckLibrarySelectionGeneration = selector.kind === "deck_library"
      ? requestRef.current.deckLibrary
      : null;
    const root = current.catalogRoot;
    const revision = current.installedRevision;
    clearActionError();
    setEstimateProgress(null);
    try {
      const value = await backend.estimateInstall(selector, (progress) => {
        if (mountedRef.current && generation === requestRef.current.estimate) setEstimateProgress(progress);
      });
      const latest = summaryRef.current;
      if (
        !mountedRef.current
        || generation !== requestRef.current.estimate
        || !latest
        || latest.catalogRoot !== root
        || latest.installedRevision !== revision
        || value.catalogRoot !== root
        || value.installedRevision !== revision
        || value.selector !== signedSelectorName(selector)
      ) return;
      setEstimate({ selector, value });
    } catch (error) {
      if (!mountedRef.current || generation !== requestRef.current.estimate) return;
      reportActionError(error);
      if (
        deckLibrarySelectionGeneration !== null
        && error instanceof VisualPackBackendError
        && error.kind === "conflict"
      ) recoverDeckLibraryConflict(deckLibrarySelectionGeneration, generation);
    } finally {
      if (mountedRef.current && generation === requestRef.current.estimate) setEstimateProgress(null);
      if (mountedRef.current) endPending("estimate");
    }
  }, [beginOnlinePending, clearActionError, endPending, recoverDeckLibraryConflict, reportActionError]);

  const trackStarted = useCallback(async (operationId: OperationStatus["operationId"], root: OperationStatus["catalogRoot"]) => {
    const backend = backendRef.current;
    if (!backend) return;
    try {
      const status = await backend.operationStatus(operationId);
      if (!mountedRef.current || status.operationId !== operationId || status.catalogRoot !== root) return;
      const selected = operationRef.current;
      const selectedProgress = progressRef.current;
      if (
        selected?.operationId === operationId
        && selectedProgress?.operation.operationId === operationId
        && operationRank(status) <= operationRank(selected)
      ) return;
      if (selected?.operationId === operationId && operationRank(status) < operationRank(selected)) return;
      if (operationIsTerminal(status)) {
        progressOutcomeRef.current = {
          identity: `${status.operationId}:${status.catalogRoot}`,
          failed: false,
          // A status READ BACK, so the ending is not witnessed: a `cancelled`
          // record here is equally a user cancel and a terminated failure, and
          // claiming the former would suppress the `failed` event that is
          // about to say which.
          cancelled: false,
          terminal: true,
        };
      }
      operationRef.current = status;
      progressRef.current = null;
      setOperation(status);
      setProgress(null);
      if (operationIsTerminal(status)) void refreshSummary(status.completedRevision ?? undefined);
    } catch (error) {
      if (mountedRef.current) {
        reportActionError(error);
        void refreshSummary();
      }
    }
  }, [refreshSummary, reportActionError]);

  const install = useCallback(async (selector: InstallSelector) => {
    const backend = backendRef.current;
    const current = summaryRef.current;
    if (!backend || !current || operationIsDurableMutation(operationRef.current)) return;
    const bound = estimate;
    if (
      !bound
      || selectorIdentity(bound.selector) !== selectorIdentity(selector)
      || bound.value.catalogRoot !== current.catalogRoot
      || bound.value.installedRevision !== current.installedRevision
    ) return;
    if (!beginOnlinePending("install")) return;
    const deckLibrarySelectionGeneration = selector.kind === "deck_library"
      ? requestRef.current.deckLibrary
      : null;
    const deckLibraryEstimateGeneration = requestRef.current.estimate;
    clearActionError();
    beginStartEventBuffer();
    try {
      const result = await backend.start({
        kind: "install",
        selector,
        objectEstimate: Number(bound.value.assetRecords),
      });
      if (!mountedRef.current) return;
      if (result.status === "healthy") {
        clearStartEventBuffer();
        await refreshSummary();
      } else {
        adoptStartedOperation(result.operationId, result.catalogRoot, "install");
        await trackStarted(result.operationId, result.catalogRoot);
      }
    } catch (error) {
      if (!mountedRef.current) return;
      const deckLibraryConflict = deckLibrarySelectionGeneration !== null
        && error instanceof VisualPackBackendError
        && error.kind === "conflict";
      // A newer art/revision invalidation may already have resolved D2 while
      // D1's start was in flight. That old rejection has no current UI state
      // to recover and must not overwrite the newer selection or its error.
      if (deckLibraryConflict && !recoverDeckLibraryConflict(deckLibrarySelectionGeneration, deckLibraryEstimateGeneration)) return;
      reportActionError(error);
      void refreshSummary();
    } finally {
      clearStartEventBuffer();
      if (mountedRef.current) endPending("install");
    }
  }, [adoptStartedOperation, beginOnlinePending, beginStartEventBuffer, clearActionError, clearStartEventBuffer, endPending, estimate, recoverDeckLibraryConflict, refreshSummary, reportActionError, trackStarted]);

  const cancel = useCallback(async () => {
    const backend = backendRef.current;
    const selected = operationRef.current;
    if (
      !backend
      || !selected
      || selected.state !== "downloading"
      || progressRef.current?.phase === "failed"
      || !beginPending("cancel")
    ) return;
    clearActionError();
    try {
      const status = await backend.cancel(selected.operationId);
      if (mountedRef.current && status.operationId === selected.operationId && status.catalogRoot === selected.catalogRoot) {
        if (operationIsTerminal(status)) {
          progressOutcomeRef.current = {
            identity: `${status.operationId}:${status.catalogRoot}`,
            failed: false,
            // Witnessed: this branch only runs because THIS panel asked for the
            // cancellation and the backend confirmed it. Nothing arriving
            // afterwards may re-report it as a failure.
            cancelled: true,
            terminal: true,
          };
        }
        operationRef.current = status;
        progressRef.current = null;
        setOperation(status);
        setProgress(null);
      }
    } catch (error) {
      if (mountedRef.current) {
        reportActionError(error);
        void trackStarted(selected.operationId, selected.catalogRoot);
      }
    } finally {
      if (mountedRef.current) endPending("cancel");
    }
  }, [beginPending, clearActionError, endPending, reportActionError, trackStarted]);

  const resume = useCallback(async () => {
    const backend = backendRef.current;
    const selected = operationRef.current;
    const selectedProgress = progressRef.current;
    if (
      !backend
      // `finalizing` too: a failure there leaves a record `create()` re-runs on
      // the next launch, so it is resumable and OperationProgress offers the
      // button. A guard that admitted only `downloading` would render a live
      // control that silently did nothing.
      || !selected
      || (selected.state !== "downloading" && selected.state !== "finalizing")
      || progressRef.current?.phase !== "failed"
      || !beginOnlinePending("resume")
    ) return;
    clearActionError();
    const previousOutcome = { ...progressOutcomeRef.current };
    progressOutcomeRef.current = {
      identity: `${selected.operationId}:${selected.catalogRoot}`,
      failed: false,
      cancelled: false,
      terminal: false,
    };
    try {
      const result = await backend.start({ kind: "resume", operationId: selected.operationId });
      if (!mountedRef.current) return;
      if (result.status === "healthy") {
        operationRef.current = null;
        progressRef.current = null;
        progressOutcomeRef.current = { identity: null, failed: false, cancelled: false, terminal: false };
        setOperation(null);
        setProgress(null);
        await refreshSummary();
      } else {
        if (progressRef.current === selectedProgress) {
          progressRef.current = null;
          setProgress(null);
        }
        await trackStarted(result.operationId, result.catalogRoot);
      }
    } catch (error) {
      if (mountedRef.current) {
        if (progressRef.current === selectedProgress) progressOutcomeRef.current = previousOutcome;
        reportActionError(error);
        void trackStarted(selected.operationId, selected.catalogRoot);
      }
    } finally {
      if (mountedRef.current) endPending("resume");
    }
  }, [beginOnlinePending, clearActionError, endPending, refreshSummary, reportActionError, trackStarted]);

  const verify = useCallback(async (mode: VerificationMode) => {
    const backend = backendRef.current;
    const current = summaryRef.current;
    if (!backend || !current) return;
    const generation = ++requestRef.current.verify;
    const root = current.catalogRoot;
    const revision = current.installedRevision;
    const pendingKey = `verify:${mode}`;
    if (!beginPending(pendingKey)) return;
    clearActionError();
    try {
      const result = await backend.verify(mode);
      if (
        mountedRef.current
        && generation === requestRef.current.verify
        && summaryRef.current?.catalogRoot === root
        && summaryRef.current?.installedRevision === revision
        && result.revision === revision
      ) setVerification({ catalogRoot: root, installedRevision: revision, value: result });
    } catch (error) {
      if (mountedRef.current && generation === requestRef.current.verify) reportActionError(error);
    } finally {
      if (mountedRef.current) endPending(pendingKey);
    }
  }, [beginPending, clearActionError, endPending, reportActionError]);

  /**
   * Repair the named packs. DELIBERATELY NOT GATED ON STORAGE, unlike install.
   *
   * `install` sends an `objectEstimate` the user has seen, and `reserveStorage`
   * refuses the request when even the cheapest reading of that figure cannot
   * fit. A repair reaches none of that: `reserveStorage`'s size branch is
   * gated on `request.kind === "install"`, and `StartRequest`'s repair arm
   * carries pack ids only.
   *
   * Producing a figure for it is not free, and it is not uniformly expensive
   * either — the honest shape is per selector. `reserveStorage` substitutes its
   * own count for CURATED alone (`curatedFetchCount`, no catalog scan); every
   * other selector keeps the caller's `objectEstimate`, and the only way to
   * produce one for them is `countScryfallAssets` reading the whole compressed
   * Scryfall archive. So a curated repair could in principle be gated cheaply —
   * except that `start()` filters it out entirely, leaving nothing to gate —
   * while a bulk repair could be gated only by putting a multi-gigabyte scan
   * behind a button labelled Repair.
   *
   * And a repair is not free of new bytes, so "it downloads nothing an install
   * did not account for" is NOT available as a reason: `start()` re-derives a
   * repair's selector from the root the pack was INSTALLED at, then drops any
   * selector already installed at the root it resolves to — so repairing a
   * current pack is a no-op, while repairing one whose catalog root has moved
   * re-fetches it at the new root, which for `complete` is the entire catalog.
   *
   * What it rests on is the asymmetry `reserveStorage` documents, which holds
   * whatever a count would have cost. Running out of quota mid-download is
   * classified `storage`, `storage` is retryable, so the operation record stays
   * `downloading`, the panel offers Resume, and the resume skips every object
   * already cached. A wrong refusal has no override anywhere. Given a choice
   * between a recoverable failure and an unappealable one, the repair path takes
   * the recoverable one.
   */
  const runStart = useCallback(async (packIds: PackId[]) => {
    const backend = backendRef.current;
    if (
      !backend
      || packIds.length === 0
      || operationIsDurableMutation(operationRef.current)
      || !beginOnlinePending("repair")
    ) return;
    clearActionError();
    beginStartEventBuffer();
    try {
      const result = await backend.start({ kind: "repair", packIds });
      if (!mountedRef.current) return;
      if (result.status === "healthy") {
        clearStartEventBuffer();
        await refreshSummary();
      } else {
        adoptStartedOperation(result.operationId, result.catalogRoot, "repair");
        await trackStarted(result.operationId, result.catalogRoot);
      }
    } catch (error) {
      if (mountedRef.current) {
        reportActionError(error);
        void refreshSummary();
      }
    } finally {
      clearStartEventBuffer();
      if (mountedRef.current) endPending("repair");
    }
  }, [adoptStartedOperation, beginOnlinePending, beginStartEventBuffer, clearActionError, clearStartEventBuffer, endPending, refreshSummary, reportActionError, trackStarted]);

  const runRemoval = useCallback(async (selector: RemovalSelector, mode: RemovalMode) => {
    const backend = backendRef.current;
    if (!backend || operationIsDurableMutation(operationRef.current) || !beginPending("remove")) return;
    clearActionError();
    try {
      const result = await backend.remove(selector, mode);
      if (!mountedRef.current) return;
      setRemoval(result);
      await refreshSummary(result.revision);
    } catch (error) {
      if (!mountedRef.current) return;
      if (errorKind(error) === "conflict" && selector.kind === "packs" && mode === "reject_dependents") {
        setConfirmation({ kind: "cascade", selector });
      } else {
        reportActionError(error);
        void refreshSummary();
      }
    } finally {
      if (mountedRef.current) endPending("remove");
    }
  }, [beginPending, clearActionError, endPending, refreshSummary, reportActionError]);

  const removeSelected = useCallback((packIds: PackId[]) => {
    if (packIds.length > 0) void runRemoval({ kind: "packs", packIds }, "reject_dependents");
  }, [runRemoval]);
  const removeComplete = useCallback(() => {
    if (operationIsDurableMutation(operationRef.current)) return;
    const root = summaryRef.current?.catalogRoot;
    if (root) setConfirmation({ kind: "complete", selector: { kind: "complete", rootSha256: root } });
  }, []);
  const removeAll = useCallback(() => {
    if (operationIsDurableMutation(operationRef.current)) return;
    setConfirmation({ kind: "all", selector: { kind: "all_installed" } });
  }, []);
  const dismissConfirmation = useCallback(() => setConfirmation(null), []);
  const confirmRemoval = useCallback(() => {
    if (!confirmation) return;
    const frozen = confirmation;
    setConfirmation(null);
    void runRemoval(frozen.selector, frozen.kind === "cascade" ? "cascade_dependents" : "reject_dependents");
  }, [confirmation, runRemoval]);

  return {
    availability,
    summary,
    curatedSelector,
    deckLibrarySelector,
    curatedDrift,
    deckLibraryDrift,
    estimate,
    estimateProgress,
    operation,
    progress,
    verification,
    removal,
    actionError,
    actionErrorDetail,
    actionErrorRefusal,
    pendingActions,
    durableMutationActive: operationIsDurableMutation(operation),
    confirmation,
    retry,
    refresh,
    resolveCuratedSelector,
    resolveDeckLibrarySelector,
    estimateInstall,
    install,
    cancel,
    resume,
    verify,
    repair: runStart,
    removeSelected,
    removeComplete,
    removeAll,
    confirmRemoval,
    dismissConfirmation,
  };
}
