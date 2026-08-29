import type {
  CatalogStatus,
  CatalogSummary,
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
  VerificationMode,
  VerificationResponse,
  VisualPackErrorKind,
} from "./types.ts";

export class VisualPackBackendError extends Error {
  constructor(public readonly kind: VisualPackErrorKind, detail?: string) {
    super(detail || `visual-pack backend ${kind}`);
    this.name = "VisualPackBackendError";
  }
}

export interface VisualPackBackend {
  catalogStatus(): Promise<CatalogStatus>;
  refreshCatalog(): Promise<CatalogSummary>;
  catalogSummary(): Promise<CatalogSummary>;
  estimateInstall(selector: InstallSelector): Promise<InstallEstimate>;
  start(request: StartRequest): Promise<StartResponse>;
  cancel(operationId: OperationId): Promise<OperationStatus>;
  operationStatus(operationId: OperationId): Promise<OperationStatus>;
  remove(selector: RemovalSelector, mode: RemovalMode): Promise<RemovalResponse>;
  verify(mode: VerificationMode): Promise<VerificationResponse>;
  resolve(keys: ResolutionKey[]): Promise<ResolutionResponse>;
  subscribeProgress(listener: (event: ProgressEvent) => void): Promise<() => void>;
  subscribeRevision(listener: (event: RevisionEvent) => void): Promise<() => void>;
}
