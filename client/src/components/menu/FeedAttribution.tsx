import { useTranslation } from "react-i18next";

import type { FeedSource } from "../../types/feed";

interface FeedAttributionProps {
  attribution: NonNullable<FeedSource["attribution"]>;
}

/** Linked credit to a feed's data provider, shown wherever the feed's decks are listed. */
export function FeedAttribution({ attribution }: FeedAttributionProps) {
  const { t } = useTranslation("menu");
  return (
    <p className="mt-0.5 text-xs text-slate-500">
      {t("feedManager.dataProvidedBy")}{" "}
      <a
        href={attribution.url}
        target="_blank"
        rel="noopener noreferrer"
        className="text-slate-300 underline hover:text-white"
      >
        {attribution.name}
      </a>
    </p>
  );
}
