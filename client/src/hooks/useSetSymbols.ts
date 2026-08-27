import { useEffect, useMemo, useState } from "react";

import { setIconCandidate } from "../services/visualPacks/candidateKeys.ts";
import type { CandidateKey } from "../services/visualPacks/types.ts";
import {
  useFixedVisualImage,
  type UseFixedVisualImageResult,
} from "./useFixedVisualImage.ts";

export interface ScryfallSetInfo {
  name: string;
  released_at: string;
  icon_svg_uri?: string;
}

export type ScryfallSetCatalog = Readonly<Record<string, ScryfallSetInfo>>;

interface UseSetCatalogResult {
  catalog: ScryfallSetCatalog | null;
  isLoading: boolean;
}

let cachedCatalog: ScryfallSetCatalog | null = null;
let catalogPromise: Promise<ScryfallSetCatalog | null> | null = null;

function validateSetCatalog(value: unknown): ScryfallSetCatalog | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;

  const catalog: Record<string, ScryfallSetInfo> = {};
  for (const [code, raw] of Object.entries(value)) {
    if (
      !raw
      || typeof raw !== "object"
      || Array.isArray(raw)
      || typeof (raw as Record<string, unknown>).name !== "string"
      || typeof (raw as Record<string, unknown>).released_at !== "string"
    ) {
      return null;
    }
    const icon = (raw as Record<string, unknown>).icon_svg_uri;
    if (icon !== undefined && icon !== null && typeof icon !== "string") return null;
    catalog[code] = {
      name: (raw as Record<string, string>).name,
      released_at: (raw as Record<string, string>).released_at,
      ...(typeof icon === "string" ? { icon_svg_uri: icon } : {}),
    };
  }
  return catalog;
}

function loadSetCatalog(): Promise<ScryfallSetCatalog | null> {
  if (cachedCatalog) return Promise.resolve(cachedCatalog);
  if (catalogPromise) return catalogPromise;

  catalogPromise = fetch(__SCRYFALL_SETS_URL__)
    .then(async (response) => response.ok ? validateSetCatalog(await response.json()) : null)
    .then((catalog) => {
      if (catalog) cachedCatalog = catalog;
      return catalog;
    })
    .catch(() => null)
    .finally(() => {
      catalogPromise = null;
    });
  return catalogPromise;
}

export function useSetCatalog(): UseSetCatalogResult {
  const [catalog, setCatalog] = useState<ScryfallSetCatalog | null>(cachedCatalog);
  const [isLoading, setIsLoading] = useState(cachedCatalog === null);

  useEffect(() => {
    if (cachedCatalog) {
      setCatalog(cachedCatalog);
      setIsLoading(false);
      return;
    }

    let cancelled = false;
    setIsLoading(true);
    void loadSetCatalog().then((loaded) => {
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
