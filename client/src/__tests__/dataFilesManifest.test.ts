import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

function defineToken(filename: string): string {
  return `__${filename.replace(/\.json$/, "").replace(/[.-]/g, "_").toUpperCase()}_URL__`;
}

describe("runtime data-file manifest", () => {
  it("has exactly one ambient Vite URL declaration per manifest entry", () => {
    const root = resolve(process.cwd(), "..");
    const manifest = JSON.parse(
      readFileSync(resolve(root, "data-files.json"), "utf8"),
    ) as string[];
    const declarations = readFileSync(resolve(root, "client/src/vite-env.d.ts"), "utf8");
    const block = declarations.match(
      /\/\/ Manifest-driven data file URLs\.[\s\S]*?\/\/ End manifest-driven data file URLs\./,
    )?.[0];
    if (!block) throw new Error("manifest-driven Vite declaration block is missing");
    const declaredTokens = [...block.matchAll(/declare const (__[A-Z0-9_]+_URL__): string;/g)]
      .map((match) => match[1]);
    const manifestTokens = manifest.map(defineToken);

    expect(manifest).toContain("combo-table.json");
    expect(declaredTokens.sort()).toEqual(manifestTokens.sort());
  });
});
