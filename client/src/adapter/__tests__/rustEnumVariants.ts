import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { expect } from "vitest";

export function repoRoot(): string {
  return resolve(dirname(fileURLToPath(import.meta.url)), "../../../..");
}

export function rustEnumVariants(source: string, enumName: string): string[] {
  const enumStart = source.indexOf(`pub enum ${enumName}`);
  expect(enumStart, `${enumName} enum should exist`).toBeGreaterThanOrEqual(0);

  const bodyStart = source.indexOf("{", enumStart);
  expect(bodyStart, `${enumName} enum body should start`).toBeGreaterThanOrEqual(0);

  let depth = 0;
  for (let index = bodyStart; index < source.length; index += 1) {
    if (source[index] === "{") depth += 1;
    if (source[index] === "}") {
      depth -= 1;
      if (depth === 0) {
        return Array.from(
          source
            .slice(bodyStart + 1, index)
            .matchAll(/^ {4}([A-Z][A-Za-z0-9]+)\s*(?:\{|\(|,)/gm),
          (match) => match[1],
        );
      }
    }
  }

  throw new Error(`${enumName} enum body should close`);
}
