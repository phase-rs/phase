import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { manaSymbolSourceUrl } from "../services/scryfall.ts";
import { manaSymbolCandidate } from "../services/visualPacks/candidateKeys.ts";
import { visualPackRepository } from "../services/visualPacks/repository.ts";
import type {
  CandidateKey,
  CardImageSource,
} from "../services/visualPacks/types.ts";
import { useEffectiveOffline } from "../stores/connectivityStore.ts";
import { nextImageSourceIndex } from "./imageSourceLadder.ts";

export interface UseFixedVisualImageResult {
  src: string | null;
  isLoading: boolean;
  source: CardImageSource | null;
  advanceFailedSource(failedSrc: string): void;
}

declare const manaSymbolShardBrand: unique symbol;
export type ManaSymbolShard = string & { readonly [manaSymbolShardBrand]: true };

/** Resolve one fixed visual without synthesizing responsive companion rungs. */
export function useFixedVisualImage(
  candidate: CandidateKey | null,
  remoteSrc: string | null,
): UseFixedVisualImageResult {
  const effectiveOffline = useEffectiveOffline();
  const [repositoryRevision, setRepositoryRevision] = useState(
    visualPackRepository.currentRevision(),
  );
  const requestKey = useMemo(
    () => JSON.stringify([candidate, remoteSrc, effectiveOffline, repositoryRevision]),
    [candidate, remoteSrc, effectiveOffline, repositoryRevision],
  );
  const [stateRequestKey, setStateRequestKey] = useState<string | null>(null);
  const [sources, setSources] = useState<CardImageSource[]>([]);
  const [sourceIndex, setSourceIndex] = useState(0);
  const [isLoading, setIsLoading] = useState(candidate !== null);
  const failedSources = useRef<{ generation: string; values: Set<string> }>({
    generation: "",
    values: new Set(),
  });

  useEffect(() => visualPackRepository.subscribe(() => {
    setRepositoryRevision(visualPackRepository.currentRevision());
  }), []);

  useEffect(() => {
    let cancelled = false;
    failedSources.current = { generation: requestKey, values: new Set() };
    setStateRequestKey(requestKey);
    setSources([]);
    setSourceIndex(0);

    if (!candidate) {
      setIsLoading(false);
      return () => {
        cancelled = true;
      };
    }

    setIsLoading(true);
    void visualPackRepository.resolve({
      groups: [{ requested: [candidate] }],
      rung: "normal",
      allowRemote: !effectiveOffline,
      remote: remoteSrc ? { src: remoteSrc } : null,
    }).then((result) => {
      if (cancelled) return;
      setSources(result.sources);
      setSourceIndex(0);
      setIsLoading(false);
    });

    return () => {
      cancelled = true;
    };
  }, [candidate, effectiveOffline, remoteSrc, requestKey]);

  const advanceFailedSource = useCallback((failedSrc: string) => {
    if (failedSources.current.generation !== requestKey) return;
    const nextIndex = nextImageSourceIndex(
      sources,
      sourceIndex,
      failedSources.current.values,
      failedSrc,
    );
    if (nextIndex === null) return;
    setSourceIndex(nextIndex);
  }, [requestKey, sourceIndex, sources]);

  if (!candidate) {
    return { src: null, isLoading: false, source: null, advanceFailedSource };
  }
  if (stateRequestKey !== requestKey) {
    return { src: null, isLoading: true, source: null, advanceFailedSource };
  }

  const activeSource = sources[sourceIndex] ?? null;
  return {
    src: activeSource?.src ?? null,
    isLoading,
    source: activeSource,
    advanceFailedSource,
  };
}

/** Resolve one finite mana shard admitted by the static symbol catalog. */
export function useManaSymbolImage(
  shard: ManaSymbolShard | null,
): UseFixedVisualImageResult {
  return useFixedVisualImage(
    shard ? manaSymbolCandidate(shard) : null,
    shard ? manaSymbolSourceUrl(shard) : null,
  );
}
