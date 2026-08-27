import { useCallback, useEffect, useRef, useState } from "react";

import { loadVisualPackBackend } from "../../../services/platform.ts";
import {
  VisualPackBackendError,
  type VisualPackBackend,
} from "../../../services/visualPacks/backend.ts";
import {
  compareRevisions,
  type CatalogSummary,
  type InstallEstimate,
  type InstallSelector,
  type OperationStatus,
  type PackId,
  type ProgressEvent,
  type RemovalMode,
  type RemovalResponse,
  type RemovalSelector,
  type RevisionEvent,
  type VerificationMode,
  type VerificationResponse,
  type VisualPackErrorKind,
} from "../../../services/visualPacks/types.ts";

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
  terminal: boolean;
}

export interface VisualPackManagerState {
  availability: ManagerAvailability;
  summary: CatalogSummary | null;
  estimate: BoundEstimate | null;
  operation: OperationStatus | null;
  progress: ProgressEvent | null;
  verification: BoundVerification | null;
  removal: RemovalResponse | null;
  actionError: VisualPackErrorKind | null;
  actionErrorDetail: string | null;
  pendingActions: ReadonlySet<string>;
  durableMutationActive: boolean;
  confirmation: FrozenConfirmation | null;
  retry(): void;
  refresh(): void;
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

export function hasPendingVisualPackMutation(pending: ReadonlySet<string>): boolean {
  return [...pending].some((entry) => MUTATION_ACTIONS.has(entry));
}

function errorKind(error: unknown): VisualPackErrorKind {
  return error instanceof VisualPackBackendError ? error.kind : "internal";
}

function errorDetail(error: unknown): string | null {
  if (error instanceof Error) return error.message || null;
  return typeof error === "string" && error ? error : null;
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
  }
}

