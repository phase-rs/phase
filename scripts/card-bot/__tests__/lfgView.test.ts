import { describe, expect, test } from "bun:test";

import { ButtonStyle, ComponentType, MessageFlags } from "../discord";
import { findFormat, FORMATS } from "../formats";
import type { Lfg } from "../lfg";
import {
  customId,
  guestLink,
  hostLink,
  type LfgAction,
  linkButtons,
  linkReply,
  parseCustomId,
  readyPing,
  refusalText,
  renderEnded,
  renderLfg,
  roomName,
} from "../lfgView";

const ID = "0b5c1f7e-4c1a-4d7e-9a53-2f1d3c4b5a69";
const CODE = "AB12CD";

function lfg(overrides: Partial<Lfg> = {}): Lfg {
  return {
    id: ID,
    guildId: "guild-1",
    creatorId: "111",
    format: findFormat("Commander")!,
    seats: 4,
    mode: "p2p",
    build: "release",
    server: null,
    state: "ready",
    code: CODE,
    touchedMs: 0,
    seated: ["111", "222"],
    ...overrides,
  };
}

const SERVER = { url: "wss://phase-0.example.com/ws", name: "Phase 0" };
const serverLfg = (overrides: Partial<Lfg> = {}) =>
  lfg({ mode: "server", server: SERVER, ...overrides });

describe("host link (Phase B grammar)", () => {
  test("P2P: exactly code, format, players, room — in order, no view; players = seated count", () => {
    const url = new URL(hostLink(lfg()));
    expect(url.origin + url.pathname).toBe("https://phase-rs.dev/multiplayer");
    expect([...url.searchParams.keys()]).toEqual(["code", "format", "players", "room"]);
    expect(url.searchParams.has("view")).toBe(false);
    expect(Object.fromEntries(url.searchParams)).toEqual({
      code: CODE,
      format: "Commander",
      players: "2", // two seated of four seats
      room: "Discord Commander",
    });
  });

  test("server mode appends server = the row url; preview uses the preview site", () => {
    const url = new URL(hostLink(serverLfg({ build: "preview" })));
    expect(url.origin).toBe("https://preview.phase-rs.dev");
    expect([...url.searchParams.keys()]).toEqual(["code", "format", "players", "room", "server"]);
    expect(url.searchParams.get("server")).toBe(SERVER.url);
  });

  test("every format's room round-trips", () => {
    for (const format of FORMATS) {
      const url = new URL(hostLink(lfg({ format })));
      expect(url.searchParams.get("room")).toBe(roomName(format));
      expect(url.searchParams.get("format")).toBe(format.format);
    }
  });

  test("fits Discord's 512-char button url with a 100-char server URL and the longest label", () => {
    const longest = [...FORMATS].sort((a, b) => b.label.length - a.label.length)[0];
    const server = { url: `wss://${"s".repeat(100 - "wss://.example/ws".length)}.example/ws`, name: "x" };
    expect(server.url).toHaveLength(100);
    const link = hostLink(serverLfg({ server, format: longest, seated: ["1", "2", "3", "4"] }));
    expect(link.length).toBeLessThanOrEqual(512);
    expect(guestLink(serverLfg({ server })).length).toBeLessThanOrEqual(512);
  });
});

describe("guest link", () => {
  const join = (l: Lfg) => new URL(guestLink(l)).searchParams.get("join");

  test("P2P joins at the build's official lobby; server mode at the row url", () => {
    expect(join(lfg())).toBe(`${CODE}@wss://lobby.phase-rs.dev/ws`);
    expect(join(lfg({ build: "preview" }))).toBe(`${CODE}@wss://lobby-preview.phase-rs.dev/ws`);
    expect(join(serverLfg())).toBe(`${CODE}@${SERVER.url}`);
    expect(join(serverLfg({ build: "preview" }))).toBe(`${CODE}@${SERVER.url}`);
    expect([...new URL(guestLink(lfg())).searchParams.keys()]).toEqual(["join"]);
  });

  test("the raw query percent-encodes @ and still decodes", () => {
    const raw = new URL(guestLink(lfg())).search;
    expect(raw).toContain(`join=${CODE}%40`);
    expect(raw).not.toContain("@");
    expect(join(lfg())?.split("@")).toEqual([CODE, "wss://lobby.phase-rs.dev/ws"]);
  });
});

