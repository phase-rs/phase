import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";

import type { GameObject, PlayerId } from "../../adapter/types.ts";
import { dispatchAction } from "../../game/dispatch.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import {
  collectObjectActions,
  resolveSingleActionDispatch,
} from "../../viewmodel/cardActionChoice.ts";
import { RichLabel } from "../mana/RichLabel.tsx";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";

/** Emblem chips are deliberately rendered well below card size (Arena-style):
 *  they never compete with real permanents for board space. Scales the shared
 *  `--art-crop-w/h` vars set by the support container. */
const EMBLEM_CHIP_SCALE = 0.62;

interface CommandZoneProps {
  playerId: PlayerId;
}

interface GroupedEmblem {
  description: string;
  sourceName: string | null;
  count: number;
  representative: GameObject;
}

/** The emblem's granted rules text ("what it does"). The engine attaches it to
 *  the produced ability definition's `description` — a static for static
 *  emblems, the first trigger for triggered emblems, and the activated ability
 *  for activatable emblems (the Momir Basic `{X}, Discard a card: …` emblem,
 *  CR 114.4) — so pull from all three definition lists. Falls back to the
 *  generic "Emblem" label only when no text is available. */
function descriptionOf(emblem: GameObject, fallback: string): string {
  const descriptionsOf = (defs: unknown[] | undefined): string[] =>
    ((defs as Array<{ description?: string }> | undefined) ?? [])
      .map((def) => def.description)
      .filter((desc): desc is string => Boolean(desc));

  const parts = [
    ...descriptionsOf(emblem.static_definitions),
    ...descriptionsOf(emblem.trigger_definitions),
    ...descriptionsOf(emblem.abilities),
  ];
  return parts.join("; ") || fallback;
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

    // Group identical emblems by source + effect (CR 114). Keying on the source
    // as well as the effect keeps emblems from different planeswalkers visually
    // distinct even when their granted-ability text happens to coincide, so
    // each chip shows the correct source art.
    const byKey = new Map<string, GroupedEmblem>();
    for (const emblem of emblems) {
      const desc = descriptionOf(emblem, t("zone.emblemFallback"));
      const sourceName = emblem.emblem_source?.name ?? null;
      const key = `${sourceName ?? ""}|${desc}`;
      const existing = byKey.get(key);
      if (existing) {
        existing.count++;
      } else {
        byKey.set(key, { description: desc, sourceName, count: 1, representative: emblem });
      }
    }

    return [...byKey.values()];
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
 * Renders an emblem as a small Arena-style chip — deliberately well below card
 * size — that shows the source's art crop (the planeswalker/spell that created
 * it), an "Emblem" ribbon, and a stack count. An emblem has no art of its own
 * (CR 114.5: it is neither a card nor a permanent), so the art is resolved from
 * the engine-provided `emblem_source` provenance via the normal card-image
 * pipeline. When no source art is available, falls back to a gold emblem seal
 * showing the granted-ability text. The full source + effect text is always
 * available on hover.
 */
