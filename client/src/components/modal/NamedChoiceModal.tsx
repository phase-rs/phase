import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { motion } from "framer-motion";

import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { getCardNames } from "../../services/cardNames.ts";
import type { WaitingFor } from "../../adapter/types.ts";

type NamedChoice = Extract<WaitingFor, { type: "NamedChoice" }>;

const CHOICE_TYPE_LABELS: Record<string, string> = {
  CreatureType: "Choose a Creature Type",
  Color: "Choose a Color",
  OddOrEven: "Choose Odd or Even",
  BasicLandType: "Choose a Basic Land Type",
  CardType: "Choose a Card Type",
  CardName: "Name a Card",
  LandType: "Choose a Land Type",
  Opponent: "Choose an Opponent",
  Player: "Choose a Player",
  TwoColors: "Choose Two Colors",
  NumberRange: "Choose a Number",
  Labeled: "Make a Choice",
};

/** Extract the string key from a ChoiceType value.
 * Unit variants serialize as strings; data variants serialize as
 * { "NumberRange": { ... } } objects (externally-tagged serde enum). */
function getChoiceTypeKey(choiceType: string | Record<string, unknown>): string {
  if (typeof choiceType === "string") return choiceType;
  const key = Object.keys(choiceType)[0];
  return key ?? "Unknown";
}

const MAX_RESULTS = 10;

export function NamedChoiceModal({ data }: { data: NamedChoice["data"] }) {
  const typeKey = getChoiceTypeKey(data.choice_type);
  if (typeKey === "CardName") {
    return <CardNameSearch />;
  }
  return <ButtonGrid data={data} typeKey={typeKey} />;
}

function CardNameSearch() {
  const dispatch = useGameDispatch();
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [allNames, setAllNames] = useState<string[]>([]);
  const [highlightIndex, setHighlightIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    getCardNames().then(setAllNames);
  }, []);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const matches = useMemo(() => {
    if (query.length < 2) return [];
    const lower = query.toLowerCase();
    const prefix: string[] = [];
    const substring: string[] = [];
    for (const name of allNames) {
      const nameLower = name.toLowerCase();
      if (nameLower.startsWith(lower)) {
        prefix.push(name);
      } else if (nameLower.includes(lower)) {
        substring.push(name);
      }
      if (prefix.length + substring.length >= MAX_RESULTS) break;
    }
    return [...prefix, ...substring].slice(0, MAX_RESULTS);
  }, [query, allNames]);

  // Reset highlight when matches change
  useEffect(() => {
    setHighlightIndex(0);
  }, [matches]);

  const handleConfirm = useCallback(() => {
    if (selected) {
      dispatch({ type: "ChooseOption", data: { choice: selected } });
    }
  }, [dispatch, selected]);

  const handleSelect = useCallback((name: string) => {
    setSelected(name);
    setQuery(name);
  }, []);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (selected) return;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setHighlightIndex((i) => Math.min(i + 1, matches.length - 1));
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setHighlightIndex((i) => Math.max(i - 1, 0));
      } else if (e.key === "Enter" && matches[highlightIndex]) {
        e.preventDefault();
        handleSelect(matches[highlightIndex]);
      }
    },
    [selected, matches, highlightIndex, handleSelect],
  );

  const showResults = matches.length > 0 && !selected;

  return (
    <ChoiceOverlay title="Name a Card" subtitle="Type to search all cards" footer={<ConfirmButton onClick={handleConfirm} disabled={!selected} />}>
      <div className="flex w-full max-w-md flex-col items-center gap-3">
        <div className="relative w-full">
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setSelected(null);
            }}
            onKeyDown={handleKeyDown}
            placeholder="Search by name..."
            className="w-full rounded-lg border-2 border-gray-600 bg-gray-900/90 px-4 py-3 text-base text-white placeholder-gray-500 outline-none transition focus:border-cyan-400"
          />
          {selected && (
            <button
              onClick={() => {
                setSelected(null);
                setQuery("");
                inputRef.current?.focus();
              }}
              className="absolute top-1/2 right-3 -translate-y-1/2 text-gray-400 hover:text-white"
            >
              &times;
            </button>
          )}
        </div>

        {showResults && (
          <motion.div
            className="w-full overflow-hidden rounded-lg border border-gray-700 bg-gray-900/95 shadow-lg shadow-black/40"
            initial={{ opacity: 0, y: -4 }}
            animate={{ opacity: 1, y: 0 }}
            transition={{ duration: 0.15 }}
          >
            {matches.map((name, i) => (
              <button
                key={name}
                className={`w-full px-4 py-2 text-left text-sm transition ${
                  i === highlightIndex
                    ? "bg-cyan-500/20 text-white"
                    : "text-gray-300 hover:bg-gray-800 hover:text-white"
                } ${i < matches.length - 1 ? "border-b border-gray-800/60" : ""}`}
                onClick={() => handleSelect(name)}
                onMouseEnter={() => setHighlightIndex(i)}
              >
                <HighlightedName name={name} query={query} />
              </button>
            ))}
          </motion.div>
        )}

        {query.length >= 2 && matches.length === 0 && !selected && (
          <p className="text-sm text-gray-500">No cards found</p>
        )}

        {selected && (
          <motion.div
            className="mt-2 rounded-lg border border-emerald-500/40 bg-emerald-500/10 px-6 py-3 text-lg font-semibold text-emerald-300"
            initial={{ opacity: 0, scale: 0.95 }}
            animate={{ opacity: 1, scale: 1 }}
            transition={{ duration: 0.2 }}
          >
            {selected}
          </motion.div>
        )}
      </div>
    </ChoiceOverlay>
  );
}

/** Highlights the matching portion of a card name. */
function HighlightedName({ name, query }: { name: string; query: string }) {
  const idx = name.toLowerCase().indexOf(query.toLowerCase());
  if (idx === -1) return <>{name}</>;
  return (
    <>
      {name.slice(0, idx)}
      <span className="font-semibold text-cyan-300">
        {name.slice(idx, idx + query.length)}
      </span>
      {name.slice(idx + query.length)}
    </>
  );
}

function ButtonGrid({ data, typeKey }: { data: NamedChoice["data"]; typeKey: string }) {
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<string | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({ type: "ChooseOption", data: { choice: selected } });
    }
  }, [dispatch, selected]);

  const title = CHOICE_TYPE_LABELS[typeKey] ?? "Make a Choice";

  return (
    <ChoiceOverlay
      title={title}
      subtitle="Select one option"
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-3xl"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected === null} />}
    >
      <div className="mx-auto mb-6 flex w-fit max-w-3xl flex-wrap items-center justify-center gap-3 sm:mb-10">
        {data.options.map((option, index) => {
          const isSelected = selected === option;
          return (
            <motion.button
              key={option}
              className={`min-h-11 rounded-lg border-2 px-4 py-3 text-sm font-semibold transition sm:px-5 sm:text-base ${
                isSelected
                  ? "border-emerald-400 bg-emerald-500/30 text-white"
                  : "border-gray-600 bg-gray-800/80 text-gray-300 hover:border-gray-400 hover:text-white"
              }`}
              initial={{ opacity: 0, y: 20, scale: 0.95 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              transition={{ delay: 0.05 + index * 0.03, duration: 0.25 }}
              whileHover={{ scale: 1.05 }}
              onClick={() => setSelected(isSelected ? null : option)}
            >
              {option}
            </motion.button>
          );
        })}
      </div>
    </ChoiceOverlay>
  );
}
