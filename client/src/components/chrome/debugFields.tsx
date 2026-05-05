/* eslint-disable react-refresh/only-export-components */
import type { ReactNode } from "react";
import { useEffect, useMemo, useRef, useState } from "react";

import type { ManaType, PlayerId } from "../../adapter/types";
import { getCardNames } from "../../services/cardNames";
import { useGameStore } from "../../stores/gameStore";
import { useUiStore } from "../../stores/uiStore";

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
  return (
    <select value={value} onChange={(e) => onChange(e.target.value as T)} className={inputClass}>
      {options.map((opt) => (
        <option key={opt} value={opt}>
          {opt}
        </option>
      ))}
    </select>
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

export function PlayerSelect({
  value,
  onChange,
}: {
  value: PlayerId;
  onChange: (v: PlayerId) => void;
}) {
  const players = useGameStore((s) => s.gameState?.players);
  return (
    <select
      value={value}
      onChange={(e) => onChange(Number(e.target.value) as PlayerId)}
      className={inputClass}
    >
      {(players ?? []).map((p) => (
        <option key={p.id} value={p.id}>
          Player {p.id}
        </option>
      ))}
    </select>
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
