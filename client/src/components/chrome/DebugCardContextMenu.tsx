import {
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";

import type {
  CounterType,
  DebugAction,
  Keyword,
  LibraryPosition,
  ObjectId,
  Zone,
} from "../../adapter/types";
import { useGameStore } from "../../stores/gameStore";
import {
  useUiStore,
  type DebugContextMenuSurface,
} from "../../stores/uiStore";
import { useGameDispatch } from "../../hooks/useGameDispatch";
import { useIsMobile } from "../../hooks/useIsMobile";
import { useFocusScopePortalBranch } from "../ui/FocusScope";

// How a submenu's panel is rendered: stacked inline below its button (mobile,
// where there is no horizontal room) or as a side flyout (tablet/desktop). The
// side is chosen per-open-menu so the flyout never runs off the screen edge.
type SubmenuFlyout = "inline" | "left" | "right";

// Only one submenu is open at a time (accordion). The parent owns which one so
// opening a second collapses the first.
type SubmenuName = "zone" | "controller" | "keywords";

// Side flyout panel width estimate, in px. Must match the `min-w-[12rem]` on the
// flyout panel below — used to decide which side has room and to keep the panel
// on-screen. (12rem = 192px.)
const FLYOUT_WIDTH = 192;

// A debug menu mixes traditional menu rows with compact command buttons (for
// counters, edited P/T, and keyword toggles). Keyboard traversal must include
// every command without crossing into an open child menu. Keeping selection in
// one helper also makes native `disabled` and ARIA-disabled filtering uniform.
const MENU_COMMAND_SELECTOR = [
  "button",
  '[role="menuitem"]',
  '[role="menuitemcheckbox"]',
  '[role="menuitemradio"]',
].join(", ");

function isEnabledCurrentMenuCommand(
  command: HTMLElement,
  menu: HTMLElement,
): boolean {
  return (
    command.closest<HTMLElement>('[role="menu"]') === menu &&
    !command.matches(':disabled, [aria-disabled="true"]')
  );
}

function currentMenuCommands(menu: HTMLElement): HTMLElement[] {
  return Array.from(
    menu.querySelectorAll<HTMLElement>(MENU_COMMAND_SELECTOR),
  ).filter((command) => isEnabledCurrentMenuCommand(command, menu));
}

function closestCurrentMenuCommand(
  target: HTMLElement,
  menu: HTMLElement,
): HTMLElement | null {
  const command = target.closest<HTMLElement>(MENU_COMMAND_SELECTOR);
  return command && isEnabledCurrentMenuCommand(command, menu) ? command : null;
}

// Everything a submenu needs to render its toggle row and panel, except its own
// label/badge/children. Bundled so the parent can hand it to every submenu with
// a single spread and the accordion/positioning logic lives in one place.
type SubmenuChrome = {
  flyout: SubmenuFlyout;
  anchorBottom: boolean;
  maxHeight: number;
  open: boolean;
  onToggle: () => void;
};

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

export function DebugCardContextMenu({
  surface,
  anchorRef,
}: {
  surface: DebugContextMenuSurface;
  anchorRef?: RefObject<HTMLElement | null>;
}) {
  const menu = useUiStore((s) => s.debugContextMenu);
  const closeMenu = useUiStore((s) => s.closeDebugContextMenu);

  useEffect(
    () => () => {
      if (useUiStore.getState().debugContextMenu?.surface === surface) {
        useUiStore.getState().closeDebugContextMenu();
      }
    },
    [surface],
  );

  if (!menu || menu.surface !== surface) return null;

  return (
    <DebugCardContextMenuInner
      objectId={menu.objectId}
      x={menu.x}
      y={menu.y}
      onClose={closeMenu}
      anchorRef={anchorRef}
    />
  );
}

function DebugCardContextMenuInner({
  objectId,
  x,
  y,
  onClose,
  anchorRef,
}: {
  objectId: ObjectId;
  x: number;
  y: number;
  onClose: () => void;
  anchorRef?: RefObject<HTMLElement | null>;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const fallbackAnchorRef = useRef<HTMLElement | null>(null);
  const liveObject = useGameStore((s) => s.gameState?.objects[objectId]);
  const retainedObjectRef = useRef(liveObject);
  if (liveObject) retainedObjectRef.current = liveObject;
  const obj = liveObject ?? retainedObjectRef.current;
  const players = useGameStore((s) => s.gameState?.players);
  const dispatch = useGameDispatch();
  const isMobile = useIsMobile();
  const headerId = useId();
  // Accordion: at most one submenu open at a time. Opening another collapses it.
  const [openSubmenu, setOpenSubmenu] = useState<SubmenuName | null>(null);

  useFocusScopePortalBranch({
    active: Boolean(obj),
    containerRef: ref,
    anchorRef: anchorRef ?? fallbackAnchorRef,
    onDismiss: onClose,
  });

  useLayoutEffect(() => {
    // An engine/debug snapshot can remove this object before an async action's
    // dispatch promise settles. Retain the last render through this layout
    // handoff so the portal unregisters while its focused owner and exact
    // anchor are both still available, then let the store close unmount it.
    if (!liveObject) onClose();
  }, [liveObject, onClose]);

  useLayoutEffect(() => {
    // Scoped modal launchers provide an exact restoration authority. Preserve
    // the legacy standalone game-surface behavior until it has one too, rather
    // than moving focus into a menu that can only dismiss back to <body>.
    if (anchorRef) {
      const menu = ref.current;
      if (menu) currentMenuCommands(menu)[0]?.focus();
    }
  }, [anchorRef]);

  const anchorBottom = y > window.innerHeight / 2;
  const left = Math.max(8, Math.min(x, window.innerWidth - 232));
  const maxHeight = anchorBottom ? y - 8 : window.innerHeight - y - 8;
  // Open the flyout on whichever side has room: prefer right, fall back to left
  // only when the right is too tight AND the left actually fits (the menu's left
  // is clamped to >= 8, so a left flyout needs `left >= FLYOUT_WIDTH`).
  const roomRight = window.innerWidth - (left + 224);
  const flyout: SubmenuFlyout = isMobile
    ? "inline"
    : roomRight < FLYOUT_WIDTH && left >= FLYOUT_WIDTH
      ? "left"
      : "right";

  const submenuProps = (name: SubmenuName): SubmenuChrome => ({
    flyout,
    anchorBottom,
    maxHeight,
    open: openSubmenu === name,
    onToggle: () => setOpenSubmenu((cur) => (cur === name ? null : name)),
  });

  const handleMenuKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const target = event.target;
    if (!(target instanceof HTMLElement)) return;

    // Number fields retain their native cursor and increment/decrement keys.
    if (target instanceof HTMLInputElement) return;

    if (event.key === "ArrowRight") {
      const trigger = target.closest<HTMLButtonElement>(
        "[data-debug-submenu-trigger]",
      );
      if (!trigger || !event.currentTarget.contains(trigger)) return;

      event.preventDefault();
      event.stopPropagation();
      if (trigger.getAttribute("aria-expanded") !== "true") {
        trigger.click();
      }
      requestAnimationFrame(() => {
        const panelId = trigger.getAttribute("aria-controls");
        const panel = panelId ? document.getElementById(panelId) : null;
        if (panel) currentMenuCommands(panel)[0]?.focus();
      });
      return;
    }

    if (event.key === "ArrowLeft") {
      const panel = target.closest<HTMLElement>(
        "[data-debug-submenu-panel]",
      );
      const triggerId = panel?.getAttribute("aria-labelledby");
      const trigger = triggerId
        ? document.getElementById(triggerId)
        : null;
      if (
        !(trigger instanceof HTMLButtonElement) ||
        !event.currentTarget.contains(trigger)
      ) {
        return;
      }

      event.preventDefault();
      event.stopPropagation();
      if (trigger.getAttribute("aria-expanded") === "true") {
        trigger.click();
      }
      trigger.focus();
      return;
    }

    if (
      event.key !== "ArrowDown" &&
      event.key !== "ArrowUp" &&
      event.key !== "Home" &&
      event.key !== "End"
    ) {
      return;
    }

    const currentMenu = target.closest<HTMLElement>('[role="menu"]');
    if (!currentMenu || !event.currentTarget.contains(currentMenu)) return;
    const controls = currentMenuCommands(currentMenu);
    if (controls.length === 0) return;

    event.preventDefault();
    event.stopPropagation();
    const currentControl = closestCurrentMenuCommand(target, currentMenu);
    const currentIndex = currentControl ? controls.indexOf(currentControl) : -1;
    if (event.key === "Home") {
      controls[0].focus();
    } else if (event.key === "End") {
      controls[controls.length - 1].focus();
    } else if (event.key === "ArrowDown") {
      controls[(currentIndex + 1 + controls.length) % controls.length].focus();
    } else {
      const previousIndex = currentIndex < 0 ? 0 : currentIndex - 1;
      controls[(previousIndex + controls.length) % controls.length].focus();
    }
  };

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
  const hasSummoningSickness = obj.has_summoning_sickness ?? false;
  const currentKeywords = obj.keywords ?? [];

  return createPortal(
    <div
      ref={ref}
      role="menu"
      aria-labelledby={headerId}
      className={
        "fixed z-[120] w-56 rounded-lg border border-gray-700 bg-gray-900/95 py-1 shadow-xl backdrop-blur-sm " +
        // Mobile expands submenus inline, so the menu can grow tall and needs to
        // scroll. Desktop flyouts must not be clipped, so the container stays
        // visible (the menu itself is short without inline expansion).
        (isMobile ? "overflow-y-auto" : "overflow-visible")
      }
      style={{
        left,
        ...(isMobile ? { maxHeight } : {}),
        ...(anchorBottom
          ? { bottom: window.innerHeight - y }
          : { top: y }),
      }}
      onContextMenu={(e) => e.preventDefault()}
      onKeyDown={handleMenuKeyDown}
    >
      {/* Card name header */}
      <div
        id={headerId}
        className="truncate border-b border-gray-800 px-3 py-1.5 font-mono text-xs font-semibold text-gray-300"
      >
        {obj.name || `Object ${objectId}`}
        {obj.class_level != null && (
          <span className="ml-1 text-amber-400">Lv.{obj.class_level}</span>
        )}
      </div>

      {/* Zone submenu */}
      <div className="border-b border-gray-800 py-0.5">
        <ZoneSubmenu
          chrome={submenuProps("zone")}
          currentZone={obj.zone}
          onSelectZone={(zone, simulate, libraryPosition) =>
            dispatchDebug({
              type: "MoveToZone",
              data: {
                object_id: objectId,
                to_zone: zone,
                ...(libraryPosition ? { library_position: libraryPosition } : {}),
                simulate,
              },
            })
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
            <ControllerSubmenu chrome={submenuProps("controller")} objectId={objectId} currentController={obj.controller} players={players!} onDispatch={dispatchDebug} />
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
          {Object.entries(obj.counters ?? {})
            .flatMap(([counterType, count]) => {
              const current = count ?? 0;
              return current > 0
                ? [
                    <CounterRow
                      key={counterType}
                      label={counterType}
                      objectId={objectId}
                      counterType={counterType}
                      current={current}
                      onDispatch={dispatchDebugKeepOpen}
                    />,
                  ]
                : [];
            })}
        </div>
      )}

      {/* Keywords */}
      {onBattlefield && (
        <div className="border-b border-gray-800 py-0.5">
          <KeywordSubmenu
            chrome={submenuProps("keywords")}
            objectId={objectId}
            currentKeywords={currentKeywords}
            onDispatch={dispatchDebugKeepOpen}
          />
        </div>
      )}

      {/* Sacrifice — routes through the engine's CR 701.21 sacrifice pipeline so
          dies / leaves-the-battlefield triggers fire, unlike "Remove" which
          deletes the object outright. Offered for any battlefield permanent. */}
      {onBattlefield && (
        <div className="border-b border-gray-800 py-0.5">
          <MenuItem
            label="Sacrifice"
            onClick={() => dispatchDebug({ type: "Sacrifice", data: { object_id: objectId } })}
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
    </div>,
    document.body,
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

// Shared expand/flyout wrapper for every nested submenu. Renders a toggle row
// (label + value badge) and, when open, the children either stacked inline
// (mobile) or as a bordered side flyout (tablet/desktop). The flyout escapes the
// parent menu's bounds, so the parent must use `overflow-visible` on desktop.
function Submenu({
  label,
  badge,
  flyout,
  anchorBottom,
  maxHeight,
  open,
  onToggle,
  children,
}: SubmenuChrome & {
  label: string;
  badge?: ReactNode;
  children: ReactNode;
}) {
  const panelId = useId();
  const triggerId = useId();
  const inline = flyout === "inline";
  // Inline (mobile) stacks below the button with its own scroll. The side flyout
  // is absolutely positioned; anchor its top or bottom to the button so it grows
  // into the same vertical half the parent menu chose, and cap it to that space
  // (with scroll only as a last-resort safety) so a tall list — the keyword grid
  // — can't run off the top or bottom of the viewport.
  const panelClass = inline
    ? "ml-2 border-l border-gray-700"
    : "absolute z-10 min-w-[12rem] overflow-y-auto rounded-lg border border-gray-700 bg-gray-900/95 py-1 shadow-xl backdrop-blur-sm " +
      (anchorBottom ? "bottom-0 " : "top-0 ") +
      (flyout === "left" ? "right-full mr-1" : "left-full ml-1");

  return (
    <div className="relative">
      <button
        id={triggerId}
        role="menuitem"
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        data-debug-submenu-trigger
        onClick={onToggle}
        className="flex w-full items-center justify-between px-3 py-1.5 text-left text-xs text-gray-300 transition-colors hover:bg-white/10"
      >
        <span>
          {label} {flyout === "left" ? "←" : "→"}
        </span>
        {badge != null && badge !== "" && (
          <span className="text-[10px] text-gray-600">{badge}</span>
        )}
      </button>
      {open && (
        <div
          id={panelId}
          role="menu"
          aria-labelledby={triggerId}
          data-debug-submenu-panel
          className={panelClass}
          style={inline ? undefined : { maxHeight }}
        >
          {children}
        </div>
      )}
    </div>
  );
}

function ZoneSubmenu({
  currentZone,
  onSelectZone,
  chrome,
}: {
  currentZone: Zone;
  onSelectZone: (
    zone: Zone,
    simulate: boolean,
    libraryPosition?: LibraryPosition,
  ) => void;
  chrome: SubmenuChrome;
}) {
  const { t } = useTranslation("game");
  // Off by default: a debug move stages the board raw. On, the engine runs the
  // real pipeline — ETB replacements and triggers, leave/dies triggers, SBAs.
  const [runEtb, setRunEtb] = useState(false);

  return (
    <Submenu label="Zone" badge={currentZone} {...chrome}>
      <button
        role="menuitemcheckbox"
        type="button"
        aria-checked={runEtb}
        onClick={() => setRunEtb((cur) => !cur)}
        className={
          "flex w-full items-center gap-2 border-b border-gray-800 px-3 py-1 text-left text-xs transition-colors hover:bg-white/10 " +
          (runEtb ? "text-cyan-300" : "text-gray-400")
        }
      >
        <span className="w-3 text-center text-[10px]">{runEtb ? "✓" : ""}</span>
        <span>{t("debugMove.runEtbEffects")}</span>
      </button>
      {ZONES.filter((z) => z !== currentZone).map((zone) =>
        zone === "Library" ? (
          <div key={zone}>
            <MenuItem
              label="Library (top)"
              onClick={() => onSelectZone(zone, runEtb, { type: "Top" })}
              compact
            />
            <MenuItem
              label="Library (bottom)"
              onClick={() => onSelectZone(zone, runEtb, { type: "Bottom" })}
              compact
            />
          </div>
        ) : (
          <MenuItem
            key={zone}
            label={zone}
            onClick={() => onSelectZone(zone, runEtb)}
            compact
          />
        ),
      )}
    </Submenu>
  );
}

function ControllerSubmenu({
  objectId,
  currentController,
  players,
  onDispatch,
  chrome,
}: {
  objectId: ObjectId;
  currentController: number;
  players: { id: number }[];
  onDispatch: (action: DebugAction) => Promise<void>;
  chrome: SubmenuChrome;
}) {
  return (
    <Submenu label="Controller" badge={`P${currentController}`} {...chrome}>
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
    </Submenu>
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
          role="menuitem"
          type="button"
          aria-label={`${label}: −1`}
          onClick={() =>
            onDispatch({ type: "ModifyCounters", data: { object_id: objectId, counter_type: counterType, delta: -1 } })
          }
          className="rounded bg-gray-800 px-1.5 py-0.5 text-gray-400 transition-colors hover:bg-gray-700 hover:text-gray-200"
        >
          −
        </button>
        <span className="w-5 text-center font-mono text-amber-400">{current}</span>
        <button
          role="menuitem"
          type="button"
          aria-label={`${label}: +1`}
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
        role="menuitem"
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
  chrome,
}: {
  objectId: ObjectId;
  currentKeywords: Keyword[];
  onDispatch: (action: DebugAction) => Promise<void>;
  chrome: SubmenuChrome;
}) {
  const stringKeywords = currentKeywords.filter((k): k is string => typeof k === "string");

  return (
    <Submenu
      label="Keywords"
      badge={stringKeywords.length > 0 ? stringKeywords.length : ""}
      {...chrome}
    >
      {/* Inline (mobile) keeps a capped scroll list; the side flyout has room to
          show every keyword at once in two content-sized columns — no scrollbar.
          `max-content` columns (not `grid-cols-2`, which is minmax(0,1fr) and
          collapses inside this auto-width panel) keep long names from clipping. */}
      <div
        className={
          chrome.flyout === "inline"
            ? "max-h-48 overflow-y-auto"
            : "grid grid-cols-[max-content_max-content]"
        }
      >
        {COMMON_KEYWORDS.map((kw) => {
          const kwStr = typeof kw === "string" ? kw : "";
          const hasKeyword = stringKeywords.includes(kwStr);
          return (
            <button
              key={kwStr}
              role="menuitemcheckbox"
              type="button"
              aria-checked={hasKeyword}
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
    </Submenu>
  );
}
