// @vitest-environment node
import path from "node:path";

import { loadConfigFromFile, type Plugin, type PluginOption } from "vite";
import { describe, expect, it, vi } from "vitest";

const CLIENT = path.resolve(__dirname, "../../..");

/** The production build's plugins, flattened out of any nested presets. */
async function buildConfig() {
  const loaded = await loadConfigFromFile(
    { command: "build", mode: "production" },
    path.join(CLIENT, "vite.config.ts"),
    CLIENT,
    "silent",
  );
  if (!loaded) throw new Error("vite.config.ts did not load");
  const options = (loaded.config.plugins ?? []) as PluginOption[];
  const plugins = (await Promise.all(options.flat(Infinity as 1))).flat(Infinity as 1) as unknown[];
  const named = plugins.filter(
    (plugin): plugin is Plugin => typeof plugin === "object" && plugin !== null && "name" in plugin,
  );
  return { define: loaded.config.define ?? {}, plugins: named };
}

function emittedAssets(plugin: Plugin): Array<{ type: string; fileName: string; source: string }> {
  const emitFile = vi.fn();
  const hook = plugin.generateBundle as unknown as (this: unknown) => void;
  hook.call({ emitFile });
  return emitFile.mock.calls.map(([file]) => file as { type: string; fileName: string; source: string });
}

describe("build manifest", () => {
  it("emits /build.json carrying the bundle's __BUILD_HASH__", async () => {
    const { define, plugins } = await buildConfig();
    // Positive control for the traversal: the sibling marker plugin is found
    // by the same lookup.
    expect(plugins.find((plugin) => plugin.name === "offline-shell-marker")).toBeDefined();

    const manifest = plugins.find((plugin) => plugin.name === "build-manifest");
    expect(manifest).toBeDefined();
    expect(manifest!.apply).toBe("build");

    const assets = emittedAssets(manifest!);
    expect(assets).toHaveLength(1);
    expect(assets[0].type).toBe("asset");
    expect(assets[0].fileName).toBe("build.json");
    expect(JSON.parse(assets[0].source)).toEqual({
      build: JSON.parse(define.__BUILD_HASH__ as string),
    });
  });
});
