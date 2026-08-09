import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { RingBenefitsPopover } from "./RingBenefitsPopover.tsx";
import { ManaFontIcon } from "../icons/ManaFontIcon.tsx";
import { GameplayTooltip } from "../ui/GameplayTooltip.tsx";
import type {
  DungeonId,
  FamilyCollapseState,
  NextSpellModifier,
  PendingNextSpellModifier,
  PendingSpellCostReduction,
  PlayerConditionKind,
  PlayerStatusView,
  ResourceAxis,
  ResourceAxisTag,
  UnboundedFamily,
} from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { useSpectatorMode } from "../../hooks/useSpectatorMode.ts";
import { getKeywordDisplayText } from "../../viewmodel/keywordProps.ts";

interface StatusBadgeProps {
  label: string;
  value?: number | string;
  tone?: "neutral" | "amber";
}

export function StatusBadge({
  label,
  value,
  tone = "neutral",
}: StatusBadgeProps) {
  return (
    <span
      className={`inline-flex items-center gap-1 rounded-full px-2 py-1 text-[10px] font-semibold tracking-[0.16em] uppercase ${
        tone === "amber"
          ? "bg-amber-400/16 text-amber-100 ring-1 ring-amber-300/30"
          : "bg-white/7 text-slate-200 ring-1 ring-white/10"
      }`}
    >
      <span>{label}</span>
      {value != null ? <span className="tabular-nums text-white">{value}</span> : null}
    </span>
  );
}

export function MonarchBadge() {
  const { t } = useTranslation("game");
  return (
    <BadgeTip text={t("badges.monarchTooltip")}>
      <span
        role="img"
        aria-label={t("badges.monarch")}
        className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[12px] leading-none ring-1 bg-amber-400 ring-amber-200/80 shadow-[0_0_14px_rgba(251,191,36,0.55)]"
      >
        <span
          aria-hidden
          className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.95)_0_10%,transparent_12%),radial-gradient(circle_at_68%_30%,rgba(254,243,199,0.95)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(180,83,9,0.7)_0_11%,transparent_13%),linear-gradient(135deg,#fffbeb_0%,#fcd34d_36%,#b45309_72%,#451a03_100%)]"
        />
        <span
          aria-hidden
          className="absolute -bottom-1 left-1/2 h-3 w-5 -translate-x-1/2 rounded-[45%] bg-amber-950/30 blur-[1px]"
        />
        <span className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.45)]">👑</span>
      </span>
    </BadgeTip>
  );
}

export function InitiativeBadge() {
  const { t } = useTranslation("game");
  return (
    <BadgeTip text={t("badges.initiativeTooltip")}>
      <span
        role="img"
        aria-label={t("badges.initiative")}
        className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[12px] leading-none ring-1 bg-cyan-500 ring-cyan-200/80 shadow-[0_0_14px_rgba(34,211,238,0.55)]"
      >
        <span
          aria-hidden
          className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.95)_0_10%,transparent_12%),radial-gradient(circle_at_68%_30%,rgba(207,250,254,0.95)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(14,116,144,0.7)_0_11%,transparent_13%),linear-gradient(135deg,#ecfeff_0%,#67e8f9_36%,#0e7490_72%,#083344_100%)]"
        />
        <span
          aria-hidden
          className="absolute -bottom-1 left-1/2 h-3 w-5 -translate-x-1/2 rounded-[45%] bg-cyan-950/30 blur-[1px]"
        />
        <span className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.5)]">🛡</span>
      </span>
    </BadgeTip>
  );
}