describe("Get-my-link reply", () => {
  test("host gets one LINK button to the host link; guest to the guest link", () => {
    const l = lfg();
    const host = linkReply(l, "111");
    expect(host.data.flags).toBe(64);
    expect(host.data.flags).toBe(MessageFlags.EPHEMERAL);
    expect(host.data.allowed_mentions).toEqual({ parse: [] });
    expect(host.data.components).toHaveLength(1);
    const [row] = host.data.components;
    expect(row.type).toBe(ComponentType.ACTION_ROW);
    expect(row.components).toEqual([
      { type: ComponentType.BUTTON, style: ButtonStyle.LINK, label: "Open as host", url: hostLink(l) },
    ]);
    expect(row.components[0].style).toBe(5);
    expect("custom_id" in row.components[0]).toBe(false);

    const guest = linkReply(l, "222").data.components[0].components;
    expect(guest).toEqual([
      { type: ComponentType.BUTTON, style: ButtonStyle.LINK, label: "Join game", url: guestLink(l) },
    ]);
  });

  test("the row is the linkButtons list (later phases append to it); labels ≤ 80", () => {
    const l = lfg();
    for (const user of l.seated) {
      expect(linkReply(l, user).data.components[0].components).toEqual(linkButtons(l, user));
      for (const button of linkButtons(l, user)) expect(button.label.length).toBeLessThanOrEqual(80);
    }
  });
});

describe("custom_id", () => {
  test("round-trips every action within 100 chars", () => {
    for (const action of ["join", "leave", "start", "link"] as LfgAction[]) {
      const id = customId(action, ID);
      expect(id.length).toBeLessThanOrEqual(100);
      expect(parseCustomId(id)).toEqual({ action, id: ID });
    }
  });

  test("rejects malformed ids", () => {
    for (const bad of ["lfg:join", "lfg:bogus:x", "card:join:x", "lfg:join:", "lfg:join:x:y", ""]) {
      expect({ bad, parsed: parseCustomId(bad) }).toEqual({ bad, parsed: null });
    }
  });
});

describe("public post", () => {
  test("a ready post never contains the code, lists every seat, and has no fields", () => {
    const l = lfg();
    const post = renderLfg(l);
    // Positive guard: the same code IS in the ephemeral reply.
    expect(linkReply(l, "111").data.content).toContain(CODE);
    expect(JSON.stringify(post)).not.toContain(CODE);

    const [embed] = post.embeds;
    expect(embed.title).toBe("LFG · Commander");
    expect(embed.description).toContain("Players 2/4");
    for (const id of l.seated) expect(embed.description).toContain(`<@${id}>`);
    expect(embed.description).toContain("<@111> (host)");
    expect(embed.description).toContain("24 hours");
    expect(embed.footer).toBeUndefined();
    expect("fields" in embed).toBe(false);
    expect(post.allowed_mentions).toEqual({ parse: [] });
    expect(post.components).toEqual([
      {
        type: ComponentType.ACTION_ROW,
        components: [
          { type: ComponentType.BUTTON, style: ButtonStyle.PRIMARY, label: "Get my link", custom_id: customId("link", ID) },
        ],
      },
    ]);
  });

  test("open: Join / Leave / Start and the idle footer", () => {
    const post = renderLfg(lfg({ state: "open", code: null, seated: ["111"] }));
    expect(post.embeds[0].footer?.text).toBe("Expires after 30 min idle");
    expect(post.embeds[0].description).toContain("Players 1/4");
    expect(post.embeds[0].description).toContain("Peer-to-peer — hosted in <@111>'s browser");
    expect(post.components[0].components).toEqual([
      { type: ComponentType.BUTTON, style: ButtonStyle.PRIMARY, label: "Join", custom_id: customId("join", ID) },
      { type: ComponentType.BUTTON, style: ButtonStyle.SECONDARY, label: "Leave", custom_id: customId("leave", ID) },
      { type: ComponentType.BUTTON, style: ButtonStyle.SUCCESS, label: "Start", custom_id: customId("start", ID) },
    ]);
    expect(post.allowed_mentions).toEqual({ parse: [] });
  });

  test("server mode names the server; cancelled and expired have no components", () => {
    expect(renderLfg(serverLfg()).embeds[0].description).toContain("Dedicated server: Phase 0");
    const cancelled = renderLfg(lfg({ state: "cancelled", code: null }));
    expect(cancelled.components).toEqual([]);
    expect(cancelled.embeds[0].description).toContain("Cancelled by the host");
    const expired = renderLfg(lfg({ state: "expired", code: null }));
    expect(expired.components).toEqual([]);
    expect(expired.embeds[0].description).toContain("Expired");
    expect(renderEnded()).toEqual({ content: "This LFG has ended.", embeds: [], components: [] });
  });
});

describe("ready ping and refusals", () => {
  test("the ping mentions exactly the seated users, with no parse key", () => {
    const ping = readyPing(lfg());
    expect(ping.allowed_mentions).toEqual({ users: ["111", "222"] });
    expect(ping.content).toContain("<@111> <@222>");
  });

  test("too_few names the format's minimum", () => {
    expect(refusalText("too_few", findFormat("TwoHeadedGiant")!)).toBe(
      "Two-Headed Giant needs at least 4 players to start.",
    );
  });
});
