/**
 * Exhaustiveness guard for discriminated unions. Place in a switch `default:`
 * (or after an if-chain) over a closed engine union — TypeScript narrows the
 * argument to `never` only when every variant is handled, so adding a new
 * variant upstream turns a silent fall-through into a compile error.
 *
 * Use ONLY where the switch is meant to be exhaustive. Switches that
 * intentionally handle a subset of an engine union should keep an explicit
 * `default` instead — do not force them to handle variants the UI ignores.
 *
 *   switch (segment.type) {
 *     case "Text": return ...;
 *     // ...every other variant...
 *     default: return assertNever(segment);
 *   }
 */
export function assertNever(value: never): never {
  throw new Error(`Unhandled discriminated-union variant: ${JSON.stringify(value)}`);
}
