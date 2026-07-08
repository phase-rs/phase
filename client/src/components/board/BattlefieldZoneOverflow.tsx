import type { CSSProperties, RefObject } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import type { GameObject, ManaColor, ObjectId } from "../../adapter/types.ts";
import { manaPipToDisplayColors } from "../card/cardFrame.ts";
import { ManaSymbol } from "../mana/ManaSymbol.tsx";
import { useIsCompactHeight } from "../../hooks/useIsCompactHeight.ts";
import { useIsMobile } from "../../hooks/useIsMobile.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { ZoneCollapseMode } from "../../stores/preferencesStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import type { GroupedPermanent } from "../../viewmodel/battlefieldProps.ts";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";
import { SelectField } from "../ui/SelectField.tsx";
import { useBoardInteractionState } from "./BoardInteractionContext.tsx";
import { BattlefieldRow } from "./BattlefieldRow.tsx";
import { ResizeHandle } from "../flexlayout/ResizeHandle.tsx";

type OverflowZone = "lands" | "support" | "creatures";
type DrawerSide = "left" | "right";

interface BattlefieldZoneOverflowProps {
  groups: GroupedPermanent[];
  zone: OverflowZone;
  side: DrawerSide;
  className?: string;
  dividerBeforeIndex?: number;
  /** Show the per-row collapse control (auto/on/off) on the summary tile. Only
   *  the local player's own lands/support rows opt in — the setting is a
   *  viewer-wide preference, so a control per opponent box would be redundant. */
  showCollapseControl?: boolean;
  /** Split multiplayer overview pane: the container is a third of the board
   *  width, so the summary tile wraps its mana pips into a squarish block
   *  instead of one long strip. Shape-only — collapse thresholds unchanged. */
  splitOverview?: boolean;
}

const ZONE_COLLAPSE_MODES: ZoneCollapseMode[] = ["auto", "on", "off"];

