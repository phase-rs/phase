import { loadVisualPackBackend } from "../platform.ts";
import { VisualPackBackendError } from "./backend.ts";
import type { VisualPackBackend } from "./backend.ts";
import { compareRevisions, installedRevision } from "./types.ts";
import type {
  CandidateKey,
  CardImageSource,
  ImageRungs,
  InstalledRevision,
  PackId,
  ResolutionKey,
  ResolvedAsset,
  VisualImageRung,
} from "./types.ts";

export interface VisualCandidateGroup {
  requested: CandidateKey[];
  small?: CandidateKey[];
  normal?: CandidateKey[];
  /** Limit every rung in this group to one installed pack. */
  packId?: PackId;
  /** A semantic fallback may only use one distinct installed asset. */
  requireUnambiguousAsset?: boolean;
}

export interface VisualRepositoryRequest {
  groups: VisualCandidateGroup[];
  rung: VisualImageRung | "large";
  allowRemote: boolean;
  remote?: { src: string; rungs?: ImageRungs } | null;
}

export interface VisualRepositoryResult {
  revision: InstalledRevision;
  sources: CardImageSource[];
}

type BackendLoader = () => Promise<VisualPackBackend | null>;

const utf8 = new TextEncoder();
function compareUtf8(left: string, right: string): number {
  const a = utf8.encode(left);
  const b = utf8.encode(right);
  const length = Math.min(a.length, b.length);
  for (let index = 0; index < length; index += 1) {
    if (a[index] !== b[index]) return a[index] - b[index];
  }
  return a.length - b.length;
}

function sortMatches(matches: ResolvedAsset[]): ResolvedAsset[] {
  return [...matches].sort((left, right) =>
    compareUtf8(left.packId, right.packId) || compareUtf8(left.assetKey, right.assetKey));
}

function groupMatches(matches: ResolvedAsset[], group: VisualCandidateGroup): ResolvedAsset[] {
  const filtered = group.packId
    ? matches.filter((match) => match.packId === group.packId)
    : matches;
  if (group.requireUnambiguousAsset && new Set(filtered.map((match) => match.assetKey)).size !== 1) {
    return [];
  }
  return sortMatches(filtered);
}

function unambiguousCompanion(
  requested: ResolvedAsset[],
  match: ResolvedAsset,
  companions: ResolvedAsset[],
): ResolvedAsset | undefined {
  const sameAuthority = (candidate: ResolvedAsset) =>
    candidate.packId === match.packId && candidate.catalogRoot === match.catalogRoot;
  const exact = companions.find((candidate) =>
    sameAuthority(candidate) && candidate.assetKey === match.assetKey);
  if (exact) return exact;
  const requestedAssets = new Set(requested.filter(sameAuthority).map((candidate) => candidate.assetKey));
  const companionAssets = new Map(
    companions.filter(sameAuthority).map((candidate) => [candidate.assetKey, candidate]),
  );
  return requestedAssets.size === 1 && companionAssets.size === 1
    ? companionAssets.values().next().value
    : undefined;
}

export class VisualPackRepository {
  private revision = installedRevision("0");
  private backendPromise: Promise<VisualPackBackend | null> | null = null;
  private revisionUnlisten: Promise<() => void> | null = null;
  private readonly listeners = new Set<() => void>();

  constructor(private readonly loadBackend: BackendLoader = loadVisualPackBackend) {}

