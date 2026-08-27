import { useCallback, useEffect, useMemo, useState } from "react";

import type { PlayerAvatarIdentity } from "../services/playerAvatars.ts";
import type { CardImageSource } from "../services/visualPacks/types.ts";
import { useCardImage } from "./useCardImage.ts";

export interface UsePlayerAvatarImageResult {
  src: string | null;
  isLoading: boolean;
  source?: CardImageSource | null;
  advanceFailedSource(failedSrc: string): void;
}

function validAvatarIdentity(identity: PlayerAvatarIdentity | null): PlayerAvatarIdentity | null {
  if (!identity || typeof identity !== "object") return null;
  switch (identity.kind) {
    case "card":
      return typeof identity.cardName === "string" && identity.cardName.trim().length > 0
        ? identity
        : null;
    case "external":
      return typeof identity.url === "string" && identity.url.trim().length > 0
        ? identity
        : null;
  }
}

/** Resolve one semantic gameplay avatar without exposing its identity to DOM consumers. */
export function usePlayerAvatarImage(
  identity: PlayerAvatarIdentity | null,
): UsePlayerAvatarImageResult {
  const validIdentity = useMemo(() => validAvatarIdentity(identity), [identity]);
  const cardName = validIdentity?.kind === "card" ? validIdentity.cardName : "";
  const cardResult = useCardImage(cardName, { size: "art_crop" });
  const advanceCardFailedSource = cardResult.advanceFailedSource;
  const externalUrl = validIdentity?.kind === "external" ? validIdentity.url : null;
  const [failedExternalUrl, setFailedExternalUrl] = useState<string | null>(null);

  useEffect(() => {
    setFailedExternalUrl(null);
  }, [externalUrl]);

  const advanceFailedSource = useCallback((failedSrc: string) => {
    if (validIdentity?.kind === "card") {
      advanceCardFailedSource?.(failedSrc);
      return;
    }
    if (externalUrl && failedSrc === externalUrl) {
      setFailedExternalUrl((current) => current ?? externalUrl);
    }
  }, [advanceCardFailedSource, externalUrl, validIdentity]);

  if (validIdentity?.kind === "card") {
    return {
      src: cardResult.src,
      isLoading: cardResult.isLoading,
      source: cardResult.source,
      advanceFailedSource,
    };
  }
  if (externalUrl) {
    return {
      src: failedExternalUrl === externalUrl ? null : externalUrl,
      isLoading: false,
      advanceFailedSource,
    };
  }
  return { src: null, isLoading: false, advanceFailedSource };
}
