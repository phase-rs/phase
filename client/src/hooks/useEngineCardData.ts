import { useEffect, useState } from "react";

import {
  type CardRuling,
  ensureCardLocale,
} from "../services/engineRuntime";
import { getSharedAdapter } from "../adapter/wasm-adapter";
import { usePreferencesStore } from "../stores/preferencesStore";

/**
 * Engine-parsed card face data returned from WASM.
 * Mirrors the Rust `CardFace` struct — same shape as card-data.json entries.
 */
export interface EngineCardFace {
  name: string;
  card_type: { supertypes: string[]; core_types: string[]; subtypes: string[] };
  oracle_text?: string | null;
  keywords: unknown[];
  abilities: unknown[];
  triggers: unknown[];
  static_abilities: unknown[];
  replacements: unknown[];
  /**
   * Overlay-only: the selected locale's pre-formatted type line from the content
   * sidecar (e.g. "Spontanzauber"). Absent for English and untranslated cards —
   * callers fall back to formatting the structured `card_type`. Not part of the
   * WASM `CardFace` shape; populated solely by the content-i18n overlay below.
   */
  localized_type_line?: string | null;
}

/**
 * A node in the engine's hierarchical parse tree for a single card.
 * Mirrors the Rust `ParsedItem` struct from `coverage.rs`.
 */
export interface ParsedItem {
  category: "keyword" | "ability" | "trigger" | "static" | "replacement" | "cost";
  label: string;
  source_text?: string | null;
  supported: boolean;
  details?: [string, string][];
  children?: ParsedItem[];
}

/**
 * Looks up a card's engine-parsed face data from the WASM card database.
 * Returns null while loading or if the card is not found.
 *
 * The card database must already be loaded before lookup — the engine runtime
 * wrapper ensures that as a prerequisite, then performs the query.
 */
export function useEngineCardData(cardName: string | null): EngineCardFace | null {
  const language = usePreferencesStore((s) => s.language);
  const [data, setData] = useState<EngineCardFace | null>(null);

  useEffect(() => {
    if (!cardName) {
      setData(null);
      return;
    }

    let cancelled = false;

    void (async () => {
      const result = (await getSharedAdapter().getCardFaceData(cardName)) as EngineCardFace | null;
      if (cancelled) return;
      if (!result || language === "en") {
        setData(result ?? null);
        return;
      }
      // Content i18n: overlay the engine's English name/oracle text with the
      // selected locale's official MTGJSON text, per-field falling back to
      // English. Identity (lookup key) stays English; only display strings change.
      const localeMap = await ensureCardLocale(language);
      if (cancelled) return;
      const localized = localeMap.get(result.name.toLowerCase());
      setData(
        localized
          ? {
              ...result,
              name: localized.name ?? result.name,
              oracle_text: localized.oracle_text ?? result.oracle_text,
              localized_type_line: localized.type_line ?? undefined,
            }
          : result,
      );
    })().catch(() => {
      if (!cancelled) setData(null);
    });

    return () => { cancelled = true; };
  }, [cardName, language]);

  return data;
}

/**
 * Returns a card's name in the selected UI language, falling back to the English
 * name. For name-only display sites (hand, battlefield face, deck rows) that
 * don't need the full engine face data. `name` must be the canonical English
 * card name — the engine's identity key, which stays English everywhere.
 */
export function useLocalizedCardName(name: string | null): string | null {
  const language = usePreferencesStore((s) => s.language);
  const [localized, setLocalized] = useState<string | null>(name);

  useEffect(() => {
    setLocalized(name);
    if (!name || language === "en") return;

    let cancelled = false;
    ensureCardLocale(language)
      .then((map) => {
        if (cancelled) return;
        const loc = map.get(name.toLowerCase());
        if (loc?.name) setLocalized(loc.name);
      })
      .catch(() => {
        /* keep English fallback */
      });

    return () => { cancelled = true; };
  }, [name, language]);

  return localized;
}

/**
 * Returns the hierarchical parse tree for a card, with per-item support status.
 * Each item includes category, label, source text, a `supported` boolean,
 * structured detail key-value pairs, and recursive children.
 *
 * This is the engine's authoritative view of what was parsed and what wasn't.
 */
export function useCardParseDetails(cardName: string | null): ParsedItem[] | null {
  const [items, setItems] = useState<ParsedItem[] | null>(null);

  useEffect(() => {
    if (!cardName) {
      setItems(null);
      return;
    }

    let cancelled = false;

    getSharedAdapter().getCardParseDetails(cardName)
      .then((result) => {
        if (cancelled) return;
        setItems((result as ParsedItem[] | null) ?? null);
      })
      .catch(() => {
        if (cancelled) return;
        setItems(null);
      });

    return () => { cancelled = true; };
  }, [cardName]);

  return items;
}

/**
 * Returns official WotC rulings for a card. Empty array while loading, or when
 * the card has no rulings, or for back faces of multi-face cards (rulings are
 * attached to the front face only).
 */
export function useCardRulings(cardName: string | null): CardRuling[] {
  const [rulings, setRulings] = useState<CardRuling[]>([]);

  useEffect(() => {
    if (!cardName) {
      setRulings([]);
      return;
    }

    let cancelled = false;

    getSharedAdapter().getCardRulings(cardName)
      .then((result) => {
        if (cancelled) return;
        setRulings((result as CardRuling[] | null) ?? []);
      })
      .catch(() => {
        if (cancelled) return;
        setRulings([]);
      });

    return () => { cancelled = true; };
  }, [cardName]);

  return rulings;
}