const MOBILE_COLLAPSE_GROUPS = 4;
const DESKTOP_COLLAPSE_GROUPS = 8;
// Split panes are a third of the board width, so loose lands/support crowd
// ~3× sooner than the full-width desktop threshold assumes. Creatures are
// exempt — they're the pane's primary content and size-to-fit instead.
const SPLIT_COLLAPSE_GROUPS = 4;
// Creatures own the full board width (lands/support each share a half-row), so
// they tolerate more cards before crowding. Identical tokens already stack into
// one group, so the creature threshold counts GROUPS (distinct stacks), not
// bodies — a lone 20-token Saproling swarm shouldn't trip the overflow.
const MOBILE_COLLAPSE_CREATURE_GROUPS = 6;
const DESKTOP_COLLAPSE_CREATURE_GROUPS = 12;
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
  showCollapseControl = false,
  splitOverview = false,
}: BattlefieldZoneOverflowProps) {
  const [open, setOpen] = useState(false);
  const panelRef = useRef<HTMLDivElement | null>(null);
  const isMobile = useIsMobile();
  const isCompactHeight = useIsCompactHeight();
  const isCreatures = zone === "creatures";
  const compact = isMobile || isCompactHeight;
  const threshold = isCreatures
    ? (compact ? MOBILE_COLLAPSE_CREATURE_GROUPS : DESKTOP_COLLAPSE_CREATURE_GROUPS)
    : splitOverview
      ? SPLIT_COLLAPSE_GROUPS
      : (compact ? MOBILE_COLLAPSE_GROUPS : DESKTOP_COLLAPSE_GROUPS);
  const objectIds = useMemo(() => groups.flatMap((group) => group.ids), [groups]);
  // Collapse by DISTINCT stack count, never body count: identical permanents
  // already render as one grouped tile, so 7 Forests + 2 duals reads as ~3
  // visible stacks — the space a body count of 9 implies is never actually
  // occupied. Creatures always worked this way (token swarms); lands/support
  // now match, so the crowding metric tracks what the player actually sees.
  const collapseMetric = groups.length;
  // Per-row user override (lands/support). Creatures have no preference, so they
  // always fall through to "auto" (the threshold compare below).
  const collapseLands = usePreferencesStore((s) => s.collapseLands);
  const collapseSupport = usePreferencesStore((s) => s.collapseSupport);
  const collapseMode: ZoneCollapseMode =
    zone === "lands" ? collapseLands : zone === "support" ? collapseSupport : "auto";
  const collapsed =
    collapseMode === "on"
      ? true
      : collapseMode === "off"
        ? false
        : collapseMetric > threshold;

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
        splitOverview={splitOverview}
      />
    );
  }

  return (
    <>
      {isCreatures ? (
        // Creatures fill the whole band: a scrollable grid of readable cards
        // (full P/T, keywords, counters, tapped state) rather than a summary
        // pill — combat-selected creatures float to the top for priority sight.
        <CreatureOverview
          groups={groups}
          objectIds={objectIds}
          onOpen={() => setOpen(true)}
        />
      ) : (
        <ZoneSummaryTile
          groups={groups}
          objectIds={objectIds}
          zone={zone}
          showCollapseControl={showCollapseControl}
          splitOverview={splitOverview}
          onOpen={() => setOpen(true)}
        />
      )}
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

// Fixed, readable card size for the scrollable creature grid. Big enough to
// read P/T, keywords, and counters; the parent scrolls the overflow.
const CREATURE_GRID_VARS: CSSProperties = {
  "--art-crop-w": "clamp(5.6rem, 4.4vw, 7.4rem)",
  "--art-crop-h": "clamp(4.2rem, 3.3vw, 5.55rem)",
  "--card-w": "clamp(3.85rem, 3.05vw, 5.15rem)",
  "--card-h": "clamp(5.4rem, 4.25vw, 7.2rem)",
} as CSSProperties;

interface CreatureOverviewProps {
  groups: GroupedPermanent[];
  objectIds: ObjectId[];
  onOpen: () => void;
}

/**
 * Crowded-creature view: a scrollable grid of full, readable cards (real board
 * cards via BattlefieldRow, so attack/block/activate/inspect all work inline).
 * Two affordances the design calls for:
 *  - Combat-selected creatures (chosen attackers, committed attackers, assigned
 *    blockers) sort to the FRONT so they're visible without scrolling.
 *  - Scrollability is made obvious with top/bottom fades, a bouncing chevron,
 *    and a header hint whenever there's more below the fold.
 */
function CreatureOverview({ groups, objectIds, onOpen }: CreatureOverviewProps) {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const { committedAttackerIds, incomingAttackerCounts } = useBoardInteractionState();
  const scrollRef = useRef<HTMLDivElement>(null);
  const [edges, setEdges] = useState({ top: false, bottom: false });

  const objects = useMemo(
    () => objectIds
      .map((id) => gameState?.objects[id])
      .filter((obj): obj is GameObject => obj != null),
    [gameState?.objects, objectIds],
  );

  // Float every combat participant to the front for priority visibility: chosen
  // and committed attackers, assigned blockers, and creatures with incoming
  // attacks (so a blocked/attacked creature is visible without scrolling). A
  // stable sort keeps everything else in its incoming order.
  const sortedGroups = useMemo(() => {
    const priority = new Set<ObjectId>([
      ...selectedAttackers,
      ...committedAttackerIds,
      ...blockerAssignments.keys(),
      ...incomingAttackerCounts.keys(),
    ]);
    if (priority.size === 0) return groups;
    const isPriority = (group: GroupedPermanent) => group.ids.some((id) => priority.has(id));
    return groups
      .map((group, index) => ({ group, index, priority: isPriority(group) }))
      .sort((a, b) => Number(b.priority) - Number(a.priority) || a.index - b.index)
      .map((entry) => entry.group);
  }, [groups, selectedAttackers, committedAttackerIds, blockerAssignments, incomingAttackerCounts]);

  const updateEdges = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    setEdges({
      top: el.scrollTop > 4,
      bottom: el.scrollTop + el.clientHeight < el.scrollHeight - 4,
    });
  }, []);

  useEffect(() => {
    updateEdges();
    const el = scrollRef.current;
    if (!el) return;
    const observer = new ResizeObserver(updateEdges);
    observer.observe(el);
    return () => observer.disconnect();
  }, [updateEdges, sortedGroups]);

  return (
    <div className="flex h-full w-full flex-col gap-1">
      <div className="flex shrink-0 flex-wrap items-center justify-center gap-2 px-2 pt-0.5">
        <span className="text-[11px] font-bold uppercase tracking-[0.18em] text-slate-200">
          {t("battlefieldOverflow.creatures.label")}
        </span>
        <span className="rounded-full bg-white/10 px-2 py-0.5 text-[11px] font-black tabular-nums text-white">
          {objectIds.length}
        </span>
        <CreatureSummary objects={objects} />
        {edges.bottom && (
          <span className="inline-flex items-center gap-1 rounded-full bg-cyan-400/15 px-2 py-0.5 text-[10px] font-bold uppercase tracking-[0.14em] text-cyan-200 ring-1 ring-cyan-300/40">
            <span aria-hidden className="animate-bounce">↓</span>
            {t("battlefieldOverflow.creatures.scrollHint")}
          </span>
        )}
        <button
          type="button"
          onClick={onOpen}
          aria-label={t("battlefieldOverflow.creatures.open", { count: objectIds.length })}
          className="rounded-md bg-white/10 px-2 py-0.5 text-[10px] font-bold uppercase tracking-[0.14em] text-slate-200 transition hover:bg-white/20 hover:text-white"
        >
          {t("battlefieldOverflow.creatures.viewAll")}
        </button>
      </div>
      {/* Floor the scroll region at one full card row (+ the pb-1 padding) so a
          cramped half-row never clips the first row of creatures; flex-1 still
          lets it grow to fill the available space. */}
      <div className="relative min-h-[calc(var(--card-h)_+_0.25rem)] flex-1" style={CREATURE_GRID_VARS}>
        <div
          aria-hidden
          className={`pointer-events-none absolute inset-x-0 top-0 z-10 h-5 bg-gradient-to-b from-slate-950 to-transparent transition-opacity ${edges.top ? "opacity-100" : "opacity-0"}`}
        />
        <div
          ref={scrollRef}
          onScroll={updateEdges}
          className="thin-scrollbar h-full overflow-y-auto overscroll-contain px-1 pb-1"
        >
          <BattlefieldRow groups={sortedGroups} rowType="creatures" fixedSize />
        </div>
        <div
          aria-hidden
          className={`pointer-events-none absolute inset-x-0 bottom-0 z-10 flex h-8 items-end justify-center bg-gradient-to-t from-slate-950 to-transparent text-base text-cyan-200 transition-opacity ${edges.bottom ? "opacity-100" : "opacity-0"}`}
        >
          <span className="animate-bounce">⌄</span>
        </div>
      </div>
    </div>
  );
}

