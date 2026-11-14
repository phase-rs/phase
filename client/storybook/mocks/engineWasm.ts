/**
 * Stand-in for the wasm-bindgen glue under `client/src/wasm/`, which wasm-pack
 * generates and `.gitignore` excludes — a fresh checkout has only the `.d.ts`
 * declarations. Both `@wasm/engine` and `@wasm/draft` resolve here.
 *
 * The catalog renders components against props, never against a live game, so
 * no story needs the engine. Every call site reaches the bundle through
 * `await import(...)`, so this module is only ever evaluated if something in a
 * story reached for game state — which the frontend is not supposed to do
 * (see "The frontend is a display layer" in CLAUDE.md). Failing loudly there is
 * the point: a silent no-op would let a story quietly depend on engine state.
 */
const UNAVAILABLE =
  "The Storybook catalog does not load the WASM engine. A story that needs " +
  "game state should take it as props instead.";

export function initSync(): never {
  throw new Error(UNAVAILABLE);
}

export default function init(): never {
  throw new Error(UNAVAILABLE);
}
