import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { GameFormat } from "../../adapter/types";
import { listSavedDeckNames, loadSavedDeck } from "../../constants/storage";
import { evaluateDeckCompatibility } from "../../services/deckCompatibility";
import { deckSupplyForFormat, type DeckSupply } from "../../services/engineRuntime";
import { DEFAULT_PILE_SOURCE, type PileChoice, type PileSource } from "../../services/pileSource";

/**
 * The engine's deck supply for `format`, or `null` until it resolves for exactly
 * this format. A failed lookup answers `"PlayerBuilt"`, which offers no pile.
 */
function useDeckSupply(format: GameFormat | null): DeckSupply | null {
  const [answer, setAnswer] = useState<{ format: GameFormat; supply: DeckSupply } | null>(null);
  useEffect(() => {
    if (format === null) return;
    let cancelled = false;
    void (async () => {
      let supply: DeckSupply;
      try {
        supply = await deckSupplyForFormat(format);
      } catch {
        supply = "PlayerBuilt";
      }
      if (!cancelled) setAnswer({ format, supply });
    })();
    return () => {
      cancelled = true;
    };
  }, [format]);
  return format !== null && answer?.format === format ? answer.supply : null;
}

const DEFAULT_OPTION = "";

interface Props {
  format: GameFormat | null;
  value: PileSource;
  onChange: (next: PileChoice) => void;
}

/** The host's pile picker; renders only when the engine says the host supplies the pile. */
export function PileSourceChoice({ format, value, onChange }: Props) {
  const { t } = useTranslation("menu");
  const supply = useDeckSupply(format);
  const savedNames = useMemo(() => listSavedDeckNames(), []);
  const [reasons, setReasons] = useState<{ name: string; reasons: string[] } | null>(null);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  const pileName = value.type === "SavedDeck" ? value.name : null;
  useEffect(() => {
    if (pileName === null || format === null) return;
    let cancelled = false;
    void (async () => {
      const deck = loadSavedDeck(pileName);
      const result = deck ? await evaluateDeckCompatibility(deck, { selectedFormat: format }) : null;
      if (cancelled) return;
      const legal = result !== null && result.selected_format_compatible !== false;
      setReasons({ name: pileName, reasons: result?.selected_format_reasons ?? [] });
      onChangeRef.current({ source: { type: "SavedDeck", name: pileName }, legal });
    })();
    return () => {
      cancelled = true;
    };
  }, [pileName, format]);

  if (supply !== "HostPile") return null;

  const shownReasons = pileName !== null && reasons?.name === pileName ? reasons.reasons : [];
  return (
    <label className="flex flex-col gap-1">
      <span className="text-xs text-slate-400">{t("pileSource.label")}</span>
      <select
        aria-label={t("pileSource.label")}
        value={pileName ?? DEFAULT_OPTION}
        onChange={(e) => {
          const name = e.target.value;
          onChange(
            name === DEFAULT_OPTION
              ? { source: DEFAULT_PILE_SOURCE, legal: true }
              : { source: { type: "SavedDeck", name }, legal: false },
          );
        }}
        className="rounded-lg border border-gray-700 bg-gray-800/60 px-2 py-1.5 text-sm text-white"
      >
        <option value={DEFAULT_OPTION}>{t("pileSource.default")}</option>
        {savedNames.map((name) => (
          <option key={name} value={name}>
            {name}
          </option>
        ))}
      </select>
      {shownReasons.length > 0 && (
        <ul role="alert" className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-200">
          {shownReasons.map((reason) => (
            <li key={reason}>{reason}</li>
          ))}
        </ul>
      )}
    </label>
  );
}