export function CityBlessingBadge() {
  const { t } = useTranslation("game");
  return (
    <BadgeTip text={t("badges.cityBlessingTooltip")}>
      <span
        role="img"
        aria-label={t("badges.cityBlessing")}
        className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[12px] leading-none ring-1 bg-yellow-400 ring-yellow-200/80 shadow-[0_0_14px_rgba(250,204,21,0.6)]"
      >
        <span
          aria-hidden
          className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_50%_40%,rgba(255,255,255,0.95)_0_18%,transparent_22%),radial-gradient(circle_at_50%_50%,rgba(254,240,138,0.85)_0_36%,transparent_42%),linear-gradient(135deg,#fefce8_0%,#fde047_36%,#ca8a04_72%,#422006_100%)]"
        />
        <span className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.4)]">☀</span>
      </span>
    </BadgeTip>
  );
}

export function EnduringStoryBadge() {
  const { t } = useTranslation("game");
  return (
    <BadgeTip text={t("badges.enduringStoryTooltip")}>
      <span
        role="img"
        aria-label={t("badges.enduringStory")}
        className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[12px] leading-none ring-1 bg-indigo-500 ring-indigo-200/80 shadow-[0_0_14px_rgba(99,102,241,0.55)]"
      >
        <span aria-hidden className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.45)]">📖</span>
      </span>
    </BadgeTip>
  );
}

interface DungeonBadgeProps {
  dungeonName: DungeonId;
  roomIndex: number;
}

const DUNGEON_DISPLAY_NAMES: Record<DungeonId, string> = {
  LostMineOfPhandelver: "Lost Mine",
  DungeonOfTheMadMage: "Mad Mage",
  TombOfAnnihilation: "Tomb",
  Undercity: "Undercity",
  BaldursGateWilderness: "Baldur's Gate",
};

export function DungeonBadge({ dungeonName, roomIndex }: DungeonBadgeProps) {
  const { t } = useTranslation("game");
  const display = DUNGEON_DISPLAY_NAMES[dungeonName];
  const room = roomIndex + 1;
  return (
    <BadgeTip text={t("badges.dungeonTooltip", { name: display, room })}>
      <span
        role="img"
        aria-label={t("badges.dungeonAriaLabel", { name: display, room })}
        className="relative inline-flex h-6 shrink-0 items-center gap-1 overflow-hidden rounded-full bg-violet-500/85 px-2 text-[10px] font-semibold uppercase tracking-[0.12em] text-violet-50 ring-1 ring-violet-300/70 shadow-[0_0_12px_rgba(139,92,246,0.45)]"
      >
        <span aria-hidden className="text-[12px] leading-none drop-shadow-[0_1px_1px_rgba(0,0,0,0.5)]">🏰</span>
        <span className="relative truncate">{display}</span>
        <span className="relative tabular-nums text-white">{room}</span>
      </span>
    </BadgeTip>
  );
}

type CounterBadgeKind = "poison" | "speed" | "rad" | "energy" | "ring" | "experience";

interface CounterBadgeProps {
  kind: CounterBadgeKind;
  value: number;
  ringBearerName?: string | null;
}

