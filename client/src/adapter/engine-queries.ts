/**
 * Stateless engine queries the UI asks outside a game: deck-construction
 * rules, format facts, card search and compiled-in catalogs. None of them reads
 * or writes game state.
 *
 * One table serves both execution sites. The engine worker runs an entry
 * against its WASM instance, and the main-thread fallback runs the same entry
 * against its own when no worker could start. UI code reaches these only
 * through `WasmAdapter.query`, so every answer comes from the one engine the
 * tab already owns. A second instance costs a 40 MB module compile and, once it
 * loads the card database, about 540 MB of WASM memory — enough on its own to
 * get the tab killed on iOS.
 */
import type * as EngineWasm from "@wasm/engine";

import type { ExpandedDeck } from "../services/deckParser";
import type { ChallengeDeck, FormatConfig, GameFormat } from "./types";

/**
 * CR 903.5a: does a request's main deck already CONTAIN the designated
 * commander cards, or are they held beside it?
 *
 * The two editors genuinely differ and neither is wrong. Constructed Commander
 * is commanders-OUTSIDE — `useDeckBuilder.handleSetCommander` moves the chosen
 * card out of `main`, so the 99 plus the commander make 100. A drafted
 * Commander pool is commanders-INSIDE — `validate_limited_deck` requires a copy
 * of every designated name IN `main_deck`, so the designation is a LABEL on a
 * deck card, never an extra card beside the deck.
 *
 * Declared by the caller rather than inferred, because both editors can run
 * under the same `CommanderDraft` format tag.
 */
export type CommanderDeckComposition = "commanders-inside" | "commanders-outside";

export type CommanderCandidatesRequest =
  Pick<ExpandedDeck, "main_deck" | "commander">
  & Partial<Omit<ExpandedDeck, "main_deck" | "commander">>
  & {
    selected_format?: GameFormat | null;
    selected_challenge_deck?: ChallengeDeck | null;
    draft_set_codes?: readonly string[];
  };

export type EngineModule = typeof EngineWasm;

interface EngineQuery {
  /** Whether the answer reads the card database, which must be loaded first. */
  readonly needsCardDb: boolean;
  readonly run: (engine: EngineModule, ...args: never[]) => unknown;
}

export const ENGINE_QUERIES = {
  searchCards: {
    needsCardDb: true,
    run: (engine: EngineModule, query: unknown) => engine.search_cards_js(query),
  },
  classifyDeck: {
    needsCardDb: true,
    run: (engine: EngineModule, cardNames: string[]) => engine.classify_deck_js(cardNames),
  },
  isCardCommanderEligible: {
    needsCardDb: true,
    run: (engine: EngineModule, name: string) => engine.is_card_commander_eligible(name),
  },
  isCardCommanderEligibleForFormat: {
    needsCardDb: true,
    run: (engine: EngineModule, name: string, format: GameFormat) =>
      engine.isCardCommanderEligibleForFormat(name, format),
  },
  commanderPartnerCandidates: {
    needsCardDb: true,
    run: (engine: EngineModule, firstCommander: string, candidates: string[], draftSetCodes: string[]) =>
      engine.commanderPartnerCandidates(firstCommander, candidates, draftSetCodes),
  },
  signatureSpellSelectionPolicy: {
    needsCardDb: true,
    run: (engine: EngineModule, request: unknown) => engine.signatureSpellSelectionPolicy(request),
  },
  commanderCandidates: {
    needsCardDb: true,
    run: (engine: EngineModule, request: CommanderCandidatesRequest, composition: CommanderDeckComposition) =>
      engine.commanderCandidates(request, composition),
  },
  companionCandidates: {
    needsCardDb: true,
    run: (engine: EngineModule, request: unknown) => engine.companionCandidates(request),
  },
  isCardCompanion: {
    needsCardDb: true,
    run: (engine: EngineModule, name: string) => engine.isCardCompanion(name),
  },
  maxDeckCopies: {
    needsCardDb: true,
    run: (engine: EngineModule, name: string, formatConfig: FormatConfig) =>
      engine.maxDeckCopies(name, formatConfig),
  },
  bestOfThreeCeilingForFormat: {
    needsCardDb: false,
    run: (engine: EngineModule, format: GameFormat) => engine.bestOfThreeCeilingForFormat(format),
  },
  deckSupplyForFormat: {
    needsCardDb: false,
    run: (engine: EngineModule, format: GameFormat) => engine.deckSupplyForFormat(format),
  },
  sideboardPolicyForFormat: {
    needsCardDb: false,
    run: (engine: EngineModule, formatConfig: FormatConfig) =>
      engine.sideboardPolicyForFormat(formatConfig),
  },
  listTokenPresets: {
    needsCardDb: false,
    run: (engine: EngineModule) => engine.list_token_presets_js(),
  },
  llmProviderCatalog: {
    needsCardDb: false,
    run: (engine: EngineModule) => engine.llmProviderCatalog(),
  },
  buildLlmProbeRequest: {
    needsCardDb: false,
    run: (engine: EngineModule, endpointJson: string) => engine.buildLlmProbeRequest(endpointJson),
  },
  validateLlmProbeResponse: {
    needsCardDb: false,
    run: (engine: EngineModule, provider: string, status: number, body: string) =>
      engine.validateLlmProbeResponse(provider, status, body),
  },
} as const satisfies Record<string, EngineQuery>;

export type EngineQueryName = keyof typeof ENGINE_QUERIES;

type QueryRun<K extends EngineQueryName> = (typeof ENGINE_QUERIES)[K]["run"];

export type EngineQueryArgs<K extends EngineQueryName> =
  QueryRun<K> extends (engine: EngineModule, ...args: infer A) => unknown ? A : never;

export type EngineQueryResult<K extends EngineQueryName> = ReturnType<QueryRun<K>>;

export function runEngineQuery<K extends EngineQueryName>(
  engine: EngineModule,
  name: K,
  args: EngineQueryArgs<K>,
): EngineQueryResult<K> {
  // TypeScript cannot correlate a generic key with its own table entry, so the
  // lookup is widened once here; the signature above keeps callers exact.
  const run = ENGINE_QUERIES[name].run as unknown as (
    engine: EngineModule,
    ...args: EngineQueryArgs<K>
  ) => EngineQueryResult<K>;
  return run(engine, ...args);
}