function EmblemCard({ group, label }: { group: GroupedEmblem; label: string }) {
  const isCompactHeight = useIsCompactHeight();
  const emblem = group.representative;
  const printedRef = emblem.emblem_source?.printed_ref ?? null;
  const { src: artSrc } = useCardImage(group.sourceName ?? "", {
    size: "art_crop",
    oracleId: printedRef?.oracle_id,
    faceName: printedRef?.face_name,
  });

  // CR 114.4 + CR 602.1: an emblem can carry an activated ability (the Momir
  // Basic `{X}, Discard a card: …` emblem). The engine maps each legal
  // `ActivateAbility` to its source via `GameAction::source_object()` and
  // surfaces it in `legalActionsByObject` only when activation is legal now
  // (sorcery speed, the controller's priority, once each turn). The chip is
  // therefore clickable exactly when the engine reports a live action for it —
  // no client-side legality inference. Static/triggered emblems never report
  // actions here, so they stay display-only as before.
  const legalActionsByObject = useGameStore((s) => s.legalActionsByObject);
  const setPendingAbilityChoice = useUiStore((s) => s.setPendingAbilityChoice);
  const emblemActions = useMemo(
    () => collectObjectActions(legalActionsByObject, emblem.id),
    [legalActionsByObject, emblem.id],
  );
  const isActivatable = emblemActions.length > 0;

  const handleActivate = useCallback(() => {
    if (emblemActions.length === 0) return;
    // Reuse the shared single-authority dispatch helper (issue #506): a
    // card-consuming ability surfaces the confirmation modal; otherwise the
    // lone action auto-fires, kicking off the engine's X / discard prompts.
    const auto = resolveSingleActionDispatch(emblemActions, emblem);
    if (auto) {
      dispatchAction(auto);
    } else {
      setPendingAbilityChoice({ objectId: emblem.id, actions: emblemActions });
    }
  }, [emblemActions, emblem, setPendingAbilityChoice]);

  return (
    <div
      // `hover:z-50` lifts the chip above later DOM siblings (the commander
      // column) within the support column so its tooltip paints on top.
      className={`group relative select-none drop-shadow-[0_3px_5px_rgba(0,0,0,0.6)] hover:z-50 ${
        isActivatable
          ? "cursor-pointer rounded-[6px] ring-1 ring-amber-300/70 hover:ring-2 hover:ring-amber-200"
          : ""
      }`}
      style={{
        width: `calc(var(--art-crop-w) * ${EMBLEM_CHIP_SCALE})`,
        height: `calc(var(--art-crop-h) * ${EMBLEM_CHIP_SCALE})`,
      }}
      data-testid="emblem-card"
      data-activatable={isActivatable || undefined}
      role={isActivatable ? "button" : undefined}
      tabIndex={isActivatable ? 0 : undefined}
      onClick={isActivatable ? handleActivate : undefined}
      onKeyDown={
        isActivatable
          ? (e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                handleActivate();
              }
            }
          : undefined
      }
    >
      {/* No-delay custom hover tooltip — the chip is too small to show "where
          it came from" and "what it does" inline. */}
      <GameplayTooltip>
        <span className="font-semibold text-amber-200">
          {label}
          {group.sourceName ? ` — ${group.sourceName}` : ""}
        </span>
        {/* Interpolate `{X}`/`{1}`/`{R}`/`{T}` etc. into Scryfall SVG symbols
            (RichLabel → ManaSymbol) so the emblem's `{X}, Discard a card: …`
            rules text reads like printed card text instead of raw braces. */}
        <RichLabel
          text={group.description}
          size="xs"
          className="mt-0.5 block text-slate-200"
        />
      </GameplayTooltip>
      {/* Outer black border + gold inlay so the chip reads as an emblem even
          over arbitrary source art. */}
      <div className="absolute inset-0 rounded-[5px] border border-black bg-[#151515] p-[2px]">
        <div className="relative h-full w-full overflow-hidden rounded-[3px] border border-amber-500/40 bg-gradient-to-b from-amber-700 via-amber-900 to-stone-950">
          {artSrc ? (
            <img
              src={artSrc}
              alt={group.sourceName ?? label}
              draggable={false}
              className="absolute inset-0 h-full w-full object-cover"
            />
          ) : (
            // Fallback: gold emblem seal + effect text when source art is absent.
            <div className="absolute inset-0 flex items-center justify-center">
              <span
                aria-hidden="true"
                className="absolute font-black leading-none text-amber-400/25"
                style={{ fontSize: "calc(var(--art-crop-h) * 0.4)" }}
              >
                ✦
              </span>
              <RichLabel
                text={group.description}
                size="xs"
                className={`relative z-10 px-1 text-center leading-tight text-amber-50/90 drop-shadow-[0_1px_1px_rgba(0,0,0,0.9)] ${
                  isCompactHeight ? "line-clamp-2 text-[6px]" : "line-clamp-3 text-[7px]"
                }`}
              />
            </div>
          )}

          {/* "Emblem" ribbon along the bottom — always visible so the chip is
              identifiable regardless of the underlying art. */}
          <div className="absolute inset-x-0 bottom-0 z-10 bg-gradient-to-t from-black/85 via-black/60 to-transparent px-1 pb-[2px] pt-[6px]">
            <span
              className={`flex items-center gap-[2px] font-extrabold uppercase leading-none tracking-wide text-amber-300 drop-shadow-[0_1px_1px_rgba(0,0,0,0.9)] ${
                isCompactHeight ? "text-[6px]" : "text-[7.5px]"
              }`}
            >
              <span aria-hidden="true">✦</span>
              {label}
            </span>
          </div>
        </div>
      </div>

      {/* Count badge (CR 114: identical emblems stacked) */}
      {group.count > 1 && (
        <div
          className={`absolute -bottom-[3px] -right-[3px] z-20 inline-flex items-center justify-center rounded-full border border-black/80 bg-amber-600 px-1 font-bold text-black shadow-[0_2px_4px_rgba(0,0,0,0.8)] ${
            isCompactHeight ? "h-3.5 min-w-3.5 text-[8px]" : "h-4 min-w-4 text-[9px]"
          }`}
        >
          ×{group.count}
        </div>
      )}
    </div>
  );
}
