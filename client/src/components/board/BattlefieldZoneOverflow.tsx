import type { CSSProperties, RefObject } from "react";
import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import type { GameObject, ManaColor, ObjectId } from "../../adapter/types.ts";
import { manaPipToDisplayColors } from "../card/cardFrame.ts";
import { ManaSymbol } from "../mana/ManaSymbol.tsx";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useIsMobile } from "../../hooks/useIsMobile.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import type { GroupedPermanent } from "../../viewmodel/battlefieldProps.ts";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";
import { useBoardInteractionState } from "./BoardInteractionContext.tsx";
import { BattlefieldRow } from "./BattlefieldRow.tsx";

type OverflowZone = "lands" | "support";
type DrawerSide = "left" | "right";

interface BattlefieldZoneOverflowProps {
  groups: GroupedPermanent[];
  zone: OverflowZone;
  side: DrawerSide;
  className?: string;
  dividerBeforeIndex?: number;
}

const MOBILE_COLLAPSE_GROUPS = 4;
const DESKTOP_COLLAPSE_GROUPS = 8;
const MANA_COLOR_ORDER: Array<ManaColor | "Colorless"> = [
  "White",
  "Blue",
  "Black",
  "Red",
  "Green",
  "Colorless",
];

const MANA_COLOR_SHARD: Record<ManaColor | "Colorless", string> = {
  White: "W",
  Blue: "U",
  Black: "B",
  Red: "R",
  Green: "G",
  Colorless: "C",
};

export function BattlefieldZoneOverflow({
  groups,
  zone,
  side,
  className,
  dividerBeforeIndex,
}: BattlefieldZoneOverflowProps) {
  const [open, setOpen] = useState(false);
  const panelRef = useRef<HTMLDivElement | null>(null);
  const isMobile = useIsMobile();
  const isCompactHeight = useIsCompactHeight();
  const threshold = isMobile || isCompactHeight
    ? MOBILE_COLLAPSE_GROUPS
    : DESKTOP_COLLAPSE_GROUPS;
  const objectIds = useMemo(() => groups.flatMap((group) => group.ids), [groups]);
  const collapsed = objectIds.length > threshold;

  useEffect(() => {
    if (!open) return;

    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") setOpen(false);
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = previousOverflow;
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    panelRef.current?.focus();
  }, [open]);

  if (!collapsed) {
    return (
      <BattlefieldRow
        groups={groups}
        rowType={zone}
        dividerBeforeIndex={dividerBeforeIndex}
        className={className}
      />
    );
  }

  return (
    <>
      <ZoneSummaryTile
        groups={groups}
        objectIds={objectIds}
        zone={zone}
        onOpen={() => setOpen(true)}
      />
      {open && createPortal(
        <BattlefieldZoneDrawer
          panelRef={panelRef}
          groups={groups}
          zone={zone}
          side={side}
          className={className}
          dividerBeforeIndex={dividerBeforeIndex}
          onClose={() => setOpen(false)}
        />,
        document.body,
      )}
    </>
  );
}

interface ZoneSummaryTileProps {
  groups: GroupedPermanent[];
  objectIds: ObjectId[];
  zone: OverflowZone;
  onOpen: () => void;
}