function signedSelectorName(selector: InstallSelector): string {
  return selector.kind === "complete" ? "complete" : selectorIdentity(selector);
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
  const progressOutcomeRef = useRef<ProgressOutcome>({ identity: null, failed: false, terminal: false });
  const startEventBufferRef = useRef({ active: false, events: new Map<string, ProgressEvent>() });
  const pendingRef = useRef(new Set<string>());
  const listenersRef = useRef<Array<() => void>>([]);
  const initializedRef = useRef(false);
  const requestRef = useRef({ initialize: 0, summary: 0, estimate: 0, verify: 0 });

  const [availability, setAvailability] = useState<ManagerAvailability>({ kind: "loading" });
  const [summary, setSummary] = useState<CatalogSummary | null>(null);
  const [estimate, setEstimate] = useState<BoundEstimate | null>(null);
  const [operation, setOperation] = useState<OperationStatus | null>(null);
  const [progress, setProgress] = useState<ProgressEvent | null>(null);
  const [verification, setVerification] = useState<BoundVerification | null>(null);
  const [removal, setRemoval] = useState<RemovalResponse | null>(null);
  const [actionError, setActionError] = useState<VisualPackErrorKind | null>(null);
  const [actionErrorDetail, setActionErrorDetail] = useState<string | null>(null);
  const [pendingActions, setPendingActions] = useState<ReadonlySet<string>>(new Set());
  const [confirmation, setConfirmation] = useState<FrozenConfirmation | null>(null);

  const clearActionError = useCallback(() => {
    setActionError(null);
    setActionErrorDetail(null);
  }, []);
  const reportActionError = useCallback((error: unknown) => {
    setActionError(errorKind(error));
    setActionErrorDetail(errorDetail(error));
  }, []);

  const beginPending = useCallback((value: string): boolean => {
    if (pendingRef.current.has(value)) return false;
    if (MUTATION_ACTIONS.has(value) && hasPendingVisualPackMutation(pendingRef.current)) return false;
    pendingRef.current.add(value);
    setPendingActions(new Set(pendingRef.current));
    return true;
  }, []);
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
      setEstimate(null);
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
    const selected = operationRef.current;
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
      outcome.terminal = false;
    }
    if (outcome.failed || outcome.terminal) return;
    if (operationRank(event.operation) < operationRank(selected)) return;
    if (event.phase === "failed") outcome.failed = true;
    if (progressIsTerminal(event)) outcome.terminal = true;
    operationRef.current = event.operation;
    progressRef.current = event;
    setOperation(event.operation);
    setProgress(event);
    if (event.phase === "failed") {
      if (event.error) {
        setActionError(event.error);
        setActionErrorDetail(null);
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
    progressOutcomeRef.current = { identity, failed: false, terminal: false };
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
      for (const unlisten of listeners.splice(0)) unlisten();
    };
  }, [initialize]);

  const retry = useCallback(() => {
    if (!initializedRef.current) void initialize();
  }, [initialize]);

  const refresh = useCallback(async () => {
    const backend = backendRef.current;
    if (!backend || !beginPending("refresh")) return;
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
  }, [acceptSummary, beginPending, clearActionError, endPending, reportActionError]);

  const estimateInstall = useCallback(async (selector: InstallSelector) => {
    const backend = backendRef.current;
    const current = summaryRef.current;
    if (!backend || !current) return;
    const generation = ++requestRef.current.estimate;
    const root = current.catalogRoot;
    const revision = current.installedRevision;
    if (!beginPending("estimate")) return;
    clearActionError();
    try {
      const value = await backend.estimateInstall(selector);
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
      if (mountedRef.current && generation === requestRef.current.estimate) reportActionError(error);
    } finally {
      if (mountedRef.current) endPending("estimate");
    }
  }, [beginPending, clearActionError, endPending, reportActionError]);

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
    if (!beginPending("install")) return;
    clearActionError();
    beginStartEventBuffer();
    try {
      const result = await backend.start({ kind: "install", selector });
      if (!mountedRef.current) return;
      if (result.status === "healthy") {
        clearStartEventBuffer();
        await refreshSummary();
      } else {
        adoptStartedOperation(result.operationId, result.catalogRoot, "install");
        await trackStarted(result.operationId, result.catalogRoot);
      }
    } catch (error) {
      if (mountedRef.current) {
        reportActionError(error);
        void refreshSummary();
      }
    } finally {
      clearStartEventBuffer();
      if (mountedRef.current) endPending("install");
    }
  }, [adoptStartedOperation, beginPending, beginStartEventBuffer, clearActionError, clearStartEventBuffer, endPending, estimate, refreshSummary, reportActionError, trackStarted]);

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
      || !selected
      || selected.state !== "downloading"
      || progressRef.current?.phase !== "failed"
      || !beginPending("resume")
    ) return;
    clearActionError();
    const previousOutcome = { ...progressOutcomeRef.current };
    progressOutcomeRef.current = {
      identity: `${selected.operationId}:${selected.catalogRoot}`,
      failed: false,
      terminal: false,
    };
    try {
      const result = await backend.start({ kind: "resume", operationId: selected.operationId });
      if (!mountedRef.current) return;
      if (result.status === "healthy") {
        operationRef.current = null;
        progressRef.current = null;
        progressOutcomeRef.current = { identity: null, failed: false, terminal: false };
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
  }, [beginPending, clearActionError, endPending, refreshSummary, reportActionError, trackStarted]);

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

  const runStart = useCallback(async (packIds: PackId[]) => {
    const backend = backendRef.current;
    if (
      !backend
      || packIds.length === 0
      || operationIsDurableMutation(operationRef.current)
      || !beginPending("repair")
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
  }, [adoptStartedOperation, beginPending, beginStartEventBuffer, clearActionError, clearStartEventBuffer, endPending, refreshSummary, reportActionError, trackStarted]);

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
    estimate,
    operation,
    progress,
    verification,
    removal,
    actionError,
    actionErrorDetail,
    pendingActions,
    durableMutationActive: operationIsDurableMutation(operation),
    confirmation,
    retry,
    refresh,
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
