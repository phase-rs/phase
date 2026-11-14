import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type { DraftCardInstance } from "../../adapter/draft-adapter";
import { BASIC_LAND_NAMES } from "../../constants/game";
import { fetchCardData } from "../../services/scryfall";
import { ManaSymbol } from "../mana/ManaSymbol";

const MANA_COLORS = ["W", "U", "B", "R", "G"] as const;
const TYPE_ROWS = [
  "land",
  "sorcery",
  "creature",
  "planeswalker",
  "artifact",
  "enchantment",
  "instant",
  "basicLand",
] as const;

const isLand = (typeLine: string) => /\bland\b/i.test(typeLine);
const isBasicLand = (typeLine: string) => /\bbasic\b/i.test(typeLine) && isLand(typeLine);

function countColorSymbols(manaCost: string, color: string): number {
  let count = 0;
  for (const match of manaCost.matchAll(/\{([^}]+)\}/g)) {
    if (match[1].split("/").includes(color)) count += 1;
  }
  return count;
}

/**
 * The builder's mana value is deliberately based on spells only. Lands do
 * not contribute to a deck's average mana cost, including virtual basics.
 */
export function AverageManaCost({ cards }: { cards: readonly DraftCardInstance[] }) {
  const { t } = useTranslation("draft");
  const averageManaValue = useMemo(() => {
    const nonlandCards = cards.filter((card) => !isLand(card.type_line));
    return nonlandCards.length === 0
      ? 0
      : nonlandCards.reduce((sum, card) => sum + card.cmc, 0) / nonlandCards.length;
  }, [cards]);

  return (
    <div className="text-center">
      <div className="text-[0.68rem] font-semibold uppercase tracking-[0.18em] text-slate-500">
        {t("limitedDeck.averageManaCost")}
      </div>
      <output className="mt-1 block text-2xl font-semibold tabular-nums text-white">
        {averageManaValue.toFixed(2)}
      </output>
    </div>
  );
}

export function DeckStatistics({
  cards,
  virtualCardNames,
}: {
  cards: readonly DraftCardInstance[];
  virtualCardNames: readonly string[];
}) {
  const { t } = useTranslation("draft");
  const [manaCosts, setManaCosts] = useState<Record<string, string>>({});
  const nonlandCards = useMemo(() => cards.filter((card) => !isLand(card.type_line)), [cards]);
  const cardNames = useMemo(
    () => [...new Set(nonlandCards.map((card) => card.name))],
    [nonlandCards],
  );

  useEffect(() => {
    let active = true;
    setManaCosts({});
    void Promise.all(cardNames.map(async (name) => {
      try {
        return [name, (await fetchCardData(name)).mana_cost] as const;
      } catch {
        return [name, ""] as const;
      }
    })).then((entries) => {
      if (active) setManaCosts(Object.fromEntries(entries));
    });
    return () => {
      active = false;
    };
  }, [cardNames]);

  const total = cards.length + virtualCardNames.length;
  const virtualBasicCount = virtualCardNames.filter((name) => BASIC_LAND_NAMES.has(name)).length;
  const typeCounts = TYPE_ROWS.map((type) => {
    if (type === "basicLand") {
      return { type, count: cards.filter((card) => isBasicLand(card.type_line)).length + virtualBasicCount };
    }
    if (type === "land") {
      return { type, count: cards.filter((card) => isLand(card.type_line)).length + virtualBasicCount };
    }
    return {
      type,
      count: cards.filter((card) => new RegExp(`\\b${type}\\b`, "i").test(card.type_line)).length,
    };
  });
  const colorCounts = MANA_COLORS.map((color) => ({
    color,
    count: nonlandCards.reduce(
      (sum, card) => sum + countColorSymbols(manaCosts[card.name] ?? "", color),
      0,
    ),
  }));

  return (
    <div className="flex flex-col gap-5">
      <AverageManaCost cards={cards} />

      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-white/10 text-left text-[0.68rem] uppercase tracking-[0.12em] text-slate-500">
            <th className="pb-2 font-semibold">{t("limitedDeck.manaColor")}</th>
            <th className="pb-2 text-right font-semibold">{t("limitedDeck.countInCost")}</th>
          </tr>
        </thead>
        <tbody>
          {colorCounts.map(({ color, count }) => (
            <tr key={color} className="border-b border-white/[0.06]">
              <th scope="row" className="py-1.5 text-left font-normal text-white/75">
                <span className="inline-flex items-center gap-2">
                  <ManaSymbol shard={color} size="xs" />
                  {color}
                </span>
              </th>
              <td className="py-1.5 text-right tabular-nums text-white">{count}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-white/10 text-left text-[0.68rem] uppercase tracking-[0.12em] text-slate-500">
            <th className="pb-2 font-semibold">{t("limitedDeck.type")}</th>
            <th className="pb-2 text-right font-semibold">{t("limitedDeck.count")}</th>
            <th className="pb-2 text-right font-semibold">{t("limitedDeck.percent")}</th>
          </tr>
        </thead>
        <tbody>
          {typeCounts.map(({ type, count }) => (
            <tr key={type} className="border-b border-white/[0.06]">
              <th scope="row" className="py-1.5 text-left font-normal text-white/75">
                {type === "basicLand" ? t("limitedDeck.basicLand") : t(`pool.groups.${type}`)}
              </th>
              <td className="py-1.5 text-right tabular-nums text-white">{count}</td>
              <td className="py-1.5 text-right tabular-nums text-white/60">
                {total === 0 ? "0%" : `${Math.round((count / total) * 100)}%`}
              </td>
            </tr>
          ))}
          <tr className="font-semibold text-white">
            <th scope="row" className="pt-2 text-left">{t("limitedDeck.total")}</th>
            <td className="pt-2 text-right tabular-nums">{total}</td>
            <td className="pt-2 text-right tabular-nums">{total === 0 ? "0%" : "100%"}</td>
          </tr>
        </tbody>
      </table>
    </div>
  );
}
