import type { DeckPrinting, DeckSection } from "../adapter/types";
import { getSharedAdapter } from "../adapter/wasm-adapter";
import type { SourcePrinting } from "../hooks/useCardImage";
import {
  usePreferencesStore,
  type ArtChainEntry,
  type CardArtOverride,
} from "../stores/preferencesStore";
import { selectedPrinting } from "./artSelection";
import { expandParsedDeck, type ExpandedDeck, type ParsedDeck } from "./deckParser";
import { getCardPrintings, type PrintingEntry } from "./scryfall";

export type SubmissionArtSnapshot = Readonly<{
  artChain: ArtChainEntry[];
  artOverrides: Record<string, CardArtOverride>;
}>;

type CardSection = Exclude<DeckSection, "unknown">;
export type SubmissionDeck = {
  main_deck: string[];
  sideboard: string[];
  sticker_sheets?: string[];
  printings?: DeckPrinting[];
} & Partial<Record<Exclude<CardSection, "main_deck" | "sideboard">, string[]>>;
export type SubmittedDeck<T> = T & { printings?: DeckPrinting[] };

const CARD_SECTIONS = [
  "main_deck", "sideboard", "commander", "companion", "attraction_deck",
  "planar_deck", "scheme_deck", "contraption_deck", "signature_spell",
] as const satisfies readonly CardSection[];
const PREPARATION_MS = 5000;

type EntryContext = {
  section: CardSection;
  name: string;
  count: number;
  source?: SourcePrinting;
};

/** Bind once at fresh submission; pass this owned snapshot to every seat in a batch. */
export function captureSubmissionArtPreferences(): SubmissionArtSnapshot {
  const { artChain, artOverrides } = usePreferencesStore.getState();
  return {
    artChain: artChain.map((entry) => ({ ...entry })),
    artOverrides: Object.fromEntries(
      Object.entries(artOverrides).map(([id, override]) => [id, { ...override }]),
    ),
  };
}

function copySections<T extends SubmissionDeck>(deck: T): T {
  const copy = { ...deck };
  for (const section of [...CARD_SECTIONS, "sticker_sheets"] as const) {
    const names = deck[section];
    if (names !== undefined) copy[section] = [...names];
  }
  return copy;
}

function listContexts(deck: SubmissionDeck): EntryContext[] {
  return CARD_SECTIONS.flatMap((section) =>
    (deck[section] ?? []).map((name) => ({ section, name, count: 1 })),
  );
}

/**
 * Preserve parser name projection, but own every section and entry source before
 * awaiting metadata. String-only sections deliberately have no imported source.
 * Import normalization can merge same-name entries and retain only the first
 * source (d-15-10); submission cannot recover sources absent from ParsedDeck.
 */
export function submitParsedDeck(
  deck: ParsedDeck,
  snapshot: SubmissionArtSnapshot = captureSubmissionArtPreferences(),
): Promise<SubmittedDeck<ExpandedDeck>> {
  const expanded = copySections(expandParsedDeck(deck));
  const contexts: EntryContext[] = [];
  for (const [section, entries] of [["main_deck", deck.main], ["sideboard", deck.sideboard]] as const) {
    for (const entry of entries) {
      contexts.push({
        section, name: entry.name, count: entry.count,
        source: entry.sourcePrinting ? { ...entry.sourcePrinting } : undefined,
      });
    }
  }
  contexts.push(...listContexts(expanded).filter(({ section }) => section !== "main_deck" && section !== "sideboard"));
  return annotate(expanded, contexts, snapshot);
}

/**
 * Decorate a fresh name list without modifying its persisted payload or extras.
 * Existing sidecars (including []) are authoritative and pass through untouched.
 */
export function submitDeckList<T extends SubmissionDeck>(
  deck: T,
  snapshot?: SubmissionArtSnapshot,
): Promise<SubmittedDeck<T>> {
  if (deck.printings !== undefined) return Promise.resolve(deck);
  const captured = snapshot ?? captureSubmissionArtPreferences();
  const copy = copySections(deck);
  return annotate(copy, listContexts(copy), captured);
}

/** Optional art has one deadline, including the engine's complete bounds check. */
async function annotate<T extends SubmissionDeck>(
  deck: T,
  contexts: EntryContext[],
  snapshot: SubmissionArtSnapshot,
): Promise<SubmittedDeck<T>> {
  if (snapshot.artChain.length === 0 && Object.keys(snapshot.artOverrides).length === 0
      && !contexts.some(({ source }) => source !== undefined)) return deck;

  const deadline = Date.now() + PREPARATION_MS;
  let timer: number | undefined;
  const expired = () => Date.now() >= deadline;
  const timeout = new Promise<T>((resolve) => {
    timer = window.setTimeout(() => resolve(deck), PREPARATION_MS);
  });
  async function prepare(): Promise<SubmittedDeck<T>> {
    const adapter = getSharedAdapter();
    const catalogs = new Map<string, Promise<PrintingEntry[]>>();
    const identities = new Map<string, Promise<{ oracleId: string; printings: PrintingEntry[] } | null>>();
    for (const { name, count } of contexts) {
      if (count <= 0 || identities.has(name)) continue;
      identities.set(name, (async () => {
        try {
          const face = await adapter.getCardFaceData(name);
          if (expired() || !face || typeof face !== "object"
              || !("scryfall_oracle_id" in face)
              || typeof face.scryfall_oracle_id !== "string" || !face.scryfall_oracle_id) return null;
          const oracleId = face.scryfall_oracle_id;
          let catalog = catalogs.get(oracleId);
          if (!catalog) {
            catalog = getCardPrintings(oracleId);
            catalogs.set(oracleId, catalog);
          }
          return { oracleId, printings: await catalog };
        } catch {
          return null;
        }
      })());
    }
    await Promise.all(identities.values());
    if (expired()) return deck;
    const rows: DeckPrinting[] = [];
    for (const context of contexts) {
      const identity = await identities.get(context.name);
      if (!identity) continue;
      for (let copy = 0; copy < context.count; copy++) {
        const match = selectedPrinting(
          identity.oracleId, identity.printings, snapshot.artChain, snapshot.artOverrides, context.source,
        );
        if (!match) continue;
        const last = rows[rows.length - 1];
        if (last && last.section === context.section && last.name === context.name
            && last.printing.set_code === match.set && last.printing.collector_number === match.collector_number) {
          last.count++;
        } else {
          rows.push({
            section: context.section, name: context.name, count: 1,
            printing: { set_code: match.set, collector_number: match.collector_number },
          });
        }
      }
    }
    if (rows.length === 0 || expired()) return deck;
    // The engine alone owns row and UTF-8 byte bounds. Never truncate a rejection.
    await adapter.checkDeckPrintingsBounds(rows);
    return expired() ? deck : { ...deck, printings: rows };
  }

  try {
    // Late work owns only private rows, never a returned submission's arrays.
    return await Promise.race([prepare().catch(() => deck), timeout]);
  } finally {
    clearTimeout(timer);
  }
}
