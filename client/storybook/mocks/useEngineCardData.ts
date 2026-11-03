import type * as RealModule from "../../src/hooks/useEngineCardData";

/**
 * Story double for `src/hooks/useEngineCardData.ts`.
 *
 * Every hook in the real module queries the WASM engine through the shared
 * adapter, and the wasm-bindgen glue under `client/src/wasm/` is gitignored —
 * a fresh checkout has only the `.d.ts` stubs, so importing the real module
 * would keep Storybook from booting until someone had built the engine.
 *
 * The catalog does not need engine lookups: a story that wants Oracle text
 * passes it as a prop, which is the same escape hatch `CardImage` already
 * offers its callers. So each hook here reports "nothing known", the state the
 * real hooks report while loading and for an unknown card.
 *
 * Every export is typed against the real module, so a signature change there
 * breaks `pnpm type-check` here instead of drifting silently.
 */

export const useEngineCardData: typeof RealModule.useEngineCardData = () => null;

/** Canonical English names are what stories pass in, so echo them back. */
export const useLocalizedCardName: typeof RealModule.useLocalizedCardName = (name) => name;

export const useCardParseDetails: typeof RealModule.useCardParseDetails = () => null;

export const useCardRulings: typeof RealModule.useCardRulings = () => [];
