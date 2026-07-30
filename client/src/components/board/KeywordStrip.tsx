import { memo, useMemo } from "react";

import type { Keyword } from "../../adapter/types";
import {
  getKeywordDetail,
  getKeywordDisplayText,
  getKeywordIconClass,
  getKeywordName,
  getKeywordReminderText,
  isGrantedKeyword,
  sortKeywords,
} from "../../viewmodel/keywordProps";
import { ManaFontIcon } from "../icons/ManaFontIcon";

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
  /**
   * Square badge edge length as a CSS length. The column applies it as its own
   * `font-size`, so every inner dimension is an `em` multiple and the badges
   * scale with the host card. The parent picks it from the card's width var
   * (`--card-w` / `--art-crop-w`).
   */
  badgeSize: string;
  /** Cap on visible cells; the last becomes a "+N" overflow indicator. */
  maxVisible: number;
}

/**
 * Compact 2\u20133 char label for keywords with no mana-font glyph, so the fallback
 * still reads as a square badge rather than a wide text pill. Multi-word names
 * collapse to initials ("Start Your Engines" \u2192 "SYE"); single words keep their
 * first two letters.
 */
function keywordAbbrev(name: string): string {
  const words = name.split(/\s+/).filter(Boolean);
  if (words.length > 1) {
    return words
      .map((w) => w[0])
      .join("")
      .slice(0, 3)
      .toUpperCase();
  }
  return name.slice(0, 2).toUpperCase();
}

const BADGE_CLASS =
  "pointer-events-auto relative flex h-[1em] w-[1em] items-center justify-center rounded-[0.16em] bg-black/80 shadow-md ring-1";

export const KeywordStrip = memo(function KeywordStrip({
  keywords,
  baseKeywords,
  sourceByKeyword,
  badgeSize,
  maxVisible,
}: KeywordStripProps) {
  const sorted = useMemo(() => sortKeywords(keywords), [keywords]);

  const items = useMemo(
    () =>
      sorted.map((kw) => {
        const name = getKeywordName(kw);
        return {
          name,
          detail: getKeywordDetail(kw),
          text: getKeywordDisplayText(kw),
          iconClass: getKeywordIconClass(kw),
          granted: isGrantedKeyword(kw, baseKeywords),
          source: sourceByKeyword?.get(name),
          reminder: getKeywordReminderText(kw),
        };
      }),
    [sorted, baseKeywords, sourceByKeyword],
  );

  if (items.length === 0) return null;

  // MTGA-style square badges in a vertical column straddling the card's top-left
  // edge (rendered at the overflow-visible card level so the off-card half is
  // not clipped). Sizes are `em` multiples of `badgeSize` so the column scales
  // with the card. The column is capped at `maxVisible` cells so a keyword-heavy
  // creature can't run badges off the bottom \u2014 the last cell becomes a "+N"
  // overflow indicator listing the hidden keywords on hover.
  const overflowing = items.length > maxVisible;
  const shown = overflowing ? items.slice(0, maxVisible - 1) : items;
  const hidden = overflowing ? items.slice(maxVisible - 1) : [];

  return (
    <div
      className="pointer-events-none absolute left-0 top-[26%] z-30 flex -translate-x-[15%] flex-col gap-[0.18em]"
      style={{ fontSize: badgeSize }}
    >
      {shown.map((item, i) => {
        const title = [
          item.source ? `${item.text} \u2014 from ${item.source}` : item.text,
          item.reminder,
        ]
          .filter(Boolean)
          .join("\n");
        const ring = item.granted ? "ring-indigo-300/80" : "ring-white/25";
        const tint = item.granted ? "text-indigo-100" : "text-white";
        return (
          <span key={i} title={title} className={`${BADGE_CLASS} ${ring} ${tint}`}>
            {item.iconClass ? (
              <ManaFontIcon
                iconClass={item.iconClass}
                fallbackText={keywordAbbrev(item.name)}
                label={item.reminder ?? item.text}
                style={{ fontSize: "0.72em" }}
              />
            ) : (
              <span className="font-bold leading-none" style={{ fontSize: "0.46em" }}>
                {keywordAbbrev(item.name)}
              </span>
            )}
            {item.detail && (
              <span
                className="absolute -right-[0.12em] -bottom-[0.12em] rounded-[0.1em] bg-black px-[0.08em] font-bold leading-none ring-1 ring-white/20"
                style={{ fontSize: "0.4em" }}
              >
                {item.detail}
              </span>
            )}
          </span>
        );
      })}
      {hidden.length > 0 && (
        <span
          title={hidden.map((h) => h.text).join("\n")}
          className={`${BADGE_CLASS} text-white ring-white/30`}
        >
          <span className="font-bold leading-none" style={{ fontSize: "0.46em" }}>
            +{hidden.length}
          </span>
        </span>
      )}
    </div>
  );
});