function ZoneSummaryTile({ groups, objectIds, zone, onOpen }: ZoneSummaryTileProps) {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const selectedCardIds = useUiStore((s) => s.selectedCardIds);
  const combatMode = useUiStore((s) => s.combatMode);
  const {
    activatableObjectIds,
    committedAttackerIds,
    incomingAttackerCounts,
    manaTappableObjectIds,
    validAttackerIds,
    validTargetObjectIds,
  } = useBoardInteractionState();
  const idSet = useMemo(() => new Set(objectIds), [objectIds]);
  const objects = useMemo(
    () => objectIds
      .map((id) => gameState?.objects[id])
      .filter((obj): obj is GameObject => obj != null),
    [gameState?.objects, objectIds],
  );
  const cardCount = objectIds.length;
  const interaction = useMemo(() => {
    let activatable = 0;
    let attacking = 0;
    let incoming = 0;
    let mana = 0;
    let selected = 0;
    let validAttackers = 0;
    let validTargets = 0;

    for (const id of objectIds) {
      if (activatableObjectIds.has(id)) activatable++;
      if (committedAttackerIds.has(id)) attacking++;
      incoming += incomingAttackerCounts.get(id) ?? 0;
      if (manaTappableObjectIds.has(id)) mana++;
      if (validAttackerIds.has(id)) validAttackers++;
      if (validTargetObjectIds.has(id)) validTargets++;
      if (
        selectedCardIds.includes(id)
        || selectedAttackers.includes(id)
        || blockerAssignments.has(id)
      ) {
        selected++;
      }
    }

    return {
      activatable,
      attacking,
      incoming,
      mana,
      selected,
      validAttackers: combatMode === "attackers" ? validAttackers : 0,
      validTargets,
    };
  }, [
    activatableObjectIds,
    blockerAssignments,
    combatMode,
    committedAttackerIds,
    incomingAttackerCounts,
    manaTappableObjectIds,
    objectIds,
    selectedAttackers,
    selectedCardIds,
    validAttackerIds,
    validTargetObjectIds,
  ]);
  const supportCounts = useMemo(() => supportTypeCounts(objects), [objects]);
  const manaOptions = useMemo(() => {
    if (zone !== "lands" || !gameState) return [];
    const commanderIdentityByPlayer = new Map(
      gameState.players.map((player) => [player.id, player.commander_color_identity]),
    );
    const colorCounts = new Map<ManaColor | "Colorless", number>();
    for (const object of objects) {
      const identity = commanderIdentityByPlayer.get(object.controller);
      for (const pip of object.available_mana_pips ?? []) {
        for (const color of manaPipToDisplayColors(pip, identity)) {
          const manaColor = color as ManaColor | "Colorless";
          colorCounts.set(manaColor, (colorCounts.get(manaColor) ?? 0) + 1);
        }
      }
    }
    return MANA_COLOR_ORDER
      .map((color) => ({
        color,
        count: colorCounts.get(color) ?? 0,
        shard: MANA_COLOR_SHARD[color],
      }))
      .filter((entry) => entry.count > 0);
  }, [gameState, objects, zone]);
  const hasInteraction =
    interaction.activatable > 0
    || interaction.attacking > 0
    || interaction.incoming > 0
    || interaction.mana > 0
    || interaction.selected > 0
    || interaction.validAttackers > 0
    || interaction.validTargets > 0;

  return (
    <button
      type="button"
      onClick={onOpen}
      data-grouped-ids={objectIds.join(" ")}
      className={`relative flex min-h-[3.25rem] min-w-[7.5rem] max-w-full flex-col justify-center rounded-lg border px-2 py-1.5 text-left shadow-[0_10px_24px_rgba(0,0,0,0.28)] backdrop-blur-md transition hover:border-white/30 hover:bg-slate-900/80 ${
        hasInteraction
          ? "border-cyan-300/60 bg-cyan-950/45 ring-1 ring-cyan-300/40"
          : "border-white/12 bg-slate-950/72"
      }`}
      aria-label={t(`battlefieldOverflow.${zone}.open`, { count: cardCount })}
    >
      <span className="flex items-center justify-between gap-2">
        <span className="text-[10px] font-bold uppercase tracking-[0.16em] text-slate-200">
          {t(`battlefieldOverflow.${zone}.label`)}
        </span>
        <span className="rounded-full bg-white/10 px-1.5 py-0.5 text-[10px] font-black tabular-nums text-white">
          {cardCount}
        </span>
      </span>
      <span className="mt-1 flex items-center gap-1">
        {zone === "lands" ? (
          manaOptions.length > 0 ? (
            manaOptions.map(({ color, count, shard }) => (
              <span
                key={color}
                className="group relative inline-flex h-5 items-center gap-0.5 rounded-full bg-black/45 px-1.5 text-[10px] font-black tabular-nums text-slate-100 ring-1 ring-white/12"
              >
                <span>{count}×</span>
                <ManaSymbol shard={shard} size="xs" className="drop-shadow-[0_1px_1px_rgba(0,0,0,0.65)]" />
                <GameplayTooltip className="left-0 right-auto w-56">
                  <span className="inline-flex items-center gap-1.5">
                    <span>{t("battlefieldOverflow.lands.pipTooltip", { count })}</span>
                    <ManaSymbol shard={shard} size="sm" className="shrink-0" />
                  </span>
                </GameplayTooltip>
              </span>
            ))
          ) : (
            <span className="text-[11px] text-slate-400">
              {t("battlefieldOverflow.noAvailablePips")}
            </span>
          )
        ) : (
          <SupportCounts counts={supportCounts} />
        )}
      </span>
      <InteractionBadges interaction={interaction} />
      {idSet.size > 0 && (
        <span className="sr-only">
          {t("battlefieldOverflow.groupCount", { count: groups.length })}
        </span>
      )}
    </button>
  );
}

