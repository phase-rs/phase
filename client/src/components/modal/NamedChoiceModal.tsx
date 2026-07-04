import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { motion } from "framer-motion";

import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useSeatColor } from "../../hooks/useSeatColor.ts";
import { usePlayerId } from "../../hooks/usePlayerId.ts";
import { getCardNames } from "../../services/cardNames.ts";
import {
  getPlayerDisplayName,
  useMultiplayerStore,
} from "../../stores/multiplayerStore.ts";
import type { PlayerId, WaitingFor } from "../../adapter/types.ts";

type NamedChoice = Extract<WaitingFor, { type: "NamedChoice" }>;

/** Maps a ChoiceType key to its i18n leaf under `namedChoice.title.*`. */
const CHOICE_TYPE_TITLE_KEYS: Record<string, string> = {
  CreatureType: "creatureType",
  Color: "color",
  OddOrEven: "oddOrEven",
  BasicLandType: "basicLandType",
  CardType: "cardType",
  CardName: "cardName",
  LandType: "landType",
  Opponent: "opponent",
  Player: "player",
  TwoColors: "twoColors",
  NumberRange: "numberRange",
  Labeled: "labeled",
  Keyword: "keyword",
  CounterKind: "counterKind",
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
  const { t } = useTranslation("game");
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
    <ChoiceOverlay title={t("namedChoice.title.cardName")} subtitle={t("namedChoice.searchSubtitle")} footer={<ConfirmButton onClick={handleConfirm} disabled={!selected} />}>
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
            placeholder={t("namedChoice.searchPlaceholder")}
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
          <p className="text-sm text-gray-500">{t("namedChoice.noCardsFound")}</p>
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

/** Lists longer than this get a filter input so the player can narrow them
 *  (e.g. ~280 creature types, the keyword list) instead of scanning a wall of
 *  buttons. Short lists (colors, card types) render without the extra chrome. */
const FILTERABLE_OPTION_THRESHOLD = 12;

function ButtonGrid({ data, typeKey }: { data: NamedChoice["data"]; typeKey: string }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const [selected, setSelected] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const filterRef = useRef<HTMLInputElement>(null);

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({ type: "ChooseOption", data: { choice: selected } });
    }
  }, [dispatch, selected]);

  const titleKey = CHOICE_TYPE_TITLE_KEYS[typeKey];
  const title = titleKey
    ? t(`namedChoice.title.${titleKey}`)
    : t("namedChoice.title.fallback");
  const isPlayerChoice = typeKey === "Player" || typeKey === "Opponent";
  const showFilter = !isPlayerChoice && data.options.length > FILTERABLE_OPTION_THRESHOLD;

  const visibleOptions = useMemo(() => {
    if (!showFilter || query.trim() === "") return data.options;
    const lower = query.trim().toLowerCase();
    return data.options.filter((option) => option.toLowerCase().includes(lower));
  }, [showFilter, query, data.options]);

  return (
    <ChoiceOverlay
      title={title}
      subtitle={t("namedChoice.buttonSubtitle")}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-3xl"
      footer={<ConfirmButton onClick={handleConfirm} disabled={selected === null} />}
    >
      {showFilter && (
        <div className="mx-auto mb-4 w-full max-w-md">
          <input
            ref={filterRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("namedChoice.filterPlaceholder")}
            autoFocus
            className="w-full rounded-lg border-2 border-gray-600 bg-gray-900/90 px-4 py-2.5 text-base text-white placeholder-gray-500 outline-none transition focus:border-cyan-400"
          />
        </div>
      )}
      <div
        className={`mx-auto mb-6 max-w-3xl gap-3 sm:mb-10 ${
          isPlayerChoice
            ? "flex w-full min-w-[18rem] flex-col px-6"
            : "flex w-fit flex-wrap items-center justify-center"
        }`}
      >
        {visibleOptions.map((option, index) => {
          const isSelected = selected === option;
          const onClick = () => setSelected(isSelected ? null : option);
          if (isPlayerChoice) {
            return (
              <PlayerOptionButton
                key={option}
                option={option}
                isSelected={isSelected}
                index={index}
                onClick={onClick}
              />
            );
          }
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
              onClick={onClick}
            >
              {option}
            </motion.button>
          );
        })}
      </div>
      {showFilter && visibleOptions.length === 0 && (
        <p className="mb-6 text-center text-sm text-gray-500">{t("namedChoice.noOptionsMatch")}</p>
      )}
    </ChoiceOverlay>
  );
}

/** Player/Opponent option: avatar + display name + seat color accent. The
 *  engine sends `PlayerId.0.to_string()` (e.g. "0", "1") as the option id;
 *  we render presentation here and pass the raw id back on confirm. */
function PlayerOptionButton({
  option,
  isSelected,
  index,
  onClick,
}: {
  option: string;
  isSelected: boolean;
  index: number;
  onClick: () => void;
}) {
  const playerId = Number(option) as PlayerId;
  const myId = usePlayerId();
  const seatColor = useSeatColor(playerId);
  const avatarUrl = useMultiplayerStore((s) => s.playerAvatars.get(playerId) ?? null);
  const displayName = getPlayerDisplayName(playerId, myId);

  return (
    <motion.button
      type="button"
      className={`flex w-full min-h-14 items-center gap-3 rounded-lg border-2 px-3 py-2 text-left font-semibold transition sm:px-4 ${
        isSelected ? "bg-emerald-500/20 text-white" : "bg-gray-800/80 text-gray-200 hover:text-white"
      }`}
      style={{
        borderColor: isSelected ? "#34D399" : `${seatColor}cc`,
        boxShadow: isSelected
          ? `0 0 0 1px ${seatColor}88, 0 0 14px ${seatColor}55`
          : `0 0 0 1px ${seatColor}33`,
      }}
      initial={{ opacity: 0, y: 20, scale: 0.95 }}
      animate={{ opacity: 1, y: 0, scale: 1 }}
      transition={{ delay: 0.05 + index * 0.03, duration: 0.25 }}
      whileHover={{ scale: 1.04 }}
      onClick={onClick}
      aria-label={displayName}
    >
      <span
        className="relative h-10 w-9 shrink-0 overflow-hidden rounded-md border bg-slate-950"
        style={{ borderColor: `${seatColor}cc` }}
      >
        {avatarUrl ? (
          <img src={avatarUrl} alt="" className="h-full w-full object-cover" />
        ) : (
          <span
            className="flex h-full w-full items-center justify-center text-sm font-bold"
            style={{ color: seatColor }}
          >
            {displayName.slice(0, 1).toUpperCase()}
          </span>
        )}
        <span className="absolute inset-0 bg-gradient-to-b from-white/12 via-transparent to-black/35" />
      </span>
      <span className="text-sm sm:text-base" style={{ color: isSelected ? undefined : seatColor }}>
        {displayName}
      </span>
    </motion.button>
  );
}
