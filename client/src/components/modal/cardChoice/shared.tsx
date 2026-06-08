import type { TFunction } from "i18next";

import type { GameObject, ObjectId, WaitingFor } from "../../../adapter/types";
import { CancelButton } from "../ChoiceOverlay";

export const CHOICE_CARD_IMAGE_CLASS = "";

type SearchChoice = Extract<WaitingFor, { type: "SearchChoice" }>;

export function CostActionFooter({
  onCancel,
  children,
}: {
  onCancel: () => void;
  children: React.ReactNode;
}) {
  return (
    <div className="mx-auto flex w-full max-w-xl flex-col gap-2 sm:flex-row">
      <div className="flex-1">
        <CancelButton onClick={onCancel} />
      </div>
      <div className="flex-1">{children}</div>
    </div>
  );
}

export function canAssignDistinctCardTypes(
  objects: Record<ObjectId, GameObject | undefined>,
  selectedIds: ObjectId[],
  categories: string[],
): boolean {
  if (selectedIds.length === 0) return true;
  if (selectedIds.length > categories.length) return false;

  const cardOptions = selectedIds
    .map((id) => {
      const obj = objects[id];
      if (!obj) return null;
      return categories
        .map((category, index) =>
          obj.card_types.core_types.includes(category) ? index : -1,
        )
        .filter((index) => index >= 0);
    });

  if (cardOptions.some((options) => !options || options.length === 0)) {
    return false;
  }

  const sortedOptions = [...cardOptions]
    .filter((options): options is number[] => Array.isArray(options))
    .sort((a, b) => a.length - b.length);
  const used = new Array(categories.length).fill(false);

  const assign = (idx: number): boolean => {
    if (idx === sortedOptions.length) return true;
    for (const categoryIndex of sortedOptions[idx]) {
      if (used[categoryIndex]) continue;
      used[categoryIndex] = true;
      if (assign(idx + 1)) return true;
      used[categoryIndex] = false;
    }
    return false;
  };

  return assign(0);
}

export function searchChoiceSubtitle(data: SearchChoice["data"], t: TFunction<"game">): string {
  const constraint = data.constraint;
  const opts = { count: data.count };

  if (constraint?.type === "MatchEachFilter") {
    return data.up_to
      ? t("cardChoice.search.subtitleMatchUpTo", opts)
      : t("cardChoice.search.subtitleMatchExact", opts);
  }
  if (constraint?.type === "DistinctQualities") {
    return data.up_to
      ? t("cardChoice.search.subtitleDistinctUpTo", opts)
      : t("cardChoice.search.subtitleDistinctExact", opts);
  }
  if (constraint?.type === "TotalManaValue") {
    return data.up_to
      ? t("cardChoice.search.subtitleManaValueUpTo", opts)
      : t("cardChoice.search.subtitleManaValueExact", opts);
  }

  return data.up_to
    ? t("cardChoice.search.subtitleUpTo", opts)
    : t("cardChoice.search.subtitleExact", opts);
}

export type EffectZoneMode = "Sacrifice" | "Topdeck" | "Hand" | "Battlefield";

export const EFFECT_ZONE_VISUAL_CLASSES: Record<
  EffectZoneMode,
  { ring: string; overlay: string; badge: string }
> = {
  Sacrifice: {
    ring: "ring-red-400/80",
    overlay: "bg-red-500/20",
    badge: "bg-red-500/90",
  },
  Topdeck: {
    ring: "ring-sky-300/80",
    overlay: "bg-sky-500/20",
    badge: "bg-sky-500/90",
  },
  Hand: {
    ring: "ring-sky-300/80",
    overlay: "bg-sky-500/20",
    badge: "bg-sky-500/90",
  },
  Battlefield: {
    ring: "ring-emerald-400/80",
    overlay: "bg-emerald-500/20",
    badge: "bg-emerald-500/90",
  },
};

export const EFFECT_ZONE_ACTION_LABEL_KEYS: Record<EffectZoneMode, string> = {
  Sacrifice: "cardChoice.effectZone.labelConfirm",
  Topdeck: "cardChoice.effectZone.labelTop",
  Hand: "cardChoice.effectZone.labelReturn",
  Battlefield: "cardChoice.effectZone.labelPut",
};

export const EFFECT_ZONE_BADGE_KEYS: Record<EffectZoneMode, string> = {
  Sacrifice: "cardChoice.badges.sacrifice",
  Topdeck: "cardChoice.badges.put",
  Hand: "cardChoice.badges.return",
  Battlefield: "cardChoice.badges.put",
};