export function CounterBadge({ kind, value, ringBearerName }: CounterBadgeProps) {
  const { t } = useTranslation("game");
  if (kind === "poison") {
    return (
      <BadgeTip text={t("badges.poisonTooltip", { count: value })}>
        <span
          role="img"
          aria-label={t("badges.poisonAriaLabel", { count: value })}
          className={`relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[11px] font-black leading-none tabular-nums text-lime-950 ring-1 ${
            value >= 8
              ? "bg-lime-300 ring-lime-100 shadow-[0_0_16px_rgba(217,249,157,0.55)]"
              : "bg-lime-400 ring-lime-200/70 shadow-[0_0_12px_rgba(190,242,100,0.34)]"
          }`}
        >
          <span
            aria-hidden
            className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.9)_0_9%,transparent_11%),radial-gradient(circle_at_68%_30%,rgba(254,240,138,0.95)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(132,204,22,0.72)_0_11%,transparent_13%),linear-gradient(135deg,#f7fee7_0%,#bef264_36%,#65a30d_72%,#1a2e05_100%)]"
          />
          <span
            aria-hidden
            className="absolute -bottom-1 left-1/2 h-3 w-5 -translate-x-1/2 rounded-[45%] bg-lime-950/28 blur-[1px]"
          />
          <span className="relative inline-flex items-center gap-px">
            <span aria-hidden>☠</span>
            {value}
          </span>
        </span>
      </BadgeTip>
    );
  }

  if (kind === "energy") {
    return (
      <BadgeTip text={t("badges.energyTooltip", { count: value })}>
        <span
          role="img"
          aria-label={t("badges.energyAriaLabel", { count: value })}
          className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center gap-px overflow-hidden rounded-full px-1 text-[11px] font-black leading-none tabular-nums text-cyan-950 ring-1 bg-cyan-300 ring-cyan-100 shadow-[0_0_12px_rgba(103,232,249,0.5)]"
        >
          <span
            aria-hidden
            className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.95)_0_9%,transparent_11%),radial-gradient(circle_at_68%_30%,rgba(207,250,254,0.9)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(8,145,178,0.7)_0_11%,transparent_13%),linear-gradient(135deg,#ecfeff_0%,#67e8f9_36%,#0891b2_72%,#083344_100%)]"
          />
          <span className="relative inline-flex items-center gap-px">
            <ManaFontIcon iconClass="ms-energy" fallbackText="⚡" />
            {value}
          </span>
        </span>
      </BadgeTip>
    );
  }

  if (kind === "ring") {
    // Static fallback tooltip (opponents): all four levels as plain text. The
    // player's OWN ring gets the richer active-highlight popover via
    // <RingBenefitsBadge> instead.
    const ringTitle = [
      t("badges.ringTooltip", { level: value }),
      ringBearerName
        ? t("badges.ringBearerTooltip", { name: ringBearerName })
        : t("badges.noRingBearerTooltip"),
      t("badges.ringLevel1"),
      t("badges.ringLevel2"),
      t("badges.ringLevel3"),
      t("badges.ringLevel4"),
    ].join("\n");
    return (
      <RingChip
        value={value}
        ariaLabel={t("badges.ringAriaLabel", {
          level: value,
          bearer: ringBearerName ?? t("badges.noRingBearer"),
        })}
        title={ringTitle}
      />
    );
  }

  if (kind === "rad") {
    return (
      <BadgeTip text={t("badges.radTooltip", { count: value })}>
        <span
          role="img"
          aria-label={t("badges.radAriaLabel", { count: value })}
          className={`relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center gap-px overflow-hidden rounded-full px-1 text-[11px] font-black leading-none tabular-nums text-amber-950 ring-1 ${
            value >= 8
              ? "bg-amber-300 ring-amber-100 shadow-[0_0_16px_rgba(252,211,77,0.55)]"
              : "bg-amber-500 ring-amber-300/70 shadow-[0_0_12px_rgba(245,158,11,0.4)]"
          }`}
        >
          <span
            aria-hidden
            className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.85)_0_9%,transparent_11%),radial-gradient(circle_at_68%_30%,rgba(254,243,199,0.9)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(217,119,6,0.72)_0_11%,transparent_13%),linear-gradient(135deg,#fffbeb_0%,#fbbf24_36%,#b45309_72%,#451a03_100%)]"
          />
          <span
            aria-hidden
            className="absolute -bottom-1 left-1/2 h-3 w-5 -translate-x-1/2 rounded-[45%] bg-amber-950/28 blur-[1px]"
          />
          <span className="relative inline-flex items-center gap-px">
            <ManaFontIcon iconClass="ms-counter-rad" fallbackText="☢" />
            {value}
          </span>
        </span>
      </BadgeTip>
    );
  }

  if (kind === "experience") {
    // CR 122.1: Experience counters are player counters; surfaced so the player can
    // see their total without activating an ability that consumes them.
    return (
      <BadgeTip text={t("badges.experienceTooltip", { count: value })}>
        <span
          role="img"
          aria-label={t("badges.experienceAriaLabel", { count: value })}
          className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center gap-px overflow-hidden rounded-full px-1 text-[11px] font-black leading-none tabular-nums text-indigo-950 ring-1 bg-indigo-300 ring-indigo-100 shadow-[0_0_12px_rgba(165,180,252,0.5)]"
        >
          <span
            aria-hidden
            className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.95)_0_9%,transparent_11%),radial-gradient(circle_at_68%_30%,rgba(224,231,255,0.9)_0_7%,transparent_9%),radial-gradient(circle_at_38%_74%,rgba(79,70,229,0.7)_0_11%,transparent_13%),linear-gradient(135deg,#eef2ff_0%,#a5b4fc_36%,#4f46e5_72%,#1e1b4b_100%)]"
          />
          <span className="relative">✦{value}</span>
        </span>
      </BadgeTip>
    );
  }

  return (
    <BadgeTip text={t("badges.speedTooltip", { value })}>
      <span
        role="img"
        aria-label={t("badges.speedAriaLabel", { value })}
        className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-[6px] px-1 text-[11px] font-black leading-none tabular-nums text-white ring-1 ring-slate-100/60 shadow-[0_0_10px_rgba(226,232,240,0.22)]"
      >
        <span
          aria-hidden
          className="absolute inset-0 bg-[linear-gradient(90deg,rgba(15,23,42,0.82)_0_2px,transparent_2px),linear-gradient(45deg,#f8fafc_25%,#020617_25%,#020617_50%,#f8fafc_50%,#f8fafc_75%,#020617_75%,#020617_100%)] bg-[length:100%_100%,7px_7px]"
        />
        <span aria-hidden className="absolute inset-0 bg-cyan-300/10" />
        <span className="relative rounded-sm bg-black/62 px-0.5">{value}</span>
      </span>
    </BadgeTip>
  );
}

