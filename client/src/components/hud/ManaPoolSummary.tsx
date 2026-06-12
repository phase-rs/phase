import { useTranslation } from "react-i18next";

import type {
  ManaRestriction,
  ManaSpellGrant,
  ManaType,
  ManaUnit,
} from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";

const EMPTY_MANA: ManaUnit[] = [];

const MANA_COLORS: Record<ManaType, string> = {
  White: "bg-amber-200 text-amber-950 ring-1 ring-amber-50/60 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Blue: "bg-blue-500/90 text-white ring-1 ring-blue-200/25 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Black: "bg-slate-700 text-slate-100 ring-1 ring-white/10 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Red: "bg-rose-500/90 text-white ring-1 ring-rose-200/25 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Green: "bg-emerald-600/90 text-white ring-1 ring-emerald-200/25 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
  Colorless: "bg-slate-300 text-slate-800 ring-1 ring-white/20 shadow-[0_0_0_1px_rgba(255,255,255,0.06)]",
};

const MANA_ORDER: ManaType[] = ["White", "Blue", "Black", "Red", "Green", "Colorless"];

// Discriminant key of a `ManaRestriction` — the bare string for unit variants,
// or the single object key for data variants. Keyed exhaustively below so
// TypeScript enforces completeness when a new restriction variant is added.
type ManaRestrictionTag =
  | "OnlyForSpellType"
  | "OnlyForCreatureType"
  | "OnlyForTypeSpellsOrAbilities"
  | "OnlyForSpellWithKeywordKind"
  | "OnlyForSpellWithKeywordKindFromZone"
  | "OnlyForActivation"
  | "OnlyForXCosts"
  | "ConvokePayment";

// i18n key (under the `manaPool` group) per restriction variant. Exhaustive
// `Record` — adding a `ManaRestriction` variant without a tooltip key here is a
// type error.
const RESTRICTION_LABEL_KEYS: Record<ManaRestrictionTag, string> = {
  OnlyForSpellType: "manaPool.onlyForSpellType",
  OnlyForCreatureType: "manaPool.onlyForCreatureType",
  OnlyForTypeSpellsOrAbilities: "manaPool.onlyForTypeSpellsOrAbilities",
  OnlyForSpellWithKeywordKind: "manaPool.onlyForSpellWithKeywordKind",
  OnlyForSpellWithKeywordKindFromZone: "manaPool.onlyForSpellWithKeywordKindFromZone",
  OnlyForActivation: "manaPool.onlyForActivation",
  OnlyForXCosts: "manaPool.onlyForXCosts",
  ConvokePayment: "manaPool.convokePayment",
};

function restrictionTag(restriction: ManaRestriction): ManaRestrictionTag {
  return (
    typeof restriction === "string"
      ? restriction
      : (Object.keys(restriction)[0] as ManaRestrictionTag)
  );
}

// Canonical, payload-inclusive string for a restriction — so "Legendary-only
// green" and "Creature-only green" hash to distinct group keys.
function canonRestriction(restriction: ManaRestriction): string {
  return typeof restriction === "string"
    ? restriction
    : JSON.stringify(restriction);
}

function canonGrant(grant: ManaSpellGrant): string {
  return typeof grant === "string" ? grant : JSON.stringify(grant);
}

interface ManaGroup {
  color: ManaType;
  restrictions: ManaRestriction[];
  grants: ManaSpellGrant[];
  count: number;
}

interface ManaPoolSummaryProps {
  playerId: number;
  size?: "default" | "sm";
}

export function ManaPoolSummary({ playerId, size = "default" }: ManaPoolSummaryProps) {
  const { t } = useTranslation("game");
  const manaUnits = useGameStore(
    (s) => s.gameState?.players[playerId]?.mana_pool.mana ?? EMPTY_MANA,
  );

  // Group by a deterministic composite key (color, restrictions, grants) so
  // distinctly-restricted mana of the same color renders as separate pills.
  const groups = new Map<string, ManaGroup>();
  for (const unit of manaUnits) {
    if (unit.restrictions.includes("ConvokePayment")) continue;
    const restrictions = unit.restrictions;
    const grants = unit.grants ?? [];
    const key = JSON.stringify([
      unit.color,
      [...restrictions].map(canonRestriction).sort(),
      [...grants].map(canonGrant).sort(),
    ]);
    const existing = groups.get(key);
    if (existing) {
      existing.count += 1;
    } else {
      groups.set(key, { color: unit.color, restrictions, grants, count: 1 });
    }
  }

  // Stable display order: by color, plain (unrestricted) groups before
  // restricted/granting ones of the same color.
  const entries = [...groups.values()].sort((a, b) => {
    const colorDelta =
      MANA_ORDER.indexOf(a.color) - MANA_ORDER.indexOf(b.color);
    if (colorDelta !== 0) return colorDelta;
    const aSpecial = a.restrictions.length > 0 || a.grants.length > 0 ? 1 : 0;
    const bSpecial = b.restrictions.length > 0 || b.grants.length > 0 ? 1 : 0;
    return aSpecial - bSpecial;
  });

  if (entries.length === 0) return null;

  return (
    <div className={`flex items-center ${size === "sm" ? "gap-0.5" : "gap-1"}`}>
      {entries.map((group, index) => {
        const special =
          group.restrictions.length > 0 || group.grants.length > 0;
        const tooltipParts = group.restrictions.map(
          (r) => t(RESTRICTION_LABEL_KEYS[restrictionTag(r)]),
        );
        if (group.grants.length > 0) tooltipParts.push(t("manaPool.grantsProperty"));
        const title = special ? tooltipParts.join("; ") : undefined;
        return (
          <span
            key={index}
            title={title}
            className={`relative inline-flex items-center justify-center rounded-full font-bold tabular-nums ${size === "sm" ? "h-5 min-w-5 px-1 text-[10px]" : "h-6 min-w-6 px-1.5 text-[11px]"} ${MANA_COLORS[group.color]} ${
              special ? "ring-2 ring-dashed ring-white/70" : ""
            }`}
          >
            {group.count}
            {special && (
              <span
                aria-hidden
                className="absolute -top-1 -right-1 h-2 w-2 rounded-full bg-white/90 ring-1 ring-slate-900/40"
              />
            )}
          </span>
        );
      })}
    </div>
  );
}