  currentRevision(): InstalledRevision { return this.revision; }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    void this.ensureRevisionSubscription();
    return () => this.listeners.delete(listener);
  }

  private notify(revision: InstalledRevision): void {
    if (compareRevisions(revision, this.revision) <= 0) return;
    this.revision = revision;
    for (const listener of this.listeners) listener();
  }

  private backend(): Promise<VisualPackBackend | null> {
    this.backendPromise ??= this.loadBackend().catch(() => {
      this.backendPromise = null;
      return null;
    });
    return this.backendPromise;
  }

  private ensureRevisionSubscription(): Promise<void> {
    this.revisionUnlisten ??= this.backend()
      .then(async (backend) => {
        if (!backend) {
          this.revisionUnlisten = null;
          return () => {};
        }
        try {
          return await backend.subscribeRevision((event) => this.notify(event.revision));
        } catch {
          this.revisionUnlisten = null;
          return () => {};
        }
      });
    return this.revisionUnlisten.then(() => undefined);
  }

  private fallbackSources(request: VisualRepositoryRequest): CardImageSource[] {
    return [
      ...(request.allowRemote && request.remote
        ? [{ kind: "remote" as const, src: request.remote.src, rungs: request.remote.rungs }]
        : []),
      { kind: "fallback" as const, src: null },
    ];
  }

  async resolve(request: VisualRepositoryRequest): Promise<VisualRepositoryResult> {
    const captured = this.revision;
    const backend = await this.backend();
    if (!backend || request.groups.length === 0) {
      return { revision: this.revision, sources: this.fallbackSources(request) };
    }
    await this.ensureRevisionSubscription();
    try {
      const first = await this.resolveOnce(backend, request);
      if (compareRevisions(first.revision, this.revision) < 0) {
        const retry = await this.resolveOnce(backend, request);
        if (compareRevisions(retry.revision, this.revision) < 0) {
          return { revision: this.revision, sources: this.fallbackSources(request) };
        }
        this.notify(retry.revision);
        return retry;
      }
      if (compareRevisions(first.revision, captured) >= 0) this.notify(first.revision);
      return first;
    } catch (error) {
      if (error instanceof VisualPackBackendError) {
        return { revision: this.revision, sources: this.fallbackSources(request) };
      }
      return { revision: this.revision, sources: this.fallbackSources(request) };
    }
  }

  private async resolveOnce(
    backend: VisualPackBackend,
    request: VisualRepositoryRequest,
  ): Promise<VisualRepositoryResult> {
    const keys: ResolutionKey[] = [];
    const indexes = request.groups.map((group) => ({
      requested: group.requested.map((key) => (keys.push({ kind: "candidate", key }) - 1)),
      small: (group.small ?? []).map((key) => (keys.push({ kind: "candidate", key }) - 1)),
      normal: (group.normal ?? []).map((key) => (keys.push({ kind: "candidate", key }) - 1)),
    }));
    const response = await backend.resolve(keys);
    for (let groupIndex = 0; groupIndex < indexes.length; groupIndex += 1) {
      const index = indexes[groupIndex];
      for (let candidateIndex = 0; candidateIndex < index.requested.length; candidateIndex += 1) {
        const requested = groupMatches(response.entries[index.requested[candidateIndex]].matches, request.groups[groupIndex]);
        if (requested.length === 0) continue;
        const smallOrdinal = index.small[candidateIndex];
        const normalOrdinal = index.normal[candidateIndex];
        const smallMatches = smallOrdinal === undefined
          ? []
          : groupMatches(response.entries[smallOrdinal].matches, request.groups[groupIndex]);
        const normalMatches = normalOrdinal === undefined
          ? []
          : groupMatches(response.entries[normalOrdinal].matches, request.groups[groupIndex]);
        const seen = new Set<string>();
        const installed = requested.flatMap<CardImageSource>((match) => {
          if (seen.has(match.assetKey)) return [];
          seen.add(match.assetKey);
          const small = unambiguousCompanion(requested, match, smallMatches);
          const normal = unambiguousCompanion(requested, match, normalMatches);
          const rungs = request.rung === "art_crop" || (!small && !normal) ? undefined : {
            ...(small ? { small: small.url } : {}),
            ...(normal ? { normal: normal.url } : {}),
          };
          return [{
            kind: "installed", src: match.url, rungs,
            assetKey: match.assetKey, packId: match.packId, catalogRoot: match.catalogRoot,
          }];
        });
        return {
          revision: response.revision,
          sources: [...installed, ...this.fallbackSources(request)],
        };
      }
    }
    return { revision: response.revision, sources: this.fallbackSources(request) };
  }
}

export const visualPackRepository = new VisualPackRepository();