// CR 104.2b / 119.7 / 119.8 / 118.3 / 101.2 / 601.2a: glyph + i18n label-key per
// player-affecting condition. Exhaustive over PlayerConditionKind["type"] so a
// new engine variant forces an entry here.
const CONDITION_GLYPH: Record<PlayerConditionKind["type"], string> = {
  CantWin: "⛔",
  CantGainLife: "💔",
  CantLoseLife: "💚",
  CantPayLifeAsCost: "🩸",
  CantCastSpells: "🚫",
  CantActivateAbilities: "🛑",
  CastOnlyFromZones: "📤",
};

const CONDITION_LABEL_KEY: Record<PlayerConditionKind["type"], string> = {
  CantWin: "badges.condCantWin",
  CantGainLife: "badges.condCantGainLife",
  CantLoseLife: "badges.condCantLoseLife",
  CantPayLifeAsCost: "badges.condCantPayLife",
  CantCastSpells: "badges.condCantCast",
  CantActivateAbilities: "badges.condCantActivate",
  CastOnlyFromZones: "badges.condCastOnlyFromZones",
};

/**
 * Renders tooltip content in the project's custom <GameplayTooltip> rather than
 * a native `title`. The wrapper supplies the `group relative` hover context and
 * is deliberately NOT `overflow-hidden`, so the absolutely-positioned tooltip
 * escapes the badge's own rounded `overflow-hidden` clip. Newline-separated text
 * becomes stacked rows (native `title` line breaks have no inline-HTML analog).
 */
function BadgeTip({ text, children }: { text: string; children: ReactNode }) {
  const lines = text.split("\n");
  return (
    <span className="group relative inline-flex">
      {children}
      <GameplayTooltip>
        {lines.length === 1
          ? lines[0]
          : lines.map((line, i) => (
              <span key={i} className="block">
                {line}
              </span>
            ))}
      </GameplayTooltip>
    </span>
  );
}

/**
 * A single player-affecting condition (afflictions like "can't gain life" /
 * "can't cast spells"), rendered as a red glossy chip. Reads its source card
 * name (when the engine surfaced one) for the tooltip; never re-derives the
 * condition itself — `condition` comes straight from `DerivedViews.player_status`.
 */
