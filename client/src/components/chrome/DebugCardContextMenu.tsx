import { useEffect, useRef, useState } from "react";

import type { CounterType, DebugAction, Keyword, ObjectId, Zone } from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";
import { useGameDispatch } from "../../hooks/useGameDispatch";

const ZONES: readonly Zone[] = [
  "Battlefield",
  "Hand",
  "Graveyard",
  "Exile",
  "Library",
  "Command",
] as const;

const COMMON_KEYWORDS: readonly Keyword[] = [
  "Flying",
  "Trample",
  "Haste",
  "Lifelink",
  "Deathtouch",
  "Vigilance",
  "FirstStrike",
  "DoubleStrike",
  "Hexproof",
  "Indestructible",
  "Menace",
  "Reach",
  "Flash",
  "Defender",
];

export function DebugCardContextMenu() {
  const menu = useUiStore((s) => s.debugContextMenu);
  const closeMenu = useUiStore((s) => s.closeDebugContextMenu);

  if (!menu) return null;

  return <DebugCardContextMenuInner objectId={menu.objectId} x={menu.x} y={menu.y} onClose={closeMenu} />;
}

function DebugCardContextMenuInner({
  objectId,
  x,
  y,
  onClose,
}: {
  objectId: ObjectId;
  x: number;
  y: number;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  const players = useGameStore((s) => s.gameState?.players);
  const dispatch = useGameDispatch();

  const anchorBottom = y > window.innerHeight / 2;
  const left = Math.max(8, Math.min(x, window.innerWidth - 232));
  const maxHeight = anchorBottom ? y - 8 : window.innerHeight - y - 8;

  useEffect(() => {
    const handlePointerDown = (e: PointerEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("pointerdown", handlePointerDown, true);
    window.addEventListener("keydown", handleKey);
    window.addEventListener("blur", onClose);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown, true);
      window.removeEventListener("keydown", handleKey);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("resize", onClose);
    };
  }, [onClose]);

  const dispatchDebug = async (action: DebugAction) => {
    await dispatch({ type: "Debug", data: action });
    onClose();
  };

  const dispatchDebugKeepOpen = async (action: DebugAction) => {
    await dispatch({ type: "Debug", data: action });
  };

  if (!obj) return null;

  const onBattlefield = obj.zone === "Battlefield";
  const isCreature = obj.card_types?.core_types?.includes("Creature") ?? false;
  const isPlaneswalker = obj.card_types?.core_types?.includes("Planeswalker") ?? false;
  const isClass = obj.card_types?.subtypes?.includes("Class") ?? false;
  const isSaga = obj.card_types?.subtypes?.includes("Saga") ?? false;
  const hasLoreCounters = isClass || isSaga;
  const hasSummoningSickness = obj.has_summoning_sickness ?? false;
  const currentKeywords = obj.keywords ?? [];

  return (
    <div
      ref={ref}
      role="menu"
      className="fixed z-[120] w-56 overflow-y-auto rounded-lg border border-gray-700 bg-gray-900/95 py-1 shadow-xl backdrop-blur-sm"
      style={{
        left,
        maxHeight,
        ...(anchorBottom
          ? { bottom: window.innerHeight - y }
          : { top: y }),
      }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {/* Card name header */}
      <div className="truncate border-b border-gray-800 px-3 py-1.5 font-mono text-xs font-semibold text-gray-300">
        {obj.name || `Object ${objectId}`}
        {obj.class_level != null && (
          <span className="ml-1 text-amber-400">Lv.{obj.class_level}</span>
        )}
      </div>

      {/* Zone submenu */}
      <div className="border-b border-gray-800 py-0.5">
        <ZoneSubmenu
          currentZone={obj.zone}
          onSelectZone={(zone) =>
            dispatchDebug({ type: "MoveToZone", data: { object_id: objectId, to_zone: zone } })
          }
        />
      </div>

      {/* Battlefield-specific state toggles */}
      {onBattlefield && (
        <div className="border-b border-gray-800 py-0.5">
          <MenuItem
            label={obj.tapped ? "Untap" : "Tap"}
            onClick={() => dispatchDebug({ type: "SetTapped", data: { object_id: objectId, tapped: !obj.tapped } })}
          />
          {isCreature && (
            <MenuItem
              label={hasSummoningSickness ? "Remove Summoning Sickness" : "Give Summoning Sickness"}
              onClick={() =>
                dispatchDebug({ type: "SetSummoningSickness", data: { object_id: objectId, sick: !hasSummoningSickness } })
              }
            />
          )}
          {/* Transform / Flip / Face Down */}
          <MenuItem
            label={obj.transformed ? "Un-transform" : "Transform"}
            onClick={() =>
              dispatchDebug({ type: "SetFaceState", data: { object_id: objectId, transformed: !obj.transformed } })
            }
          />
          <MenuItem
            label={obj.face_down ? "Turn Face Up" : "Turn Face Down"}
            onClick={() =>
              dispatchDebug({ type: "SetFaceState", data: { object_id: objectId, face_down: !obj.face_down } })
            }
          />
          {(players?.length ?? 0) > 1 && (
            <ControllerSubmenu objectId={objectId} currentController={obj.controller} players={players!} onDispatch={dispatchDebug} />
          )}
        </div>
      )}

      {/* P/T for creatures on battlefield */}
      {onBattlefield && isCreature && (
        <div className="border-b border-gray-800 py-0.5">
          <PowerToughnessInput
            currentPower={obj.base_power}
            currentToughness={obj.base_toughness}
            onSet={(p, t) =>
              dispatchDebug({ type: "SetBasePowerToughness", data: { object_id: objectId, power: p, toughness: t } })
            }
          />
        </div>
      )}

      {/* Counter actions */}
      {onBattlefield && (
        <div className="border-b border-gray-800 py-0.5">
          {isCreature && (
            <>
              <CounterRow label="+1/+1" objectId={objectId} counterType="p1p1" current={obj.counters?.p1p1 ?? 0} onDispatch={dispatchDebugKeepOpen} />
              <CounterRow label="-1/-1" objectId={objectId} counterType="m1m1" current={obj.counters?.m1m1 ?? 0} onDispatch={dispatchDebugKeepOpen} />
            </>
          )}
          {isPlaneswalker && (
            <CounterRow label="Loyalty" objectId={objectId} counterType="loyalty" current={obj.counters?.loyalty ?? 0} onDispatch={dispatchDebugKeepOpen} />
          )}
          {hasLoreCounters && (
            <CounterRow label="Lore" objectId={objectId} counterType="lore" current={obj.counters?.lore ?? 0} onDispatch={dispatchDebugKeepOpen} />
          )}
        </div>
      )}

      {/* Keywords */}
      {onBattlefield && (
        <div className="border-b border-gray-800 py-0.5">
          <KeywordSubmenu
            objectId={objectId}
            currentKeywords={currentKeywords}
            onDispatch={dispatchDebugKeepOpen}
          />
        </div>
      )}

      {/* Destructive action */}
      <div className="py-0.5">
        <MenuItem
          label="Remove"
          danger
          onClick={() => dispatchDebug({ type: "RemoveObject", data: { object_id: objectId } })}
        />
      </div>
    </div>
  );
}

function MenuItem({
  label,
  onClick,
  danger,
  compact,
}: {
  label: string;
  onClick: () => void;
  danger?: boolean;
  compact?: boolean;
}) {
  return (
    <button
      role="menuitem"
      type="button"
      onClick={onClick}
      className={
        "flex w-full items-center px-3 text-left text-xs transition-colors " +
        (compact ? "py-1 " : "py-1.5 ") +
        (danger
          ? "text-red-400 hover:bg-red-900/30"
          : "text-gray-300 hover:bg-white/10")
      }
    >
      {label}
    </button>
  );
}

function ZoneSubmenu({
  currentZone,
  onSelectZone,
}: {
  currentZone: Zone;
  onSelectZone: (zone: Zone) => void;
}) {
  const [open, setOpen] = useState(false);

  return (
    <div className="relative">
      <button
        role="menuitem"
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center justify-between px-3 py-1.5 text-left text-xs text-gray-300 transition-colors hover:bg-white/10"
      >
        <span>Zone →</span>
        <span className="text-[10px] text-gray-600">{currentZone}</span>
      </button>
      {open && (
        <div className="ml-2 border-l border-gray-700">
          {ZONES.filter((z) => z !== currentZone).map((zone) => (
            <MenuItem key={zone} label={zone} onClick={() => onSelectZone(zone)} compact />
          ))}
        </div>
      )}
    </div>
  );
}

function ControllerSubmenu({
  objectId,
  currentController,
  players,
  onDispatch,
}: {
  objectId: ObjectId;
  currentController: number;
  players: { id: number }[];
  onDispatch: (action: DebugAction) => Promise<void>;
}) {
  const [open, setOpen] = useState(false);

  return (
    <div className="relative">
      <button
        role="menuitem"
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center justify-between px-3 py-1.5 text-left text-xs text-gray-300 transition-colors hover:bg-white/10"
      >
        <span>Controller →</span>
        <span className="text-[10px] text-gray-600">P{currentController}</span>
      </button>
      {open && (
        <div className="ml-2 border-l border-gray-700">
          {players
            .filter((p) => p.id !== currentController)
            .map((p) => (
              <MenuItem
                key={p.id}
                label={`Player ${p.id}`}
                onClick={() => onDispatch({ type: "SetController", data: { object_id: objectId, controller: p.id } })}
                compact
              />
            ))}
        </div>
      )}
    </div>
  );
}

function CounterRow({
  label,
  objectId,
  counterType,
  current,
  onDispatch,
}: {
  label: string;
  objectId: ObjectId;
  counterType: CounterType;
  current: number;
  onDispatch: (action: DebugAction) => Promise<void>;
}) {
  return (
    <div className="flex items-center justify-between px-3 py-1 text-xs text-gray-300">
      <span>{label}</span>
      <div className="flex items-center gap-1">
        <button
          type="button"
          onClick={() =>
            onDispatch({ type: "ModifyCounters", data: { object_id: objectId, counter_type: counterType, delta: -1 } })
          }
          className="rounded bg-gray-800 px-1.5 py-0.5 text-gray-400 transition-colors hover:bg-gray-700 hover:text-gray-200"
        >
          −
        </button>
        <span className="w-5 text-center font-mono text-amber-400">{current}</span>
        <button
          type="button"
          onClick={() =>
            onDispatch({ type: "ModifyCounters", data: { object_id: objectId, counter_type: counterType, delta: 1 } })
          }
          className="rounded bg-gray-800 px-1.5 py-0.5 text-gray-400 transition-colors hover:bg-gray-700 hover:text-gray-200"
        >
          +
        </button>
      </div>
    </div>
  );
}

function PowerToughnessInput({
  currentPower,
  currentToughness,
  onSet,
}: {
  currentPower: number | null;
  currentToughness: number | null;
  onSet: (power: number | null, toughness: number | null) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [power, setPower] = useState(String(currentPower ?? 0));
  const [toughness, setToughness] = useState(String(currentToughness ?? 0));

  if (!editing) {
    return (
      <button
        role="menuitem"
        type="button"
        onClick={() => setEditing(true)}
        className="flex w-full items-center justify-between px-3 py-1.5 text-left text-xs text-gray-300 transition-colors hover:bg-white/10"
      >
        <span>Set Base P/T</span>
        <span className="font-mono text-[10px] text-gray-600">
          {currentPower ?? "?"}/{currentToughness ?? "?"}
        </span>
      </button>
    );
  }

  return (
    <div className="flex items-center gap-1 px-3 py-1.5">
      <span className="text-xs text-gray-500">P/T:</span>
      <input
        type="number"
        value={power}
        onChange={(e) => setPower(e.target.value)}
        className="w-10 rounded border border-gray-700 bg-gray-800 px-1 py-0.5 text-center text-xs text-gray-200"
        autoFocus
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            onSet(parseInt(power) || 0, parseInt(toughness) || 0);
          }
        }}
      />
      <span className="text-xs text-gray-600">/</span>
      <input
        type="number"
        value={toughness}
        onChange={(e) => setToughness(e.target.value)}
        className="w-10 rounded border border-gray-700 bg-gray-800 px-1 py-0.5 text-center text-xs text-gray-200"
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            onSet(parseInt(power) || 0, parseInt(toughness) || 0);
          }
        }}
      />
      <button
        type="button"
        onClick={() => onSet(parseInt(power) || 0, parseInt(toughness) || 0)}
        className="rounded bg-cyan-800/50 px-1.5 py-0.5 text-[10px] text-cyan-300 transition-colors hover:bg-cyan-700/50"
      >
        Set
      </button>
    </div>
  );
}

