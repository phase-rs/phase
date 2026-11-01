import { describe, expect, test } from "bun:test";

import { FORMATS } from "../formats";
import { LfgRoleCache, lfgRoleName } from "../lfgRoles";

describe("LFG format roles", () => {
  test("every playable format has a distinct role name", () => {
    const names = FORMATS.map(lfgRoleName);
    expect(new Set(names).size).toBe(FORMATS.length);
    expect(names).toContain("LFG Tiny Leaders: Reborn");
    expect(names).toContain("LFG Momir's Madness");
  });

  test("resolves role IDs by exact format label and refreshes after the cache expires", async () => {
    let now = 60_000;
    let requests = 0;
    const fetchRoles = async () => {
      requests++;
      return Response.json([{ id: String(requests), name: "LFG Commander" }]);
    };
    const cache = new LfgRoleCache("guild", "token", fetchRoles, () => now);
    const commander = FORMATS.find((format) => format.format === "Commander")!;
    const modern = FORMATS.find((format) => format.format === "Modern")!;

    expect(await cache.resolve("other-guild", commander)).toBeUndefined();
    expect(requests).toBe(0);
    expect(await cache.resolve("guild", commander)).toBe("1");
    expect(await cache.resolve("guild", modern)).toBeUndefined();
    expect(requests).toBe(1);
    now += 60_000;
    expect(await cache.resolve("guild", commander)).toBe("2");
    expect(requests).toBe(2);
  });
});
