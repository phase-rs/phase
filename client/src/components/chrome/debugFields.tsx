/* eslint-disable react-refresh/only-export-components */
import type { ReactNode } from "react";
import { useEffect, useMemo, useRef, useState } from "react";

import type { GameObject, Keyword, ManaType, ObjectId, PlayerId, Zone } from "../../adapter/types";
import { getCardNames } from "../../services/cardNames";
import { useGameStore } from "../../stores/gameStore";
import { getSeatColor } from "../../hooks/useSeatColor";
import { getPlayerDisplayName } from "../../stores/multiplayerStore";
import { usePerspectivePlayerId } from "../../hooks/usePlayerId";
import { useUiStore } from "../../stores/uiStore";
import { MenuSelect } from "../ui/MenuSelect";

// ── Layout ──────────────────────────────────────────────────────────────

export function FieldRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex items-center gap-2">
      <label className="w-20 shrink-0 font-mono text-[10px] text-gray-400">{label}</label>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

// ── Inputs ──────────────────────────────────────────────────────────────

const inputClass =
  "w-full rounded border border-gray-700 bg-gray-800 px-2 py-1 font-mono text-xs text-gray-300 focus:border-blue-500 focus:outline-none";

/** Shared MenuSelect wiring for debug-panel fields inside scrollable overflow containers. */
const DEBUG_MENU_PROPS = {
  menuLayout: "dropdown" as const,
  fitContainer: true,
  wrapperClassName: "w-full min-w-0",
  className:
    "rounded border border-gray-700 bg-gray-800 px-2 py-1 font-mono text-xs text-gray-300 hover:bg-gray-800 focus-visible:ring-2 focus-visible:ring-blue-500/40",
  chevronClassName: "h-3 w-3 text-gray-500",
  menuClassName: "border-gray-700 bg-gray-800/98 py-0.5 font-mono text-[10px]",
  menuZClassName: "z-[10000]",
};

export function NumberInput({
  value,
  onChange,
  min,
  max,
  placeholder,
}: {
  value: number;
  onChange: (v: number) => void;
  min?: number;
  max?: number;
  placeholder?: string;
}) {
  return (
    <input
      type="number"
      value={value}
      onChange={(e) => onChange(Number(e.target.value))}
      min={min}
      max={max}
      placeholder={placeholder}
      className={inputClass}
    />
  );
}

export function TextInput({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <input
      type="text"
      value={value}
      onChange={(e) => onChange(e.target.value)}
      placeholder={placeholder}
      className={inputClass}
    />
  );
}

export function SelectInput<T extends string>({
  value,
  onChange,
  options,
}: {
  value: T;
  onChange: (v: T) => void;
  options: readonly T[];
}) {
  const items = useMemo(
    () => options.map((opt) => ({ value: opt, label: opt })),
    [options],
  );

  return (
    <MenuSelect
      {...DEBUG_MENU_PROPS}
      label={value}
      items={items}
      selectedValue={value}
      onSelect={(next) => onChange(next as T)}
    />
  );
}

export function CheckboxInput({
  checked,
  onChange,
  label,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
}) {
  return (
    <label className="flex cursor-pointer items-center gap-1.5 font-mono text-[10px] text-gray-400">
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="accent-blue-500"
      />
      {label}
    </label>
  );
}

// ── Domain-specific inputs ──────────────────────────────────────────────

export function ObjectIdInput({
  value,
  onChange,
  label,
}: {
  value: number;
  onChange: (v: number) => void;
  label?: string;
}) {
  const selectedObjectId = useUiStore((s) => s.selectedObjectId);
  const gameState = useGameStore((s) => s.gameState);
  const selectedName =
    selectedObjectId != null && gameState
      ? gameState.objects[selectedObjectId]?.name
      : null;

  return (
    <FieldRow label={label ?? "Object ID"}>
      <div className="flex items-center gap-1">
        <input
          type="number"
          value={value}
          onChange={(e) => onChange(Number(e.target.value))}
          className={inputClass + " flex-1"}
          min={0}
        />
        {selectedObjectId != null && (
          <button
            onClick={() => onChange(selectedObjectId)}
            className="shrink-0 rounded bg-gray-700 px-1.5 py-1 text-[10px] text-blue-300 transition-colors hover:bg-gray-600"
            title={selectedName ? `Use ${selectedName} (${selectedObjectId})` : `Use ${selectedObjectId}`}
          >
            sel
          </button>
        )}
      </div>
    </FieldRow>
  );
}