interface InteractionSummary {
  activatable: number;
  attacking: number;
  incoming: number;
  mana: number;
  selected: number;
  validAttackers: number;
  validTargets: number;
}

function InteractionBadges({ interaction }: { interaction: InteractionSummary }) {
  const { t } = useTranslation("game");
  const badges = [
    interaction.validTargets > 0
      ? { key: "target", label: t("battlefieldOverflow.badges.target"), tooltip: t("battlefieldOverflow.badgeTooltips.target"), count: interaction.validTargets, className: "bg-lime-300 text-lime-950" }
      : null,
    interaction.validAttackers > 0
      ? { key: "attack", label: t("battlefieldOverflow.badges.attack"), tooltip: t("battlefieldOverflow.badgeTooltips.attack"), count: interaction.validAttackers, className: "bg-orange-500 text-white" }
      : null,
    interaction.mana > 0
      ? { key: "mana", label: t("battlefieldOverflow.badges.mana"), tooltip: t("battlefieldOverflow.badgeTooltips.mana"), count: interaction.mana, className: "bg-cyan-400 text-cyan-950" }
      : null,
    interaction.activatable > 0
      ? { key: "activate", label: t("battlefieldOverflow.badges.activate"), tooltip: t("battlefieldOverflow.badgeTooltips.activate"), count: interaction.activatable, className: "bg-indigo-400 text-indigo-950" }
      : null,
    interaction.selected > 0
      ? { key: "selected", label: t("battlefieldOverflow.badges.selected"), tooltip: t("battlefieldOverflow.badgeTooltips.selected"), count: interaction.selected, className: "bg-white text-black" }
      : null,
    interaction.attacking > 0
      ? { key: "attacking", label: t("battlefieldOverflow.badges.attacking"), tooltip: t("battlefieldOverflow.badgeTooltips.attacking"), count: interaction.attacking, className: "bg-orange-600 text-white" }
      : null,
    interaction.incoming > 0
      ? { key: "incoming", label: t("battlefieldOverflow.badges.incoming"), tooltip: t("battlefieldOverflow.badgeTooltips.incoming"), count: interaction.incoming, className: "bg-red-600 text-white" }
      : null,
  ].filter((badge): badge is { key: string; label: string; tooltip: string; count: number; className: string } => badge != null);

  if (badges.length === 0) return null;

  return (
    <span className="mt-1 flex flex-wrap gap-1">
      {badges.slice(0, 3).map((badge) => (
        <span
          key={badge.key}
          className={`group relative rounded px-1 py-0.5 text-[9px] font-black uppercase leading-none ${badge.className}`}
        >
          {badge.label} {badge.count}
          <GameplayTooltip className="left-0 right-auto w-52">
            {badge.tooltip}
          </GameplayTooltip>
        </span>
      ))}
    </span>
  );
}

interface SupportTypeCounts {
  artifacts: number;
  enchantments: number;
  other: number;
  planeswalkers: number;
}

function supportTypeCounts(objects: GameObject[]): SupportTypeCounts {
  const counts: SupportTypeCounts = {
    artifacts: 0,
    enchantments: 0,
    other: 0,
    planeswalkers: 0,
  };

  for (const object of objects) {
    const types = object.card_types.core_types;
    if (types.includes("Planeswalker")) {
      counts.planeswalkers++;
    } else if (types.includes("Artifact")) {
      counts.artifacts++;
    } else if (types.includes("Enchantment")) {
      counts.enchantments++;
    } else {
      counts.other++;
    }
  }

  return counts;
}

