// @vitest-environment node
import type { ConfigEnv, UserConfigFnObject } from "vite";
import type { VitePWAOptions } from "vite-plugin-pwa";
import { describe, expect, it, vi } from "vitest";

// The production build hands these Workbox options to vite-plugin-pwa, which
// writes them into the generated service worker. Capture them at that
// boundary so the rule under test is the one that ships.
const captured = vi.hoisted(() => ({ options: undefined as Partial<VitePWAOptions> | undefined }));

vi.mock("vite-plugin-pwa", () => ({
  VitePWA: (options: Partial<VitePWAOptions>) => {
    captured.options = options;
    return [];
  },
}));

import viteConfig from "../../../vite.config";

type RuntimeCaching = NonNullable<NonNullable<VitePWAOptions["workbox"]>["runtimeCaching"]>[number];
type MatchCallback = (options: { sameOrigin: boolean; url: URL; request: { destination: string } }) => boolean;

async function workboxOptions() {
  captured.options = undefined;
  await (viteConfig as UserConfigFnObject)({ command: "build", mode: "production" } as ConfigEnv);
  // Assigned by the VitePWA mock during the call above.
  const options = captured.options as Partial<VitePWAOptions> | undefined;
  const workbox = options?.workbox;
  if (!workbox) throw new Error("vite.config.ts did not configure Workbox");
  return workbox;
}

/**
 * The first runtime rule Workbox would route this script request to, from a
 * page on `origin` (Workbox tries the rules in order).
 */
function routeFor(runtimeCaching: readonly RuntimeCaching[], href: string, origin: string): RuntimeCaching | undefined {
  const url = new URL(href);
  const sameOrigin = url.origin === origin;
  const request = { destination: "script" };
  return runtimeCaching.find(({ urlPattern }) => {
    if (typeof urlPattern === "function") return (urlPattern as MatchCallback)({ sameOrigin, url, request });
    if (urlPattern instanceof RegExp) {
      // Workbox's RegExpRoute only accepts a cross-origin match at index 0.
      const match = urlPattern.exec(url.href);
      return match !== null && (sameOrigin || match.index === 0);
    }
    return urlPattern === url.href || urlPattern === url.pathname;
  });
}

const ORIGIN = "https://self-hosted.example";

describe("the deployment's /config.js under the service worker", () => {
  it("is fetched network-first so an offline start keeps the last deployed copy", async () => {
    const { globIgnores = [], runtimeCaching = [] } = await workboxOptions();

    // Never precached: a precached copy would pin the build-time placeholder.
    expect(globIgnores).toContain("**/config.js");

    const rule = routeFor(runtimeCaching, `${ORIGIN}/config.js`, ORIGIN);
    expect(rule?.handler).toBe("NetworkFirst");
    // A network timeout would serve a stale project while the network is up.
    expect(rule?.options?.networkTimeoutSeconds).toBeUndefined();
  });

  it.each([
    ["another origin's config.js", "https://elsewhere.example/config.js"],
    ["a nested config.js", `${ORIGIN}/assets/config.js`],
  ])("does not cache %s as the deployment's config", async (_label, href) => {
    const { runtimeCaching = [] } = await workboxOptions();
    const deploymentConfigRule = routeFor(runtimeCaching, `${ORIGIN}/config.js`, ORIGIN);
    expect(deploymentConfigRule).toBeDefined();
    expect(routeFor(runtimeCaching, href, ORIGIN)).not.toBe(deploymentConfigRule);
  });
});
