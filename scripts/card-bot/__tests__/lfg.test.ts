import { Database } from "bun:sqlite";
import { describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { findFormat, type LfgFormat } from "../formats";
import { LFG_IDLE_MS, type Lfg, LfgStore, mintGameCode, type NewLfg, type Outcome } from "../lfg";

const GUILD = "guild-1";
const T0 = 1_000_000_000_000;
const DAY_MS = 24 * 60 * 60_000;
const HOUR_MS = 60 * 60_000;
const CODE = /^[A-Z0-9]{6}$/;

const format = (key: string): LfgFormat => findFormat(key)!;

function input(overrides: Partial<NewLfg> = {}): NewLfg {
  return {
    guildId: GUILD,
    creatorId: "creator",
    format: format("Commander"),
    seats: 4,
    mode: "p2p",
    build: "release",
    server: null,
    ...overrides,
  };
}

function created(store: LfgStore, overrides: Partial<NewLfg> = {}, now = T0): Lfg {
  const result = store.create(input(overrides), now);
  if (result.kind !== "created") throw new Error(`create refused: ${result.reason}`);
  return result.lfg;
}

function lfgOf(outcome: Outcome): Lfg {
  if (outcome.lfg === null) throw new Error(`outcome ${outcome.kind} carries no lfg`);
  return outcome.lfg;
}

describe("join", () => {
  test("filling the last seat makes the LFG ready with a code", () => {
    const store = new LfgStore(":memory:");
    const lfg = created(store, { seats: 2 });
    expect(lfg.state).toBe("open");
    expect(lfg.code).toBeNull();

    const outcome = store.join(lfg.id, GUILD, "b", T0 + 1);
    expect(outcome).toMatchObject({ kind: "changed", becameReady: true });
    const ready = lfgOf(outcome);
    expect(ready.state).toBe("ready");
    expect(ready.code).toMatch(CODE);
    expect(ready.seated).toEqual(["creator", "b"]);
  });

  test("a non-filling join stays open; duplicate and late joins are refused", () => {
    const store = new LfgStore(":memory:");
    const lfg = created(store, { seats: 3 });
    expect(store.join(lfg.id, GUILD, "b", T0 + 1)).toMatchObject({ kind: "changed", becameReady: false });
    expect(store.join(lfg.id, GUILD, "b", T0 + 2)).toMatchObject({ kind: "refused", reason: "already_seated" });
    expect(store.join(lfg.id, GUILD, "c", T0 + 3)).toMatchObject({ kind: "changed", becameReady: true });
    expect(store.join(lfg.id, GUILD, "d", T0 + 4)).toMatchObject({ kind: "refused", reason: "not_open" });
  });
});

describe("leave", () => {
  test("a guest's leave frees their seat; the creator's cancels", () => {
    const store = new LfgStore(":memory:");
    const lfg = created(store);
    store.join(lfg.id, GUILD, "b", T0 + 1);
    const left = store.leave(lfg.id, GUILD, "b", T0 + 2);
    expect(left).toMatchObject({ kind: "changed", becameReady: false });
    expect(lfgOf(left)).toMatchObject({ state: "open", seated: ["creator"] });

    const cancelled = store.leave(lfg.id, GUILD, "creator", T0 + 3);
    expect(lfgOf(cancelled).state).toBe("cancelled");
    expect(store.join(lfg.id, GUILD, "c", T0 + 4)).toMatchObject({ kind: "ended" });
  });

  test("leaving without a seat, or after ready, is refused", () => {
    const store = new LfgStore(":memory:");
    const lfg = created(store, { seats: 2 });
    expect(store.leave(lfg.id, GUILD, "stranger", T0 + 1)).toMatchObject({ kind: "refused", reason: "not_seated" });
    store.join(lfg.id, GUILD, "b", T0 + 2);
    expect(store.leave(lfg.id, GUILD, "b", T0 + 3)).toMatchObject({ kind: "refused", reason: "not_open" });
  });
});

describe("authorization", () => {
  test("only the creator may start, and only with format.min_players seated", () => {
    const store = new LfgStore(":memory:");
    const lfg = created(store, { format: format("TwoHeadedGiant"), seats: 4 });
    store.join(lfg.id, GUILD, "b", T0 + 1);
    store.join(lfg.id, GUILD, "c", T0 + 2);
    expect(store.start(lfg.id, GUILD, "b", T0 + 3)).toMatchObject({ kind: "refused", reason: "not_creator" });
    expect(store.start(lfg.id, GUILD, "creator", T0 + 4)).toMatchObject({ kind: "refused", reason: "too_few" });

    // Positive pair: Commander (min 2) with 3 of 6 seated starts early.
    const commander = created(store, { creatorId: "other", seats: 6 });
    store.join(commander.id, GUILD, "x", T0 + 5);
    store.join(commander.id, GUILD, "y", T0 + 6);
    const started = store.start(commander.id, GUILD, "other", T0 + 7);
    expect(started).toMatchObject({ kind: "changed", becameReady: true });
    expect(lfgOf(started)).toMatchObject({ state: "ready", seated: ["other", "x", "y"] });
    expect(lfgOf(started).code).toMatch(CODE);
  });

  test("2HG: 3 seated is too few, 4 seated reaches ready", () => {
    const store = new LfgStore(":memory:");
    // seats > 4 is impossible for 2HG via /lfg, but the store only checks min_players on Start;
    // seats 5 keeps the 4th join from auto-filling so Start's own threshold is what is tested.
    const lfg = created(store, { format: format("TwoHeadedGiant"), seats: 5 });
    store.join(lfg.id, GUILD, "b", T0 + 1);
    store.join(lfg.id, GUILD, "c", T0 + 2);
    expect(store.start(lfg.id, GUILD, "creator", T0 + 3)).toMatchObject({ kind: "refused", reason: "too_few" });
    store.join(lfg.id, GUILD, "d", T0 + 4);
    expect(store.start(lfg.id, GUILD, "creator", T0 + 5)).toMatchObject({ kind: "changed", becameReady: true });
  });

  test("links go to seated users of a ready LFG only", () => {
    const store = new LfgStore(":memory:");
    const lfg = created(store, { seats: 2 });
    expect(store.linkFor(lfg.id, GUILD, "creator", T0 + 1)).toMatchObject({ kind: "refused", reason: "not_ready" });
    store.join(lfg.id, GUILD, "b", T0 + 2);
    expect(store.linkFor(lfg.id, GUILD, "stranger", T0 + 3)).toMatchObject({ kind: "refused", reason: "not_seated" });
    expect(store.linkFor(lfg.id, GUILD, "b", T0 + 3)).toMatchObject({ kind: "ready" });
    expect(store.linkFor(lfg.id, GUILD, "creator", T0 + 3)).toMatchObject({ kind: "ready" });
  });

  test("a click from another guild is ended(null) and changes nothing", () => {
    const store = new LfgStore(":memory:");
    const lfg = created(store, { seats: 3 });
    for (const act of [store.join, store.leave, store.start, store.linkFor]) {
      expect(act.call(store, lfg.id, "guild-2", "creator", T0 + 1)).toEqual({ kind: "ended", lfg: null });
    }
    // Row unchanged: still open with only the creator, not touched (a same-guild
    // click at the original idle boundary still acts).
    const next = store.join(lfg.id, GUILD, "b", T0 + LFG_IDLE_MS);
    expect(lfgOf(next)).toMatchObject({ state: "open", seated: ["creator", "b"] });
  });

  test("an unknown id is ended(null)", () => {
    const store = new LfgStore(":memory:");
    expect(store.join("missing", GUILD, "b", T0)).toEqual({ kind: "ended", lfg: null });
  });
});

describe("lazy expiry", () => {
  test("an open LFG idle past LFG_IDLE_MS expires on the next click; at the boundary it still acts", () => {
    const store = new LfgStore(":memory:");
    const a = created(store, { seats: 3 });
    expect(store.join(a.id, GUILD, "b", T0 + LFG_IDLE_MS)).toMatchObject({ kind: "changed" });

    const b = created(store, { creatorId: "creator-2", seats: 3 });
    const expired = store.join(b.id, GUILD, "c", T0 + LFG_IDLE_MS + 1);
    expect(expired.kind).toBe("ended");
    expect(lfgOf(expired)).toMatchObject({ state: "expired", seated: ["creator-2"] });
    // Idempotent: a further click on the expired post is ended again.
    expect(lfgOf(store.join(b.id, GUILD, "d", T0 + LFG_IDLE_MS + 2)).state).toBe("expired");
  });

  test("a ready LFG never idle-expires; it lives until the 24 h sweep", () => {
    const store = new LfgStore(":memory:");
    const lfg = created(store, { seats: 2 });
    const readyAt = T0 + 1;
    store.join(lfg.id, GUILD, "b", readyAt);
    expect(store.linkFor(lfg.id, GUILD, "creator", readyAt + LFG_IDLE_MS + 1)).toMatchObject({ kind: "ready" });
    const later = store.linkFor(lfg.id, GUILD, "b", readyAt + 2 * HOUR_MS);
    expect(later.kind).toBe("ready");
    expect(lfgOf(later).state).toBe("ready");
    expect(store.linkFor(lfg.id, GUILD, "b", readyAt + DAY_MS + 1)).toEqual({ kind: "ended", lfg: null });
  });
});

describe("one open LFG per creator per guild", () => {
  test("a second open is refused until the first idles out", () => {
    const store = new LfgStore(":memory:");
    const first = created(store);
    expect(store.create(input(), T0 + 1)).toEqual({ kind: "refused", reason: "has_open" });

    const second = store.create(input(), T0 + LFG_IDLE_MS + 1);
    expect(second.kind).toBe("created");
    expect(store.join(first.id, GUILD, "b", T0 + LFG_IDLE_MS + 2)).toMatchObject({
      kind: "ended",
      lfg: { state: "expired" },
    });
  });

  test("another guild, or after ready, is allowed", () => {
    const store = new LfgStore(":memory:");
    const first = created(store, { seats: 2 });
    expect(store.create(input({ guildId: "guild-2" }), T0 + 1).kind).toBe("created");
    store.join(first.id, GUILD, "b", T0 + 2);
    expect(store.create(input(), T0 + 3).kind).toBe("created");
  });
});

describe("sweep", () => {
  test("rows untouched for 24 h are deleted with their seats on any write; a 23 h row survives", () => {
    // A file database, so a second connection can read the raw seat table.
    const dir = mkdtempSync(join(tmpdir(), "lfg-sweep-"));
    const path = join(dir, "lfg.sqlite");
    let store: LfgStore | undefined;
    let raw: Database | undefined;
    // Cleanup runs even when an assertion fails: close both connections, then
    // remove the temp database.
    try {
      store = new LfgStore(path);
      const old = created(store, { seats: 3 });
      store.join(old.id, GUILD, "b", T0);
      const young = created(store, { creatorId: "creator-2", seats: 3 }, T0 + HOUR_MS);
      const now = T0 + DAY_MS + 1; // old is 24 h + 1 ms stale, young 23 h + 1 ms

      // The write that triggers the sweep is an unrelated create.
      created(store, { creatorId: "creator-3" }, now);
      expect(store.linkFor(old.id, GUILD, "b", now)).toEqual({ kind: "ended", lfg: null });
      // Seats cascaded: the raw table has none for the swept id (reach guard: the
      // young row's seat is still there).
      const reader = new Database(path, { readonly: true, strict: true });
      raw = reader;
      const count = (table: "lfg" | "lfg_seat", column: "id" | "lfg_id", id: string) =>
        (reader.query(`SELECT COUNT(*) AS n FROM ${table} WHERE ${column} = $id`).get({ id }) as { n: number }).n;
      expect([count("lfg", "id", old.id), count("lfg_seat", "lfg_id", old.id)]).toEqual([0, 0]);
      expect([count("lfg", "id", young.id), count("lfg_seat", "lfg_id", young.id)]).toEqual([1, 1]);
    } finally {
      raw?.close();
      // LfgStore exposes no close(); its connection is reached through the
      // private field (bracket access) so the test does not leak it.
      store?.["db"].close();
      rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe("room codes", () => {
  test("mintGameCode: 6 × [A-Z0-9], not a constant generator", () => {
    const codes = Array.from({ length: 1000 }, () => mintGameCode());
    for (const code of codes) expect(code).toMatch(CODE);
    for (let i = 0; i < 6; i++) {
      expect(new Set(codes.map((c) => c[i])).size).toBeGreaterThan(1);
    }
  });

  test("bytes ≥ 252 are rejected, so every symbol comes from exactly 7 byte values", () => {
    const counts = new Map<string, number>();
    for (let byte = 0; byte < 256; byte++) {
      // First fill: six copies of `byte`; second fill: 0..5 ("ABCDEF").
      const fills = [new Array(6).fill(byte), [0, 1, 2, 3, 4, 5]];
      const code = mintGameCode((bytes) => bytes.set(fills.shift()!));
      if (byte >= 252) {
        expect({ byte, code }).toEqual({ byte, code: "ABCDEF" });
      } else {
        expect(code).toBe(code[0].repeat(6));
        counts.set(code[0], (counts.get(code[0]) ?? 0) + 1);
      }
    }
    expect(counts.size).toBe(36);
    expect(new Set(counts.values())).toEqual(new Set([7]));
  });
});

describe("atomicity", () => {
  test("a failed ready transition rolls the whole click back", () => {
    const failing = new LfgStore(":memory:", () => {
      throw new Error("mint failed");
    });
    const lfg = created(failing, { seats: 2 });
    expect(() => failing.join(lfg.id, GUILD, "b", T0 + 1)).toThrow("mint failed");
    // The joiner has no seat and the post is still open.
    expect(failing.leave(lfg.id, GUILD, "b", T0 + 2)).toMatchObject({ kind: "refused", reason: "not_seated" });
    expect(failing.linkFor(lfg.id, GUILD, "creator", T0 + 2)).toMatchObject({
      kind: "refused",
      reason: "not_ready",
      lfg: { state: "open", seated: ["creator"] },
    });

    // Positive guard: the real minter seats the same joiner and reaches ready.
    const real = new LfgStore(":memory:");
    const ok = created(real, { seats: 2 });
    expect(lfgOf(real.join(ok.id, GUILD, "b", T0 + 1))).toMatchObject({ state: "ready", seated: ["creator", "b"] });
  });
});

test("a server-mode LFG round-trips its server", () => {
  const store = new LfgStore(":memory:");
  const server = { url: "wss://phase-0.example/ws", name: "Phase 0" };
  const lfg = created(store, { mode: "server", server, format: format("CommanderDraft"), seats: 8 });
  expect(lfg).toMatchObject({ mode: "server", server, build: "release", seats: 8 });
});