export function ConditionBadge({ condition }: { condition: PlayerStatusView }) {
  const { t } = useTranslation("game");
  const sourceName = useGameStore((s) =>
    condition.source != null
      ? (s.gameState?.objects[String(condition.source)]?.name ?? null)
      : null,
  );
  const kind = condition.kind.type;
  const label = t(CONDITION_LABEL_KEY[kind]);
  const title = sourceName ? t("badges.condFromSource", { condition: label, name: sourceName }) : label;
  return (
    <BadgeTip text={title}>
      <span
        role="img"
        aria-label={title}
        className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[12px] leading-none ring-1 bg-red-500/85 ring-red-300/70 shadow-[0_0_12px_rgba(239,68,68,0.45)]"
      >
        <span
          aria-hidden
          className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.85)_0_9%,transparent_11%),linear-gradient(135deg,#fee2e2_0%,#ef4444_42%,#7f1d1d_100%)]"
        />
        <span className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.5)]">{CONDITION_GLYPH[kind]}</span>
      </span>
    </BadgeTip>
  );
}

// Externally-tagged `ResourceAxis`: unit variants are bare strings, data/tuple
// variants are single-key objects. The tag is the string itself or its only key.
const axisTag = (axis: ResourceAxis): ResourceAxisTag =>
  typeof axis === "string" ? axis : (Object.keys(axis)[0] as ResourceAxisTag);

// Exhaustive `Record<ResourceAxisTag, …>` so a new engine axis tag forces a
// compile-time update here (the TS drift guard). Pinned VALUE-BY-VALUE against the engine's
// `family_of` by `unbounded-family-tags.json`, which is why it is exported for that test.
export const UNBOUNDED_FAMILY_FOR_TEST: Record<ResourceAxisTag, UnboundedFamily> = {
  Mana: "mana",
  Life: "life",
  DamageDealt: "damage",
  LibraryDelta: "mill",
  Counter: "counters",
  Trigger: "triggers",
  TokensCreated: "tokens",
  CardsDrawn: "cards",
  Casts: "casts",
  LandfallTriggers: "triggers",
  CombatPhases: "combats",
  ExtraTurns: "turns",
  DeathTriggers: "triggers",
  EtbTriggers: "triggers",
  LtbTriggers: "triggers",
  SacTriggers: "triggers",
  Poison: "counters",
};

/** Map an engine-provided `ResourceAxis` to its display family. No STATE surface uses this — the
 *  engine publishes each seat's families on `unbounded_families`, and both the HUD badge and
 *  `ManaPoolSummary`'s mana marker read that channel. It survives for the ONE caller holding a
 *  bare axis list with no family channel to read: `LoopShortcutModal`'s PRE-accept offer badges,
 *  where nothing is marked unbounded yet so the engine has published nothing. This table is pinned
 *  tag-by-tag against the engine's `family_of` by the `unbounded-family-tags.json` golden, so the
 *  mirror cannot drift from the authority. */
export const familyOf = (axis: ResourceAxis): UnboundedFamily =>
  UNBOUNDED_FAMILY_FOR_TEST[axisTag(axis)];

const UNBOUNDED_FAMILY_GLYPH: Record<UnboundedFamily, string> = {
  mana: "💎",
  life: "❤",
  damage: "🔥",
  mill: "📚",
  counters: "🔢",
  tokens: "🪙",
  cards: "🃏",
  casts: "✦",
  combats: "⚔",
  turns: "⏳",
  triggers: "✴",
};

const UNBOUNDED_FAMILY_LABEL_KEY: Record<UnboundedFamily, string> = {
  mana: "badges.unboundedMana",
  life: "badges.unboundedLife",
  damage: "badges.unboundedDamage",
  mill: "badges.unboundedMill",
  counters: "badges.unboundedCounters",
  tokens: "badges.unboundedTokens",
  cards: "badges.unboundedCards",
  casts: "badges.unboundedCasts",
  combats: "badges.unboundedCombats",
  turns: "badges.unboundedTurns",
  triggers: "badges.unboundedTriggers",
};

