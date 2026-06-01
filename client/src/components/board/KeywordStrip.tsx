import { memo, useMemo } from "react";

import type { Keyword } from "../../adapter/types";
import {
  getKeywordDisplayText,
  getKeywordName,
  getKeywordReminderText,
  isGrantedKeyword,
  sortKeywords,
} from "../../viewmodel/keywordProps";

interface KeywordStripProps {
  keywords: Keyword[];
  baseKeywords: Keyword[];
  /**
   * Map from displayed keyword name to the granting source's display name,
   * built by `buildGrantedKeywordSources` from `state.attribution`. When
   * absent (legacy state or attribution side-table empty), falls back to
   * the printed-vs-current name diff for the granted/base distinction
   * without source info.
   */
  sourceByKeyword?: Map<string, string>;
}

export const KeywordStrip = memo(function KeywordStrip({
  keywords,
  baseKeywords,
  sourceByKeyword,
}: KeywordStripProps) {
  const sorted = useMemo(() => sortKeywords(keywords), [keywords]);

  const items = useMemo(
    () =>
      sorted.map((kw) => {
        const name = getKeywordName(kw);
        return {
          text: getKeywordDisplayText(kw),
          granted: isGrantedKeyword(kw, baseKeywords),
          source: sourceByKeyword?.get(name),
          reminder: getKeywordReminderText(kw),
        };
      }),
    [sorted, baseKeywords, sourceByKeyword],
  );

  if (items.length === 0) return null;

  const fullText = items
    .map((i) => (i.source ? `${i.text} (from ${i.source})` : i.text))
    .join(" \u00B7 ");

  return (
    <div
      className="absolute inset-x-0 bottom-0 z-20 overflow-hidden truncate bg-black/60 px-1 py-[1px] text-[9px] leading-tight"
      title={fullText}
    >
      {items.map((item, i) => (
        <span
          key={i}
          title={[
            item.source ? `${item.text} \u2014 from ${item.source}` : item.text,
            item.reminder,
          ].filter(Boolean).join("\n")}
        >
          {i > 0 && <span className="text-gray-500"> &middot; </span>}
          <span className={item.granted ? "text-indigo-300" : "text-white"}>
            {item.text}
          </span>
        </span>
      ))}
    </div>
  );
});
