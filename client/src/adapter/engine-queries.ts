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

import type { FormatConfig, GameFormat } from "./types";

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
  companionCandidates: {
    needsCardDb: true,
    run: (engine: EngineModule, request: unknown) => engine.companionCandidates(request),
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