// `ObjectSelect` — searchable, zone-grouped dropdown for picking a `GameObject`
// by name. Replaces raw numeric `ObjectIdInput` everywhere a debug action needs
// an `ObjectId`. The optional `filter` lets each call site declare its actual
// constraint (e.g., "battlefield only" for SetController, "creatures only" for
// Equipment attach). The currently-selected object is always shown even when it
// would otherwise be filtered out — never silently lose the user's selection
// when state changes underneath.
//
// Ordering: groups by zone (Battlefield → Hand → others), then by "you" first
// within each group, then by name.
const ZONE_ORDER: readonly Zone[] = [
  "Battlefield",
  "Stack",
  "Hand",
  "Graveyard",
  "Exile",
  "Library",
  "Command",
];

interface ObjectSelectProps {
  value: ObjectId | null;
  onChange: (v: ObjectId) => void;
  /** Optional predicate. Default: include all objects in any zone. */
  filter?: (obj: GameObject) => boolean;
  label?: string;
  placeholder?: string;
}

export function ObjectSelect({
  value,
  onChange,
  filter,
  label,
  placeholder = "Pick an object…",
}: ObjectSelectProps) {
  const objectsMap = useGameStore((s) => s.gameState?.objects);
  const myId = usePerspectivePlayerId();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [highlightIndex, setHighlightIndex] = useState(0);
  const containerRef = useRef<HTMLDivElement>(null);

  // Close on outside pointerdown.
  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("pointerdown", handleClick, true);
    return () => document.removeEventListener("pointerdown", handleClick, true);
  }, [open]);

  const grouped = useMemo(() => {
    if (!objectsMap) return [];
    const filtered: GameObject[] = [];
    for (const obj of Object.values(objectsMap) as GameObject[]) {
      if (filter && !filter(obj)) {
        // Always include the currently-selected object so the displayed value
        // never becomes a phantom — applies whether or not it matches the filter.
        if (obj.id !== value) continue;
      }
      const q = query.trim().toLowerCase();
      if (q) {
        const idMatch = String(obj.id).includes(q);
        const nameMatch = obj.name.toLowerCase().includes(q);
        if (!idMatch && !nameMatch && obj.id !== value) continue;
      }
      filtered.push(obj);
    }
    // Group by zone, sort by "you" then name within each group.
    const buckets = new Map<Zone, GameObject[]>();
    for (const obj of filtered) {
      const arr = buckets.get(obj.zone) ?? [];
      arr.push(obj);
      buckets.set(obj.zone, arr);
    }
    const result: { zone: Zone; objects: GameObject[] }[] = [];
    for (const zone of ZONE_ORDER) {
      const arr = buckets.get(zone);
      if (!arr) continue;
      arr.sort((a, b) => {
        const aMine = a.controller === myId ? 0 : 1;
        const bMine = b.controller === myId ? 0 : 1;
        if (aMine !== bMine) return aMine - bMine;
        return a.name.localeCompare(b.name);
      });
      result.push({ zone, objects: arr });
    }
    return result;
  }, [objectsMap, filter, query, value, myId]);

  // Flat list mirrors the rendered order — drives keyboard navigation.
  const flat = useMemo(() => grouped.flatMap((g) => g.objects), [grouped]);

  useEffect(() => {
    setHighlightIndex(0);
  }, [query, open]);

  const selectedObj =
    value != null && objectsMap ? (objectsMap[value] as GameObject | undefined) : undefined;
  const selectedLabel = selectedObj
    ? `${selectedObj.name}  #${selectedObj.id}`
    : placeholder;

  // Drive a *debug-specific* board highlight (`debugHighlightedObjectId`),
  // not the standard `hoveredObjectId`. Most board elements don't visibly
  // react to plain hover, so the debug-panel preview needs its own loud
  // signal — `PermanentCard` (and any other surface that opts in) renders an
  // fuchsia ring + pulse for the debug-highlighted object. Decoupling from
  // `hoveredObjectId` also avoids fighting the standard hover-lift behavior
  // when the user is just trying to inspect from afar.
  const setDebugHighlight = useUiStore((s) => s.setDebugHighlightedObjectId);

  // Clear the highlight when the dropdown closes for any reason (selection,
  // outside-click, Escape). The cleanup function ALSO runs on component
  // unmount — without it, closing the parent accordion mid-hover would leave
  // a phantom fuchsia ring on the last-previewed permanent forever.
  useEffect(() => {
    if (!open) setDebugHighlight(null);
    return () => setDebugHighlight(null);
  }, [open, setDebugHighlight]);

  const select = (id: ObjectId) => {
    onChange(id);
    setOpen(false);
    setQuery("");
    setDebugHighlight(null);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (!flat.length) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setHighlightIndex((i) => Math.min(i + 1, flat.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setHighlightIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter" && flat[highlightIndex]) {
      e.preventDefault();
      select(flat[highlightIndex].id);
    } else if (e.key === "Escape") {
      setOpen(false);
    }
  };

  let runningIndex = -1;
  return (
    <FieldRow label={label ?? "Object"}>
      <div ref={containerRef} className="relative">
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          className={inputClass + " text-left flex items-center justify-between gap-2"}
        >
          <span className={selectedObj ? "truncate" : "truncate text-gray-500"}>
            {selectedLabel}
          </span>
          <span className="shrink-0 text-[10px] text-gray-500">▾</span>
        </button>
        {open && (
          <div className="absolute left-0 right-0 top-full z-50 mt-0.5 max-h-72 overflow-hidden rounded border border-gray-700 bg-gray-800 shadow-lg">
            <input
              type="text"
              value={query}
              autoFocus
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder="Search by name or id…"
              className="block w-full border-b border-gray-700 bg-gray-900 px-2 py-1 font-mono text-[11px] text-gray-200 focus:outline-none"
            />
            <div className="max-h-60 overflow-y-auto">
              {grouped.length === 0 && (
                <div className="px-2 py-1.5 font-mono text-[10px] text-gray-500">
                  No matching objects
                </div>
              )}
              {grouped.map(({ zone, objects }) => (
                <div key={zone}>
                  <div className="px-2 pt-1 pb-0.5 font-mono text-[9px] uppercase tracking-wider text-gray-500">
                    {zone}
                  </div>
                  {objects.map((obj) => {
                    runningIndex += 1;
                    const isHighlighted = runningIndex === highlightIndex;
                    const isSelected = obj.id === value;
                    return (
                      <button
                        key={obj.id}
                        type="button"
                        onClick={() => select(obj.id)}
                        onMouseEnter={() => setDebugHighlight(obj.id)}
                        onMouseLeave={() => setDebugHighlight(null)}
                        onFocus={() => setDebugHighlight(obj.id)}
                        onBlur={() => setDebugHighlight(null)}
                        className={
                          "flex w-full items-center justify-between gap-2 px-2 py-1 text-left font-mono text-[10px] transition-colors " +
                          (isHighlighted
                            ? "bg-blue-700/40 text-blue-100"
                            : isSelected
                              ? "bg-gray-700/40 text-gray-200"
                              : "text-gray-300 hover:bg-white/10")
                        }
                      >
                        <span className="truncate">
                          {obj.name}
                          <span
                            className={
                              "ml-1.5 text-[9px] " +
                              (obj.controller === myId ? "text-blue-400" : "text-rose-400")
                            }
                          >
                            {obj.controller === myId ? "you" : `P${obj.controller}`}
                          </span>
                        </span>
                        <span className="shrink-0 text-[9px] text-gray-500">#{obj.id}</span>
                      </button>
                    );
                  })}
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </FieldRow>
  );
}

export function PlayerSelect({
  value,
  onChange,
}: {
  value: PlayerId;
  onChange: (v: PlayerId) => void;
}) {
  const players = useGameStore((s) => s.gameState?.players);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);
  const myId = usePerspectivePlayerId();
  const setDebugHighlightedPlayerId = useUiStore((s) => s.setDebugHighlightedPlayerId);

  const items = useMemo(
    () =>
      (players ?? []).map((p) => ({
        value: String(p.id),
        label: getPlayerDisplayName(p.id, myId),
      })),
    [players, myId],
  );

  const selectedLabel =
    items.find((item) => item.value === String(value))?.label ?? "Player";
  const seatColor = getSeatColor(value, seatOrder);

  return (
    <MenuSelect
      {...DEBUG_MENU_PROPS}
      label={selectedLabel}
      items={items}
      selectedValue={String(value)}
      triggerStyle={{ color: seatColor }}
      chevronClassName="h-3 w-3"
      getOptionStyle={(item) => ({
        color: getSeatColor(Number(item.value) as PlayerId, seatOrder),
      })}
      onOptionHover={(next) =>
        setDebugHighlightedPlayerId(next != null ? (Number(next) as PlayerId) : null)
      }
      onSelect={(next) => {
        const playerId = Number(next) as PlayerId;
        onChange(playerId);
        setDebugHighlightedPlayerId(playerId);
      }}
    />
  );
}

const MANA_TYPES: readonly ManaType[] = [
  "White",
  "Blue",
  "Black",
  "Red",
  "Green",
  "Colorless",
] as const;

const MANA_LABELS: Record<ManaType, string> = {
  White: "W",
  Blue: "U",
  Black: "B",
  Red: "R",
  Green: "G",
  Colorless: "C",
};

export function ManaTypeSelect({
  value,
  onChange,
}: {
  value: ManaType[];
  onChange: (v: ManaType[]) => void;
}) {
  const toggle = (mana: ManaType) => {
    onChange(
      value.includes(mana) ? value.filter((m) => m !== mana) : [...value, mana],
    );
  };

  return (
    <div className="flex flex-wrap gap-1">
      {MANA_TYPES.map((m) => {
        const active = value.includes(m);
        return (
          <button
            key={m}
            type="button"
            onClick={() => toggle(m)}
            className={
              "rounded-full border px-2 py-0.5 font-mono text-[10px] transition-colors " +
              (active
                ? "border-blue-500/60 bg-blue-500/20 text-blue-300"
                : "border-gray-700 bg-transparent text-gray-600 hover:border-gray-600")
            }
          >
            {MANA_LABELS[m]}
          </button>
        );
      })}
    </div>
  );
}

// ── Attachment legality ─────────────────────────────────────────────────
// Shared between the Attach form (works against a battlefield `GameObject`)
// and the spawn-attached CreateCard form (works against a pre-spawn
// `CardFace` from the database). The minimal shape both share is:
//   - `keywords`: the printed/runtime keyword list (Enchant filter lives here)
//   - `subtypes`: detects Equipment / Fortification when no Enchant
// The returned `AttachmentInfo` is consumed by both forms to pick between a
// `PlayerSelect` and an `ObjectSelect` filtered to the right host class.

export interface AttachmentInfo {
  /** True when the source can attach to a player (e.g., Curse cycle). */
  canTargetPlayer: boolean;
  /** True when the source can attach to an object (most cases). */
  canTargetObject: boolean;
  /** Object filter applied to ObjectSelect when targeting objects. */
  objectFilter: (obj: GameObject) => boolean;
}

const onBattlefield = (obj: GameObject) => obj.zone === "Battlefield";
const isCreatureOnBattlefield = (obj: GameObject) =>
  obj.zone === "Battlefield" && obj.card_types.core_types.includes("Creature");

export function deriveAttachmentInfo(input: {
  keywords?: Keyword[] | null;
  subtypes?: string[] | null;
}): AttachmentInfo {
  const subtypes = input.subtypes ?? [];
  // Equipment / Fortification — Object-only, filtered to creatures per CR 301.5.
  if (subtypes.includes("Equipment") || subtypes.includes("Fortification")) {
    return {
      canTargetPlayer: false,
      canTargetObject: true,
      objectFilter: isCreatureOnBattlefield,
    };
  }
  // Inspect Keyword::Enchant payload. Keywords serialize as either bare strings
  // ("Flying") or `{ Variant: data }` (parameterized) — Enchant is the latter,
  // carrying a TargetFilter that tells us what hosts are legal.
  for (const kw of input.keywords ?? []) {
    if (typeof kw !== "object" || kw === null || !("Enchant" in kw)) continue;
    const filter = (kw as { Enchant: unknown }).Enchant;
    if (!filter || typeof filter !== "object") continue;
    const t = (filter as { type?: string }).type;
    if (t === "Player") {
      return { canTargetPlayer: true, canTargetObject: false, objectFilter: onBattlefield };
    }
    if (t === "Typed") {
      const typeFilters: string[] =
        (filter as { type_filters?: string[] }).type_filters ?? [];
      if (typeFilters.includes("Creature")) {
        return {
          canTargetPlayer: false,
          canTargetObject: true,
          objectFilter: isCreatureOnBattlefield,
        };
      }
    }
    // Unknown Enchant variant — accept any permanent.
    return { canTargetPlayer: false, canTargetObject: true, objectFilter: onBattlefield };
  }
  // No Enchant, no Equipment/Fortification — not an attachment-shaped card.
  return { canTargetPlayer: false, canTargetObject: false, objectFilter: onBattlefield };
}

// ── Card Name Autocomplete ─────────────────────────────────────────────

const MAX_SUGGESTIONS = 12;

export function CardNameAutocomplete({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  const [allNames, setAllNames] = useState<string[]>([]);
  const [open, setOpen] = useState(false);
  const [highlightIndex, setHighlightIndex] = useState(0);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    getCardNames().then(setAllNames);
  }, []);

  const matches = useMemo(() => {
    if (value.length < 2) return [];
    const lower = value.toLowerCase();
    const prefix: string[] = [];
    const substring: string[] = [];
    for (const name of allNames) {
      const nameLower = name.toLowerCase();
      if (nameLower === lower) return [];
      if (nameLower.startsWith(lower)) {
        prefix.push(name);
      } else if (nameLower.includes(lower)) {
        substring.push(name);
      }
      if (prefix.length + substring.length >= MAX_SUGGESTIONS) break;
    }
    return [...prefix, ...substring].slice(0, MAX_SUGGESTIONS);
  }, [value, allNames]);

  useEffect(() => {
    setHighlightIndex(0);
  }, [matches]);

  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("pointerdown", handleClick, true);
    return () => document.removeEventListener("pointerdown", handleClick, true);
  }, [open]);

  const select = (name: string) => {
    onChange(name);
    setOpen(false);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (!matches.length) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setHighlightIndex((i) => Math.min(i + 1, matches.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setHighlightIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter" && open && matches[highlightIndex]) {
      e.preventDefault();
      select(matches[highlightIndex]);
    } else if (e.key === "Escape") {
      setOpen(false);
    }
  };

  return (
    <div ref={containerRef} className="relative">
      <input
        type="text"
        value={value}
        onChange={(e) => { onChange(e.target.value); setOpen(true); }}
        onFocus={() => setOpen(true)}
        onKeyDown={handleKeyDown}
        placeholder={placeholder}
        className={inputClass}
      />
      {open && matches.length > 0 && (
        <div className="absolute left-0 right-0 top-full z-50 mt-0.5 max-h-40 overflow-y-auto rounded border border-gray-700 bg-gray-800 shadow-lg">
          {matches.map((name, i) => (
            <button
              key={name}
              type="button"
              onClick={() => select(name)}
              className={
                "block w-full px-2 py-1 text-left font-mono text-[10px] transition-colors " +
                (i === highlightIndex
                  ? "bg-blue-700/40 text-blue-200"
                  : "text-gray-300 hover:bg-white/10")
              }
            >
              {name}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

// ── Actions ─────────────────────────────────────────────────────────────

export function SubmitButton({
  onClick,
  children,
  disabled,
}: {
  onClick: () => void;
  children: ReactNode;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className="w-full rounded bg-blue-700 px-2 py-1 text-xs font-medium text-white transition-colors hover:bg-blue-600 disabled:cursor-not-allowed disabled:opacity-40"
    >
      {children}
    </button>
  );
}

export function StatusMessage({ status }: { status: { type: "success" | "error"; message: string } }) {
  return (
    <div
      className={`mt-1 rounded px-2 py-1 text-[10px] ${
        status.type === "error"
          ? "bg-red-900/50 text-red-300"
          : "bg-green-900/50 text-green-300"
      }`}
    >
      {status.message}
    </div>
  );
}

// ── Accordion ───────────────────────────────────────────────────────────

export function AccordionItem({
  label,
  expanded,
  onToggle,
  children,
}: {
  label: string;
  expanded: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  return (
    <div className="border-b border-gray-800 last:border-b-0">
      <button
        onClick={onToggle}
        className="flex w-full items-center justify-between px-1 py-1.5 text-left text-xs text-gray-400 transition-colors hover:text-gray-200"
      >
        <span>{label}</span>
        <span className="text-[10px] text-gray-600">{expanded ? "−" : "+"}</span>
      </button>
      {expanded && <div className="flex flex-col gap-1.5 pb-2">{children}</div>}
    </div>
  );
}

// ── Accordion Hook ──────────────────────────────────────────────────────

export function useAccordion() {
  const [expanded, setExpanded] = useState<string | null>(null);
  const toggle = (key: string) => setExpanded((prev) => (prev === key ? null : key));
  return { expanded, toggle };
}