interface ZoneSummaryTileProps {
  groups: GroupedPermanent[];
  objectIds: ObjectId[];
  zone: OverflowZone;
  showCollapseControl: boolean;
  splitOverview?: boolean;
  onOpen: () => void;
}

function ZoneSummaryTile({
  groups,
  objectIds,
  zone,
  showCollapseControl,
  splitOverview = false,
  onOpen,
}: ZoneSummaryTileProps) {
  const { t } = useTranslation("game");
  const gameState = useGameStore((s) => s.gameState);
  // Aspect-preserving size multiplier for the collapsed overflow pill (absent ⇒
  // 1). Anchored to the column's outer edge so it grows toward the central
  // corridor rather than off-screen: lands hug the left, support the right.
  const summaryScale = usePreferencesStore((s) => s.flexLayout.scales?.summaryTile) ?? 1;
  const flexEditMode = useUiStore((s) => s.flexEditMode);
  const isMobile = useIsMobile();
  // The collapsed overflow pill is more compact on mobile so it claims less of
  // the cramped half-row; desktop keeps the roomier footprint.
  const supportSizeClass = isMobile
    ? "min-h-[2.85rem] min-w-[7.25rem] px-1.5 py-1"
    : "min-h-[3.75rem] min-w-[9.25rem] px-2.5 py-1.5";
  const defaultSizeClass = isMobile
    ? "min-h-[2.25rem] min-w-[4.75rem] px-1.5 py-0.5"
    : "min-h-[3.25rem] min-w-[7.5rem] px-2 py-1.5";
  // Split panes cap the tile's width so the pip row wraps into a squarish
  // block (~2 pips per line) instead of one long strip eating the pane width.
  const splitSizeClass = "min-h-[3.25rem] min-w-[6.5rem] max-w-[9rem] px-2 py-1.5";
  const sizeClass = splitOverview
    ? splitSizeClass
    : zone === "support"
      ? supportSizeClass
      : defaultSizeClass;
  const selectedAttackers = useUiStore((s) => s.selectedAttackers);
  const blockerAssignments = useUiStore((s) => s.blockerAssignments);
  const selectedCardIds = useUiStore((s) => s.selectedCardIds);
  const combatMode = useUiStore((s) => s.combatMode);
  const {
    activatableObjectIds,
    boardChoiceObjectIds,
    committedAttackerIds,
    incomingAttackerCounts,
    manaTappableObjectIds,
    selectableSacrificeObjectIds,
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
    let boardChoice = 0;
    let sacrifice = 0;
    let selected = 0;
    let validAttackers = 0;
    let validTargets = 0;

    for (const id of objectIds) {
      if (activatableObjectIds.has(id)) activatable++;
      if (committedAttackerIds.has(id)) attacking++;
      incoming += incomingAttackerCounts.get(id) ?? 0;
      if (manaTappableObjectIds.has(id)) mana++;
      if (boardChoiceObjectIds.has(id)) boardChoice++;
      if (selectableSacrificeObjectIds.has(id)) sacrifice++;
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
      boardChoice,
      sacrifice,
      selected,
      validAttackers: combatMode === "attackers" ? validAttackers : 0,
      validTargets,
    };
  }, [
    activatableObjectIds,
    boardChoiceObjectIds,
    blockerAssignments,
    combatMode,
    committedAttackerIds,
    incomingAttackerCounts,
    manaTappableObjectIds,
    objectIds,
    selectableSacrificeObjectIds,
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
    // Per-color land counts split by availability. `total` counts every land
    // that can produce the color; `untapped` counts only those usable now.
    // CR 106.1: a tapped land can't produce mana, so `!tapped` is the
    // available-now signal (controller-agnostic — correct for own and
    // opponent boxes alike, unlike the viewer-scoped manaTappableObjectIds).
    const total = new Map<ManaColor | "Colorless", number>();
    const untapped = new Map<ManaColor | "Colorless", number>();
    for (const object of objects) {
      const identity = commanderIdentityByPlayer.get(object.controller);
      const usableNow = !object.tapped;
      for (const pip of object.available_mana_pips ?? []) {
        for (const color of manaPipToDisplayColors(pip, identity)) {
          const manaColor = color as ManaColor | "Colorless";
          total.set(manaColor, (total.get(manaColor) ?? 0) + 1);
          if (usableNow) untapped.set(manaColor, (untapped.get(manaColor) ?? 0) + 1);
        }
      }
    }
    return MANA_COLOR_ORDER
      .map((color) => ({
        color,
        total: total.get(color) ?? 0,
        untapped: untapped.get(color) ?? 0,
        shard: MANA_COLOR_SHARD[color],
      }))
      .filter((entry) => entry.total > 0);
  }, [gameState, objects, zone]);
  const hasInteraction =
    interaction.activatable > 0
    || interaction.attacking > 0
    || interaction.incoming > 0
    || interaction.mana > 0
    || interaction.boardChoice > 0
    || interaction.sacrifice > 0
    || interaction.selected > 0
    || interaction.validAttackers > 0
    || interaction.validTargets > 0;

  return (
    // Scale the wrapper (not the button) so the resize handle — a sibling of the
    // button — scales and moves WITH the tile. (transform is visual-only, so a
    // handle outside the scaled node would stay at the unscaled corner.)
    <span
      className={`relative inline-flex flex-col gap-0.5 ${zone === "support" ? "items-end" : "items-start"}`}
      style={
        summaryScale !== 1
          ? {
              transform: `scale(${summaryScale})`,
              transformOrigin: zone === "support" ? "right center" : "left center",
            }
          : undefined
      }
    >
      {showCollapseControl && (zone === "lands" || zone === "support") && (
        <ZoneCollapseControl zone={zone} />
      )}
      <button
        type="button"
        onClick={onOpen}
        data-grouped-ids={objectIds.join(" ")}
        className={`relative flex ${sizeClass} max-w-full flex-col justify-center rounded-lg border text-left shadow-[0_10px_24px_rgba(0,0,0,0.28)] backdrop-blur-md transition hover:border-white/30 hover:bg-slate-900/80 ${
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
        <span
          className={
            zone === "support"
              ? "mt-1 block w-full"
              : splitOverview
                ? "mt-1 flex flex-wrap items-center gap-1"
                : "mt-1 flex items-center gap-1"
          }
        >
          {zone === "lands" ? (
            manaOptions.length > 0 ? (
              manaOptions.map(({ color, total, untapped, shard }) => (
                <span
                  key={color}
                  className={`group relative inline-flex h-5 items-center gap-0.5 rounded-full bg-black/45 px-1.5 text-[10px] font-black tabular-nums ring-1 ring-white/12 ${
                    untapped === 0 ? "text-slate-400 opacity-60" : "text-slate-100"
                  }`}
                >
                  {/* untapped (available now) bright; tapped remainder as a dim
                      "/total" so the box reads "available of how many lands". */}
                  <span>{untapped}</span>
                  {untapped !== total ? (
                    <span className="font-bold text-slate-400">/{total}</span>
                  ) : null}
                  <span>×</span>
                  <ManaSymbol shard={shard} size="xs" className="drop-shadow-[0_1px_1px_rgba(0,0,0,0.65)]" />
                  <GameplayTooltip className="left-0 right-auto w-56">
                    <span className="inline-flex items-center gap-1.5">
                      <span>{t("battlefieldOverflow.lands.pipAvailability", { untapped, total })}</span>
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
      {/* Edit-mode corner grip scales the pill (anchored to its column edge). */}
      {flexEditMode && (
        <ResizeHandle scaleKey="summaryTile" corner={zone === "support" ? "bl" : "br"} />
      )}
    </span>
  );
}

/** Compact per-row collapse control (auto / always-on / always-off) shown above
 *  the summary tile on the local player's own lands/support rows. A native
 *  <select> keeps it touch-friendly and keyboard-accessible; it reads and writes
 *  the row's own `collapse{Lands,Support}` preference, so the tile and the
 *  Preferences-sheet segmented control stay in lockstep through one store value. */
function ZoneCollapseControl({ zone }: { zone: "lands" | "support" }) {
  const { t } = useTranslation("game");
  const value = usePreferencesStore((s) => (zone === "lands" ? s.collapseLands : s.collapseSupport));
  const setValue = usePreferencesStore((s) =>
    zone === "lands" ? s.setCollapseLands : s.setCollapseSupport,
  );
  return (
    <SelectField
      chevronSize="sm"
      aria-label={t("battlefieldOverflow.collapse.label")}
      title={t("battlefieldOverflow.collapse.label")}
      value={value}
      onChange={(event) => setValue(event.target.value as ZoneCollapseMode)}
      className="rounded-md border border-white/12 bg-slate-950/72 py-0.5 pl-1.5 text-[9px] font-semibold uppercase tracking-[0.12em] text-slate-300 backdrop-blur-md hover:border-white/25 hover:text-white"
    >
      {ZONE_COLLAPSE_MODES.map((mode) => (
        <option key={mode} value={mode}>
          {t(`battlefieldOverflow.collapse.${mode}`)}
        </option>
      ))}
    </SelectField>
  );
}

interface InteractionSummary {
  activatable: number;
  attacking: number;
  incoming: number;
  mana: number;
  boardChoice: number;
  sacrifice: number;
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
    interaction.sacrifice > 0
      ? { key: "sacrifice", label: t("battlefieldOverflow.badges.sacrifice"), tooltip: t("battlefieldOverflow.badgeTooltips.sacrifice"), count: interaction.sacrifice, className: "bg-red-500 text-white" }
      : null,
    interaction.boardChoice > interaction.sacrifice
      ? { key: "choice", label: t("battlefieldOverflow.badges.choice"), tooltip: t("battlefieldOverflow.badgeTooltips.choice"), count: interaction.boardChoice - interaction.sacrifice, className: "bg-sky-400 text-sky-950" }
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

interface SupportCountTone {
  chip: string;
  count: string;
  label: string;
}

const SUPPORT_COUNT_TONES: Record<keyof SupportTypeCounts, SupportCountTone> = {
  artifacts: {
    chip: "bg-sky-400/10 ring-sky-300/30",
    count: "text-sky-50",
    label: "text-sky-200",
  },
  enchantments: {
    chip: "bg-fuchsia-400/10 ring-fuchsia-300/30",
    count: "text-fuchsia-50",
    label: "text-fuchsia-200",
  },
  other: {
    chip: "bg-slate-400/10 ring-slate-300/25",
    count: "text-slate-50",
    label: "text-slate-200",
  },
  planeswalkers: {
    chip: "bg-amber-400/10 ring-amber-300/30",
    count: "text-amber-50",
    label: "text-amber-200",
  },
};

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

/** Aggregate power/toughness across the collapsed creatures, so the tile still
 *  conveys board presence at a glance. Null P/T (e.g. unset characteristic-
 *  defining values) counts as 0. */
function CreatureSummary({ objects }: { objects: GameObject[] }) {
  const { t } = useTranslation("game");
  const { power, toughness } = objects.reduce(
    (totals, object) => ({
      power: totals.power + (object.power ?? 0),
      toughness: totals.toughness + (object.toughness ?? 0),
    }),
    { power: 0, toughness: 0 },
  );

  return (
    <span className="group relative inline-flex h-5 items-center gap-1 rounded-full bg-black/45 px-2 text-[10px] font-black tabular-nums text-slate-100 ring-1 ring-white/12">
      <span aria-hidden>⚔</span>
      <span>{`${power}/${toughness}`}</span>
      <GameplayTooltip className="left-0 right-auto w-52">
        {t("battlefieldOverflow.creatures.totalPower", { power, toughness })}
      </GameplayTooltip>
    </span>
  );
}

function SupportCounts({ counts }: { counts: SupportTypeCounts }) {
  const { t } = useTranslation("game");
  const entries = [
    counts.artifacts > 0 ? {
      key: "artifacts",
      label: t("battlefieldOverflow.support.artifacts"),
      tooltip: t("battlefieldOverflow.supportTooltips.artifacts"),
      count: counts.artifacts,
      tone: SUPPORT_COUNT_TONES.artifacts,
    } : null,
    counts.enchantments > 0 ? {
      key: "enchantments",
      label: t("battlefieldOverflow.support.enchantments"),
      tooltip: t("battlefieldOverflow.supportTooltips.enchantments"),
      count: counts.enchantments,
      tone: SUPPORT_COUNT_TONES.enchantments,
    } : null,
    counts.planeswalkers > 0 ? {
      key: "planeswalkers",
      label: t("battlefieldOverflow.support.planeswalkers"),
      tooltip: t("battlefieldOverflow.supportTooltips.planeswalkers"),
      count: counts.planeswalkers,
      tone: SUPPORT_COUNT_TONES.planeswalkers,
    } : null,
    counts.other > 0 ? {
      key: "other",
      label: t("battlefieldOverflow.support.other"),
      tooltip: t("battlefieldOverflow.supportTooltips.other"),
      count: counts.other,
      tone: SUPPORT_COUNT_TONES.other,
    } : null,
  ].filter((entry): entry is {
    count: number;
    key: string;
    label: string;
    tone: SupportCountTone;
    tooltip: string;
  } => entry != null);

  return (
    <span className="flex w-full flex-wrap gap-1">
      {entries.map((entry) => (
        <span
          key={entry.key}
          className={`group relative inline-flex h-5 min-w-[3.3rem] items-center justify-between gap-1 rounded-full bg-black/45 px-1.5 text-[10px] font-black tabular-nums ring-1 ${entry.tone.chip}`}
        >
          <span className={entry.tone.count}>
            {entry.count}
          </span>
          <span className={`min-w-0 truncate ${entry.tone.label}`}>
            {entry.label}
          </span>
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
            className={`${zone === "lands" ? "justify-start" : zone === "creatures" ? "justify-center" : "justify-end"} ${className ?? ""}`}
          />
        </div>
      </div>
    </div>
  );
}
