import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type { GameObject, PlayerId } from "../../adapter/types.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { useResolvedCommandZoneDisplay } from "../../hooks/useResolvedCommandZoneDisplay.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import {
  type CommanderDamageEntry,
  commanderDamageEntriesFor,
  commandersInZone,
} from "../../viewmodel/commanderColumn.ts";
import { CommanderDamage } from "../board/CommanderDamage.tsx";
import { CommanderCardZone } from "./CommanderCardZone.tsx";
import { CommandZone } from "./CommandZone.tsx";

interface CommandDockProps {
  playerId: PlayerId;
  /** The focused-opponent area renders mirrored (anchored to the top of the
   *  screen), so the compact popover must open downward instead of upward. */
  isMirrored: boolean;
}

/** Card-size CSS vars the dock's children read (`CommanderCardZone` → --card-*,
 *  emblems → --art-crop-*). The dock establishes its own scale context because
 *  it lives outside PlayerArea's support-column `zoneStyle`. */
function dockStyle(scale: number): React.CSSProperties {
  return {
    "--art-crop-w": `calc(var(--art-crop-base) * var(--card-size-scale) * ${scale})`,
    "--art-crop-h": `calc(var(--art-crop-base) * var(--card-size-scale) * ${scale} * 0.85)`,
    "--card-w": `calc(var(--card-base) * var(--card-size-scale) * ${scale})`,
    "--card-h": `calc(var(--card-base) * var(--card-size-scale) * ${scale} * 1.4)`,
  } as React.CSSProperties;
}

const INLINE_SCALE = 0.68;
const POPOVER_SCALE = 0.82;

/**
 * Command zone (CR 408) rendered as a self-contained corner dock — commander
 * card(s) + tax, emblems (CR 114), and commander-damage badges — instead of
 * being interleaved into PlayerArea's battlefield support row. Two layouts,
 * resolved from the user's `commandZoneDisplay` preference:
 *  - **inline**: a bounded, always-visible vertical cluster.
 *  - **compact**: a collapsed pile (commander thumbnail + emblem/damage badges)
 *    that expands to a popover on hover/click.
 */
export function CommandDock({ playerId, isMirrored }: CommandDockProps) {
  const { t } = useTranslation("game");
  const mode = useResolvedCommandZoneDisplay();
  const gameState = useGameStore((s) => s.gameState);

  const commanders = useMemo(
    () => (gameState ? commandersInZone(gameState, playerId) : []),
    [gameState, playerId],
  );
  const damageEntries = useMemo(
    () => (gameState ? commanderDamageEntriesFor(gameState, playerId) : []),
    [gameState, playerId],
  );
  // Count this player's emblems for the compact badge + content gate. Mirrors
  // the filter in CommandZone (kept local rather than refactoring that
  // concurrently-edited component into a shared selector).
  const emblemCount = useMemo(() => {
    if (!gameState) return 0;
    return (gameState.command_zone ?? []).reduce((n, id) => {
      const obj = gameState.objects[id];
      return obj?.is_emblem === true && obj.controller === playerId ? n + 1 : n;
    }, 0);
  }, [gameState, playerId]);

  // Same content gate PlayerArea used for `hasSupportExtras` — render nothing
  // when the command zone is empty so it reserves no corner space.
  const hasContent = commanders.length > 0 || emblemCount > 0 || damageEntries.length > 0;
  if (!hasContent) return null;

  // The full cluster — rendered in exactly one place (inline body OR popover),
  // never both, so the interactive commander card is never duplicated.
  const fullContent = (
    <div className="flex flex-col items-end gap-1">
      <CommanderCardZone playerId={playerId} />
      <CommandZone playerId={playerId} />
      <CommanderDamage playerId={playerId} />
    </div>
  );

  if (mode === "inline") {
    return (
      <div
        className="flex max-w-[28vw] flex-col items-end gap-1"
        style={dockStyle(INLINE_SCALE)}
        data-debug-label="Command"
      >
        {fullContent}
      </div>
    );
  }

  return (
    <CompactCommandDock
      isMirrored={isMirrored}
      commanders={commanders}
      emblemCount={emblemCount}
      damageEntries={damageEntries}
      label={t("zone.commandZone")}
    >
      {fullContent}
    </CompactCommandDock>
  );
}

interface CompactCommandDockProps {
  isMirrored: boolean;
  commanders: GameObject[];
  emblemCount: number;
  damageEntries: CommanderDamageEntry[];
  label: string;
  children: React.ReactNode;
}

function CompactCommandDock({
  isMirrored,
  commanders,
  emblemCount,
  damageEntries,
  label,
  children,
}: CompactCommandDockProps) {
  const [open, setOpen] = useState(false);
  const firstCommander = commanders[0];
  const { src } = useCardImage(firstCommander?.name ?? "", { size: "normal" });
  const totalDamage = damageEntries.reduce(
    (sum, entry) => sum + entry.views.reduce((s, v) => s + v.damage, 0),
    0,
  );

  return (
    <div
      className="relative"
      onMouseEnter={() => setOpen(true)}
      onMouseLeave={() => setOpen(false)}
      data-debug-label="Command"
    >
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="relative flex h-12 w-12 items-center justify-center overflow-hidden rounded-lg border border-amber-400/60 bg-stone-900 shadow-md transition-transform hover:scale-105"
        title={label}
        aria-expanded={open}
      >
        {firstCommander && src ? (
          <img src={src} alt={firstCommander.name} className="h-full w-full object-cover" draggable={false} />
        ) : (
          <span aria-hidden className="text-2xl leading-none text-amber-500/80">✦</span>
        )}
        {commanders.length > 1 && (
          <span className="absolute left-0 top-0 rounded-br bg-amber-700 px-1 text-[9px] font-bold text-amber-100">
            ×{commanders.length}
          </span>
        )}
        {emblemCount > 0 && (
          <span className="absolute -bottom-1 -left-1 inline-flex h-4 min-w-4 items-center justify-center rounded-full border border-black/70 bg-amber-600 px-1 text-[9px] font-bold text-black shadow">
            ✦{emblemCount}
          </span>
        )}
        {totalDamage > 0 && (
          <span className="absolute -bottom-1 -right-1 inline-flex h-4 min-w-4 items-center justify-center rounded-full border border-black/70 bg-red-700 px-1 text-[9px] font-bold text-red-100 shadow">
            {totalDamage}
          </span>
        )}
      </button>
      {open && (
        <div
          className={`absolute right-0 z-50 rounded-lg border border-white/15 bg-black/85 p-2 shadow-xl backdrop-blur-md ${
            isMirrored ? "top-full mt-1" : "bottom-full mb-1"
          }`}
          style={dockStyle(POPOVER_SCALE)}
        >
          {children}
        </div>
      )}
    </div>
  );
}
