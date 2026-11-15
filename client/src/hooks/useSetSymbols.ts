import { useEffect, useMemo, useState } from "react";

import {
  cachedSetCatalog,
  ensureSetCatalog,
  type ScryfallSetCatalog,
  type ScryfallSetInfo,
} from "../services/setCatalog.ts";
import { setIconCandidate } from "../services/visualPacks/candidateKeys.ts";
import type { CandidateKey } from "../services/visualPacks/types.ts";
import {
  useFixedVisualImage,
  type UseFixedVisualImageResult,
} from "./useFixedVisualImage.ts";

export type { ScryfallSetCatalog, ScryfallSetInfo };

interface UseSetCatalogResult {
  catalog: ScryfallSetCatalog | null;
  isLoading: boolean;
}

export function useSetCatalog(): UseSetCatalogResult {
  const [catalog, setCatalog] = useState<ScryfallSetCatalog | null>(cachedSetCatalog());
  const [isLoading, setIsLoading] = useState(cachedSetCatalog() === null);

  useEffect(() => {
    if (cachedSetCatalog()) {
      setCatalog(cachedSetCatalog());
      setIsLoading(false);
      return;
    }

    let cancelled = false;
    setIsLoading(true);
    void ensureSetCatalog().then((loaded) => {
      if (cancelled) return;
      if (loaded) setCatalog(loaded);
      setIsLoading(false);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  return { catalog, isLoading };
}

function setIconRequest(setCode: string | undefined): {
  code: string;
  candidate: CandidateKey;
} | null {
  if (!setCode) return null;
  const code = setCode.toLowerCase().normalize("NFC");
  try {
    return { code, candidate: setIconCandidate(code) };
  } catch {
    return null;
  }
}

export function useSetSymbol(setCode: string | undefined): UseFixedVisualImageResult {
  const { catalog } = useSetCatalog();
  const request = useMemo(() => setIconRequest(setCode), [setCode]);
  const remote = request ? catalog?.[request.code]?.icon_svg_uri ?? null : null;
  return useFixedVisualImage(request?.candidate ?? null, remote);
}