/**
 * CR 732.2a: an `∞` badge for one unbounded-resource display family. Rendered
 * once per distinct family per player. The HUD callers pass the engine's
 * `unbounded_families` rows straight through — the engine decides which families
 * are present, on which HUD, and what each one may promise; `LoopShortcutModal`
 * still de-dups a bare axis list with a `Set` and passes no state. This badge only
 * formats the family to a glyph + label.
 */
// `state` is OPTIONAL and defaults to `Unscheduled` so a caller with no family row in hand renders
// today's bare-`∞` badge unchanged. `LoopShortcutModal`'s FamilyBadges is exactly that caller and
// stays that way on purpose: it renders at OFFER time, before any player has accepted, so nothing
// can be scheduled.
export function UnboundedBadge({
  family,
  state = { type: "Unscheduled" },
}: {
  family: UnboundedFamily;
  state?: FamilyCollapseState;
}) {
  const { t } = useTranslation("game");
  const resource = t(UNBOUNDED_FAMILY_LABEL_KEY[family]);
  // Both hooks are called UNCONDITIONALLY, before any branch — rules of hooks. No render site
  // changed: the badge resolves the viewer itself rather than taking it as a prop.
  const viewer = usePlayerId();
  const spectating = useSpectatorMode();
  // A `Committed` scheduled collapse is an accepted-but-unapplied bound, and N is named at the
  // next step/phase end by the loop's CONTROLLER — who is NOT necessarily the seat this badge sits
  // on: the row is keyed by the engine's attribution player, which for `Life`/`DamageDealt`/
  // `LibraryDelta`/`Poison` axes is the victim, and the badge also renders on opponent HUDs. The
  // engine now publishes that controller as `state.data.prompted`, so the copy can address the
  // seat that will actually be asked, and falls back to the passive voice for everyone else.
  // `Conditional` promises no bound at all, which is why it keeps its own copy in both voices.
  // The window itself is CR 732.2c's advance to the shortcut's ending point; this only reports
  // what the engine says is pending.
  //
  // `usePlayerId()` is the RAW seat, mirroring `useTurnStatus`'s documented rule — `prompted` is a
  // seat, so it compares against seat identity and NOT against `usePerspectivePlayerId()`, which
  // returns the seat whose turn is being controlled. TWO BOUNDS ARE DISCLOSED, not fixed here:
  //  (i) under a turn-control effect where viewer V controls seat A's turn and A is prompted, V
  //      personally answers the prompt but reads the third-person copy. Conservative by
  //      construction — the same fallback the `prompted === undefined` case takes, and never a
  //      false "you". Closing it needs the engine to publish "seat X may submit for seat Y",
  //      which is a turn-control question, not a CR 732 one.
  // (ii) `game::turns` raises ONE `PayAmountChoice` for the controller's WHOLE stash, so a
  //      tokens+counters+life collapse shows the second-person badge on THREE families for ONE
  //      joint count. The copy says "you'll name the count", which is true of every family that
  //      count collapses, rather than "you'll choose how many of these", which would not be.
  //
  // The spectator gate is REQUIRED, not defensive: `usePlayerId()` returns `PLAYER_ID` (0) in
  // spectate mode — never `SPECTATOR_PLAYER_ID` — so a loop prompted to seat 0 would otherwise
  // read "you'll name the count" to every spectator. `useSpectatorMode()`'s predicate is exactly
  // the union of `useCanActForWaitingState`'s two spectator gates, so this badge is never more
  // permissive than the submit authority it is describing.
  const you = !spectating && state.type === "Scheduled" && state.data.prompted === viewer;
  const title = ((): string => {
    switch (state.type) {
      case "Unscheduled":
        return t("badges.unboundedTooltip", { resource });
      case "Mixed":
        return t("badges.unboundedMixedTooltip", { resource });
      case "Scheduled":
        return state.data.certainty === "Committed"
          ? t(you ? "badges.unboundedScheduledYouTooltip" : "badges.unboundedScheduledTooltip", {
              resource,
            })
          : t(
              you ? "badges.unboundedConditionalYouTooltip" : "badges.unboundedConditionalTooltip",
              { resource },
            );
    }
  })();
  return (
    <BadgeTip text={title}>
      <span
        role="img"
        aria-label={title}
        className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center gap-0.5 overflow-hidden rounded-full px-1 text-[11px] font-black leading-none text-fuchsia-50 ring-1 bg-fuchsia-600/85 ring-fuchsia-300/70 shadow-[0_0_12px_rgba(217,70,239,0.5)]"
      >
        <span
          aria-hidden
          className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.85)_0_9%,transparent_11%),linear-gradient(135deg,#fae8ff_0%,#d946ef_42%,#701a75_100%)]"
        />
        {/* `∞` alone is a universal symbol, but the bounded form is frontend-authored copy — the
            "N" is a letter standing for "some number", which is language-dependent. Localized for
            the same reason its tooltip is. */}
        <span className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.5)]">
          {((): string => {
            switch (state.type) {
              // `Mixed` renders a BARE `∞`: part of this family has a pending collapse and part
              // does not, and one glyph cannot say two things, so it says the weaker true one.
              case "Unscheduled":
              case "Mixed":
                return "∞";
              // The GLYPH is not person-dependent: `∞→N` / `∞→?` says what will land, not who is
              // asked. Only the tooltip changes voice.
              case "Scheduled":
                return state.data.certainty === "Committed"
                  ? t("badges.unboundedScheduledGlyph")
                  : t("badges.unboundedConditionalGlyph");
            }
          })()}
        </span>
        <span aria-hidden className="relative text-[10px] leading-none drop-shadow-[0_1px_1px_rgba(0,0,0,0.5)]">
          {UNBOUNDED_FAMILY_GLYPH[family]}
        </span>
      </span>
    </BadgeTip>
  );
}

