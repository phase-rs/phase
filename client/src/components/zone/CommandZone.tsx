import { useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { GameObject, PlayerId } from "../../adapter/types.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useGameStore } from "../../stores/gameStore.ts";

interface CommandZoneProps {
  playerId: PlayerId;
}

interface GroupedEmblem {
  description: string;
  count: number;
  representative: GameObject;
}

function descriptionOf(emblem: GameObject, fallback: string): string {
  return (
    (emblem.static_definitions as Array<{ description?: string }>)
      ?.map((sd) => sd.description)
      .filter(Boolean)
      .join("; ") || fallback
  );
}

/**
 * Renders emblems in the command zone as a compact horizontal strip.
 * Identical emblems are stacked with a count badge (CR 114).
 */
export function CommandZone({ playerId }: CommandZoneProps) {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);

  const groups = useMemo(() => {
    if (!gameState) return [];

    const commandZoneIds = gameState.command_zone ?? [];
    const emblems: GameObject[] = commandZoneIds
      .map((id) => gameState.objects[id])
      .filter(
        (obj): obj is GameObject =>
          obj != null && obj.is_emblem === true && obj.controller === playerId,
      );

    // Group identical emblems by description
    const byDesc = new Map<string, GroupedEmblem>();
    for (const emblem of emblems) {
      const desc = descriptionOf(emblem, t("zone.emblemFallback"));
      const existing = byDesc.get(desc);
      if (existing) {
        existing.count++;
      } else {
        byDesc.set(desc, { description: desc, count: 1, representative: emblem });
      }
    }

    return [...byDesc.values()];
  }, [gameState, playerId, t]);

  if (groups.length === 0) return null;

  return (
    <div className="flex items-center gap-1.5">
      {groups.map((group) => (
        <EmblemCard key={group.representative.id} group={group} label={t("zone.emblem")} />
      ))}
    </div>
  );
}

/**
 * Renders an emblem as a card-shaped art-crop tile that visually matches the
 * adjacent permanents in the support row. Emblems carry no Scryfall art (the
 * engine names them all "Emblem" with no printed_ref, and the image pipeline
 * excludes the emblem layout), so the art area is a gold emblem seal showing
 * the granted-ability text rather than a real card image. Sized via the
 * shared --art-crop-w/h vars set by the support container's zoneStyle.
 */
function EmblemCard({ group, label }: { group: GroupedEmblem; label: string }) {
  const isCompactHeight = useIsCompactHeight();
  return (
    <div
      className="relative select-none drop-shadow-[0_4px_6px_rgba(0,0,0,0.6)]"
      style={{ width: "var(--art-crop-w)", height: "var(--art-crop-h)" }}
      title={group.description}
      data-testid="emblem-card"
    >
      {/* Outer black border */}
      <div className="absolute inset-0 rounded-[6px] border border-black bg-[#151515] p-[3px]">
        {/* Gold emblem frame */}
        <div className="relative flex h-full w-full flex-col overflow-hidden rounded-[3px] bg-gradient-to-b from-amber-600 via-amber-800 to-stone-950 shadow-[inset_0_1px_1px_rgba(255,255,255,0.3)]">
          {/* Header light reflection */}
          <div className="pointer-events-none absolute inset-x-0 top-0 z-10 h-[20px] bg-gradient-to-b from-white/30 to-transparent" />

          {/* Header */}
          <div
            className={`${isCompactHeight ? "h-[12px] px-1" : "h-[20px] px-1.5"} z-10 flex w-full shrink-0 items-center border-b border-black/40 shadow-[0_1px_2px_rgba(0,0,0,0.4)]`}
          >
            <span
              className={`${isCompactHeight ? "text-[8px]" : "text-[11.5px]"} mt-[1px] truncate font-extrabold uppercase leading-none tracking-wide text-[#2a1a05] drop-shadow-[0_1px_0_rgba(255,255,255,0.45)]`}
            >
              {label}
            </span>
          </div>

          {/* Art area: emblem seal + granted-ability text */}
          <div className="relative z-0 flex w-full flex-1 flex-col px-[2px] pb-[2px]">
            <div className="relative flex h-full w-full items-center justify-center overflow-hidden rounded-[1.5px] border border-black/80 bg-gradient-to-br from-stone-800 via-stone-900 to-black shadow-[inset_0_1px_3px_rgba(0,0,0,0.6)]">
              <span
                aria-hidden="true"
                className="absolute font-black leading-none text-amber-500/20"
                style={{ fontSize: "calc(var(--art-crop-h) * 0.55)" }}
              >
                ✦
              </span>
              <p
                className={`relative z-10 px-1 text-center leading-tight text-amber-100/90 drop-shadow-[0_1px_1px_rgba(0,0,0,0.9)] ${
                  isCompactHeight ? "line-clamp-2 text-[6.5px]" : "line-clamp-4 text-[8px]"
                }`}
              >
                {group.description}
              </p>
            </div>
          </div>
        </div>
      </div>

      {/* Count badge (CR 114: identical emblems stacked) */}
      {group.count > 1 && (
        <div
          className={`absolute -bottom-[3px] -right-[3px] z-20 inline-flex items-center justify-center rounded-full border border-black/80 bg-amber-600 px-1 font-bold text-black shadow-[0_2px_4px_rgba(0,0,0,0.8)] ${
            isCompactHeight ? "h-3.5 min-w-3.5 text-[8px]" : "h-5 min-w-5 text-[10px]"
          }`}
        >
          ×{group.count}
        </div>
      )}
    </div>
  );
}
