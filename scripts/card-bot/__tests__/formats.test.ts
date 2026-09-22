import { describe, expect, test } from "bun:test";
import { join } from "node:path";

import { FORMAT_REGISTRY } from "../../../client/src/data/formatRegistry";
import { findFormat, FORMATS, MAX_SEATS, P2P_MAX_PEERS, seatCap } from "../formats";
import { roomName } from "../lfgView";

describe("FORMATS mirrors the client format registry", () => {
  test("deep-equals the registry's projection", () => {
    const projection = FORMAT_REGISTRY.map((m) => ({
      format: m.format,
      label: m.label,
      min_players: m.default_config.min_players,
      max_players: m.default_config.max_players,
    }));
    // Reach guard: player bounds really come from default_config (2HG seats 4).
    expect(projection.some((row) => row.min_players === 4)).toBe(true);
    expect(FORMATS).toEqual(projection);
  });

  test("fits Discord's 25-choice cap", () => {
    expect(FORMATS.length).toBeLessThanOrEqual(25);
  });

  test("P2P_MAX_PEERS equals HostSetup's", async () => {
    const hostSetup = await Bun.file(
      join(import.meta.dir, "../../../client/src/components/lobby/HostSetup.tsx"),
    ).text();
    const matches = [...hostSetup.matchAll(/const P2P_MAX_PEERS = (\d+)/g)];
    expect(matches).toHaveLength(1);
    expect(P2P_MAX_PEERS).toBe(Number(matches[0][1]));
  });

  test("every room name fits HostSetup's 40-char room field", () => {
    for (const format of FORMATS) expect(roomName(format).length).toBeLessThanOrEqual(40);
  });

  test("seat caps: P2P is capped at P2P_MAX_PEERS, a server at the format maximum", () => {
    const draft = findFormat("CommanderDraft")!;
    expect(draft.max_players).toBe(8);
    expect(seatCap(draft, "p2p")).toBe(P2P_MAX_PEERS);
    expect(seatCap(draft, "server")).toBe(8);
    expect(seatCap(findFormat("Standard")!, "p2p")).toBe(2);
    expect(MAX_SEATS).toBe(8);
    expect(findFormat("NotAFormat")).toBeUndefined();
  });
});