function SupportCounts({ counts }: { counts: SupportTypeCounts }) {
  const { t } = useTranslation("game");
  const entries = [
    counts.artifacts > 0 ? {
      key: "artifacts",
      label: t("battlefieldOverflow.support.artifacts"),
      tooltip: t("battlefieldOverflow.supportTooltips.artifacts"),
      count: counts.artifacts,
    } : null,
    counts.enchantments > 0 ? {
      key: "enchantments",
      label: t("battlefieldOverflow.support.enchantments"),
      tooltip: t("battlefieldOverflow.supportTooltips.enchantments"),
      count: counts.enchantments,
    } : null,
    counts.planeswalkers > 0 ? {
      key: "planeswalkers",
      label: t("battlefieldOverflow.support.planeswalkers"),
      tooltip: t("battlefieldOverflow.supportTooltips.planeswalkers"),
      count: counts.planeswalkers,
    } : null,
    counts.other > 0 ? {
      key: "other",
      label: t("battlefieldOverflow.support.other"),
      tooltip: t("battlefieldOverflow.supportTooltips.other"),
      count: counts.other,
    } : null,
  ].filter((entry): entry is { key: string; label: string; tooltip: string; count: number } => entry != null);

  return (
    <span className="flex flex-wrap gap-1">
      {entries.map((entry) => (
        <span key={entry.key} className="group relative rounded bg-white/10 px-1.5 py-0.5 text-[10px] font-bold text-slate-100">
          {entry.label} {entry.count}
          <GameplayTooltip className="left-0 right-auto w-52">
            {entry.tooltip}
          </GameplayTooltip>
        </span>
      ))}
    </span>
  );
}

interface BattlefieldZoneDrawerProps {
  groups: GroupedPermanent[];
  zone: OverflowZone;
  side: DrawerSide;
  className?: string;
  dividerBeforeIndex?: number;
  onClose: () => void;
  panelRef: RefObject<HTMLDivElement | null>;
}

function BattlefieldZoneDrawer({
  groups,
  zone,
  side,
  className,
  dividerBeforeIndex,
  onClose,
  panelRef,
}: BattlefieldZoneDrawerProps) {
  const { t } = useTranslation("game");
  const objectCount = groups.reduce((total, group) => total + group.count, 0);

  return (
    <div className="fixed inset-0 z-[58] overscroll-contain">
      <button
        type="button"
        aria-label={t("battlefieldOverflow.close")}
        className="absolute inset-0 bg-black/45"
        onClick={onClose}
      />
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-label={t(`battlefieldOverflow.${zone}.title`, { count: objectCount })}
        tabIndex={-1}
        className={`absolute top-0 flex h-full w-[min(22rem,85vw)] flex-col border-white/10 bg-[#0b1020]/96 shadow-2xl backdrop-blur-md outline-none ${
          side === "left" ? "left-0 border-r" : "right-0 border-l"
        }`}
      >
        <div className="flex shrink-0 items-center justify-between gap-2 border-b border-white/10 px-3 py-2">
          <div className="min-w-0">
            <h2 className="truncate text-sm font-bold text-white">
              {t(`battlefieldOverflow.${zone}.title`, { count: objectCount })}
            </h2>
            <p className="text-[11px] text-slate-400">
              {t("battlefieldOverflow.groupCount", { count: groups.length })}
            </p>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="rounded-md px-2 py-1 text-xs font-semibold text-slate-300 hover:bg-white/10 hover:text-white"
          >
            {t("battlefieldOverflow.close")}
          </button>
        </div>
        <div
          className="thin-scrollbar min-h-0 flex-1 overflow-y-auto overscroll-contain p-3"
          style={{
            "--art-crop-w": "7rem",
            "--art-crop-h": "5.25rem",
            "--card-w": "5rem",
            "--card-h": "7rem",
          } as CSSProperties}
        >
          <BattlefieldRow
            groups={groups}
            rowType={zone}
            dividerBeforeIndex={dividerBeforeIndex}
            className={`${zone === "lands" ? "justify-start" : "justify-end"} ${className ?? ""}`}
          />
        </div>
      </div>
    </div>
  );
}