/** One human-readable line for a pending next-spell modifier. */
function describeNextSpellModifier(
  t: ReturnType<typeof useTranslation>["t"],
  modifier: NextSpellModifier,
): string {
  switch (modifier.type) {
    case "CantBeCountered":
      return t("badges.pendingCantBeCountered");
    case "HasKeyword":
      return t("badges.pendingHasKeyword", { keyword: getKeywordDisplayText(modifier.keyword) });
    case "CastAsThoughFlash":
      return t("badges.pendingFlash");
    case "WithoutPayingManaCost":
      return t("badges.pendingFree");
  }
}

/**
 * CR 601.2f: badge surfacing pending one-shot modifiers/reductions for the
 * player's next spell (copy, flash, can't-be-countered, cheaper, free). Glyph
 * + a multi-line tooltip enumerating each pending effect.
 */
export function PendingSpellBadge({
  modifiers,
  reductions,
}: {
  modifiers: PendingNextSpellModifier[];
  reductions: PendingSpellCostReduction[];
}) {
  const { t } = useTranslation("game");
  const lines = [
    ...modifiers.map((m) => describeNextSpellModifier(t, m.modifier)),
    ...reductions.map((r) => t("badges.pendingCheaper", { amount: r.amount })),
  ];
  const title = [t("badges.pendingSpell"), ...lines].join("\n");
  return (
    <BadgeTip text={title}>
      <span
        role="img"
        aria-label={t("badges.pendingSpell")}
        className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center overflow-hidden rounded-full px-1 text-[12px] leading-none ring-1 bg-indigo-500/85 ring-indigo-300/70 shadow-[0_0_12px_rgba(99,102,241,0.5)]"
      >
        <span
          aria-hidden
          className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_30%_24%,rgba(255,255,255,0.9)_0_9%,transparent_11%),linear-gradient(135deg,#e0e7ff_0%,#6366f1_42%,#312e81_100%)]"
        />
        <span className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.5)]">✨</span>
      </span>
    </BadgeTip>
  );
}

