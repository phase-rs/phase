import { expect } from "vitest";

/**
 * A **bare dotted key path** — what i18next renders when a key is missing.
 *
 * Measured on this tree: `t("standings.doesNotExist")` renders
 * `"standings.doesNotExist"`, WITHOUT the namespace prefix. An assertion
 * looking for a `"tournament.…"` string therefore can never fire, which is
 * why this matches the bare path instead.
 */
const RAW_KEY_PATH = /^[a-z][A-Za-z0-9]*(?:\.[A-Za-z0-9]+)+$/;

function textNodeValues(root: Node): string[] {
  const values: string[] = [];
  for (const node of Array.from(root.childNodes)) {
    if (node.nodeType === 3) {
      const text = (node.nodeValue ?? "").trim();
      if (text.length > 0) values.push(text);
    } else {
      values.push(...textNodeValues(node));
    }
  }
  return values;
}

/**
 * Asserts no rendered text node is an unresolved catalog key path.
 *
 * Carries its own paired positive control: the detector is checked against a
 * known missing-key render and against real English copy on every call, so a
 * regex that silently stopped matching anything cannot let this pass.
 *
 * Pair every use with {@link expectCatalogValuePresent} — this assertion is
 * vacuously satisfiable by a component that rendered nothing at all.
 */
export function expectNoRawKeyPaths(container: HTMLElement): void {
  expect(RAW_KEY_PATH.test("standings.doesNotExist")).toBe(true);
  expect(RAW_KEY_PATH.test("Standings")).toBe(false);

  const offenders = textNodeValues(container).filter((text) =>
    RAW_KEY_PATH.test(text),
  );
  expect(offenders).toEqual([]);
}

/**
 * The positive reach-guard for {@link expectNoRawKeyPaths}: at least one known
 * English catalog value really did render.
 */
export function expectCatalogValuePresent(container: HTMLElement, value: string): void {
  expect(container.textContent).toContain(value);
}