function KeywordSubmenu({
  objectId,
  currentKeywords,
  onDispatch,
}: {
  objectId: ObjectId;
  currentKeywords: Keyword[];
  onDispatch: (action: DebugAction) => Promise<void>;
}) {
  const [open, setOpen] = useState(false);

  const stringKeywords = currentKeywords.filter((k): k is string => typeof k === "string");

  return (
    <div className="relative">
      <button
        role="menuitem"
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center justify-between px-3 py-1.5 text-left text-xs text-gray-300 transition-colors hover:bg-white/10"
      >
        <span>Keywords →</span>
        <span className="text-[10px] text-gray-600">{stringKeywords.length > 0 ? stringKeywords.length : ""}</span>
      </button>
      {open && (
        <div className="ml-2 max-h-48 overflow-y-auto border-l border-gray-700">
          {COMMON_KEYWORDS.map((kw) => {
            const kwStr = typeof kw === "string" ? kw : "";
            const hasKeyword = stringKeywords.includes(kwStr);
            return (
              <button
                key={kwStr}
                type="button"
                onClick={() =>
                  onDispatch(
                    hasKeyword
                      ? { type: "RemoveKeyword", data: { object_id: objectId, keyword: kw } }
                      : { type: "GrantKeyword", data: { object_id: objectId, keyword: kw } },
                  )
                }
                className={
                  "flex w-full items-center gap-2 px-3 py-1 text-left text-xs transition-colors hover:bg-white/10 " +
                  (hasKeyword ? "text-cyan-300" : "text-gray-400")
                }
              >
                <span className="w-3 text-center text-[10px]">{hasKeyword ? "✓" : ""}</span>
                <span>{kwStr}</span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