// Shared ring-chip visual (gold disc + level number). Used by both the
// opponent `CounterBadge` ring branch (static tooltip) and `RingBenefitsBadge`
// (hover popover) so the chip styling lives in exactly one place.
function RingChip({
  value,
  ariaLabel,
  title,
  chipRef,
  onMouseEnter,
  onMouseLeave,
}: {
  value: number;
  ariaLabel: string;
  title?: string;
  chipRef?: React.Ref<HTMLSpanElement>;
  onMouseEnter?: () => void;
  onMouseLeave?: () => void;
}) {
  const chip = (
    <span
      ref={chipRef}
      role="img"
      aria-label={ariaLabel}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
      className="relative inline-flex h-6 min-w-6 shrink-0 items-center justify-center gap-px overflow-hidden rounded-full px-1 text-[11px] font-black leading-none tabular-nums text-amber-950 ring-1 bg-yellow-600 ring-yellow-300/70 shadow-[0_0_12px_rgba(202,138,4,0.55)]"
    >
      <span
        aria-hidden
        className="absolute inset-0 rounded-full bg-[radial-gradient(circle_at_50%_50%,transparent_0_30%,rgba(0,0,0,0.45)_32%,transparent_38%),linear-gradient(135deg,#fde68a_0%,#d97706_45%,#78350f_100%)]"
      />
      <span className="relative drop-shadow-[0_1px_1px_rgba(0,0,0,0.6)]">{value}</span>
    </span>
  );
  // Opponent ring badge: static multi-line tooltip. The player's own ring omits
  // `title` and gets the richer hover popover via <RingBenefitsBadge> instead.
  return title ? <BadgeTip text={title}>{chip}</BadgeTip> : chip;
}

// Brief dismiss delay smoothing cursor jitter on the chip edge (mirrors
// EnchantmentsBadge's HOVER_CLOSE_DELAY_MS).
const RING_HOVER_CLOSE_DELAY_MS = 80;

/**
 * The player's OWN Ring badge: the gold level chip plus a hover popover
 * (<RingBenefitsPopover>) listing the four CR 701.54 abilities with those
 * active at the current `level` highlighted. The ability TEXT comes from the
 * existing `badges.ringLevelN` i18n strings (the project's convention for
 * fixed rules text, like keyword reminder text); only the active/inactive
 * split is state-driven — a comparison against `level`, not game logic.
 */
export function RingBenefitsBadge({
  level,
  ringBearerName,
}: {
  level: number;
  ringBearerName: string | null;
}) {
  const { t } = useTranslation("game");
  const chipRef = useRef<HTMLSpanElement>(null);
  const [hoverOpen, setHoverOpen] = useState(false);
  const closeTimerRef = useRef<number | null>(null);

  const cancelClose = useCallback(() => {
    if (closeTimerRef.current != null) {
      window.clearTimeout(closeTimerRef.current);
      closeTimerRef.current = null;
    }
  }, []);
  const onEnter = useCallback(() => {
    cancelClose();
    setHoverOpen(true);
  }, [cancelClose]);
  const onLeave = useCallback(() => {
    cancelClose();
    closeTimerRef.current = window.setTimeout(() => {
      setHoverOpen(false);
      closeTimerRef.current = null;
    }, RING_HOVER_CLOSE_DELAY_MS);
  }, [cancelClose]);
  useEffect(() => () => cancelClose(), [cancelClose]);

  return (
    <>
      <RingChip
        value={level}
        chipRef={chipRef}
        ariaLabel={t("badges.ringAriaLabel", {
          level,
          bearer: ringBearerName ?? t("badges.noRingBearer"),
        })}
        onMouseEnter={onEnter}
        onMouseLeave={onLeave}
      />
      {hoverOpen && chipRef.current ? (
        <RingBenefitsPopover anchorEl={chipRef.current} level={level} bearerName={ringBearerName} />
      ) : null}
    </>
  );
}
