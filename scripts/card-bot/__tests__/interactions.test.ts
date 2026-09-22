import { afterEach, beforeEach, describe, expect, spyOn, test } from "bun:test";

import {
  ButtonStyle,
  type CommandInteraction,
  type ComponentInteraction,
  createFollowupMessage,
  editOriginalResponse,
  type InteractionOption,
  InteractionType,
  MessageFlags,
  OptionType,
  registerGuildCommands,
  ResponseType,
} from "../discord";
import { handleInteraction } from "../index";
import { type CreateResult, LfgStore } from "../lfg";
import { type LfgDeps, lfgAutocomplete, lfgCommand, lfgComponent } from "../lfgInteractions";
import { customId, type LfgAction } from "../lfgView";
import { DIRECTORY_VERSION, type DirectoryServer, type FetchFn, ServerCache } from "../servers";

const GUILD = "guild-1";
const CREATOR = "111";
const T0 = 1_000_000_000_000;

function row(overrides: Partial<DirectoryServer> = {}): DirectoryServer {
  return {
    url: "wss://a.example/ws",
    name: "Alpha",
    mode: "Full",
    protocol_version: 76,
    lobby_protocol_version: 10,
    current_players: 3,
    score: { value: 90 },
    ...overrides,
  };
}

/** A ServerCache whose every build reports this broker and these directory rows. */
async function cache(lobbyProtocol = 10, rows: DirectoryServer[] = [row()]): Promise<ServerCache> {
  const stub: FetchFn = async (url) =>
    url.endsWith("/health")
      ? Response.json({ mode: "LobbyOnly", protocol_version: 76, lobby_protocol_version: lobbyProtocol })
      : Response.json({ directory_version: DIRECTORY_VERSION, servers: rows });
  const servers = new ServerCache(stub);
  await servers.refresh();
  return servers;
}

type Recorded = { appId: string; token: string; body: unknown };

function deps(servers: ServerCache): LfgDeps & { pings: Recorded[] } {
  const pings: Recorded[] = [];
  let clock = T0;
  return {
    store: new LfgStore(":memory:"),
    servers,
    now: () => (clock += 1000),
    followup: async (appId, token, body) => void pings.push({ appId, token, body }),
    pings,
  };
}

const member = (userId: string) => ({ user: { id: userId, username: `u${userId}` } });

function command(options: InteractionOption[], userId = CREATOR): CommandInteraction {
  return {
    type: InteractionType.APPLICATION_COMMAND,
    application_id: "app",
    token: "tok",
    guild_id: GUILD,
    member: member(userId),
    data: { name: "lfg", options },
  };
}

const opt = (name: string, value: string | number, focused?: boolean): InteractionOption => ({
  name,
  type: typeof value === "number" ? OptionType.INTEGER : OptionType.STRING,
  value,
  ...(focused ? { focused } : {}),
});

function component(action: LfgAction, id: string, userId: string): ComponentInteraction {
  return {
    type: InteractionType.MESSAGE_COMPONENT,
    application_id: "app",
    token: `tok-${userId}`,
    guild_id: GUILD,
    member: member(userId),
    data: { custom_id: customId(action, id), component_type: 2 },
  };
}

const click = (d: LfgDeps, action: LfgAction, id: string, userId: string) =>
  lfgComponent(component(action, id, userId), { action, id }, d);

interface Body {
  type: number;
  data: {
    flags?: number;
    content?: string;
    embeds?: { description?: string }[];
    components?: { components: { label: string; custom_id?: string }[] }[];
    choices?: { name: string; value: string }[];
  };
}
const body = async (res: Response) => (await res.json()) as Body;
const buttonLabels = (b: Body) => b.data.components?.flatMap((r) => r.components.map((c) => c.label)) ?? [];

/** Every `store.create` result, so a test can tell whether a row was written. */
function spyCreates(d: LfgDeps) {
  const spy = spyOn(d.store, "create");
  return () => spy.mock.results.map((r) => r.value as CreateResult).filter((r) => r.kind === "created");
}

/** Replaces the global `fetch` (restore with `.mockRestore()`). The stub keeps
 *  the real `preconnect`, so it has `fetch`'s full type. */
function stubGlobalFetch(impl: (input: Parameters<typeof fetch>[0], init?: RequestInit) => Promise<Response>) {
  return spyOn(globalThis, "fetch").mockImplementation(
    Object.assign(impl, { preconnect: globalThis.fetch.preconnect }),
  );
}

let quiet: ReturnType<typeof spyOn>[] = [];
beforeEach(() => {
  quiet = [
    spyOn(console, "log").mockImplementation(() => {}),
    spyOn(console, "error").mockImplementation(() => {}),
  ];
});
afterEach(() => {
  for (const s of quiet) s.mockRestore();
});

describe("/lfg command (T-cmd)", () => {
  test("success is a public post with Join / Leave / Start and creates exactly one LFG", async () => {
    const d = deps(await cache());
    const created = spyCreates(d);
    const res = await body(lfgCommand(command([opt("format", "Commander")]), d));
    expect(res.type).toBe(ResponseType.CHANNEL_MESSAGE_WITH_SOURCE);
    expect(res.data.flags).toBeUndefined();
    expect(buttonLabels(res)).toEqual(["Join", "Leave", "Start"]);
    expect(created()).toHaveLength(1);
    // Default seats: min(Commander max 6, P2P cap 6); default build release.
    expect(created()[0]).toMatchObject({ kind: "created", lfg: { seats: 6, mode: "p2p", build: "release", server: null } });
  });

  // [name, cache, options, the refusal's own text]
  const refusals: [string, () => Promise<ServerCache>, InteractionOption[], string][] = [
    [
      "cold cache",
      async () => new ServerCache(async () => new Response(null, { status: 500 })),
      [opt("format", "Commander")],
      "Checking lobby status",
    ],
    ["broker below lobby protocol 10", () => cache(9), [opt("format", "Commander")], "doesn't support Discord games yet"],
    [
      // A lobby-10 server on a pre-10 build: the build's site would ignore the links.
      "server mode on a broker below lobby protocol 10, with an eligible lobby-10 server",
      () => cache(9, [row()]),
      [opt("format", "Commander"), opt("mode", "server")],
      "doesn't support Discord games yet",
    ],
    [
      "no eligible server",
      () => cache(10, [row({ mode: "LobbyOnly" })]),
      [opt("format", "Commander"), opt("mode", "server")],
      "No dedicated server",
    ],
    [
      "a server URL that is in the directory but not eligible",
      () => cache(10, [row(), row({ url: "wss://old.example/ws", lobby_protocol_version: 9 })]),
      [opt("format", "Commander"), opt("server", "wss://old.example/ws")],
      "isn't available for Discord games",
    ],
    [
      "a server URL not in the directory",
      () => cache(),
      [opt("format", "Commander"), opt("server", "wss://evil.example/ws")],
      "isn't available for Discord games",
    ],
    ["seats above the P2P cap", () => cache(), [opt("format", "CommanderDraft"), opt("seats", 7)], "take 3–6 players"],
    ["seats below the format minimum", () => cache(), [opt("format", "TwoHeadedGiant"), opt("seats", 3)], "take 4 players"],
    [
      "explicit mode:p2p with a server",
      () => cache(),
      [opt("format", "Commander"), opt("mode", "p2p"), opt("server", row().url)],
      "only applies to dedicated-server games",
    ],
  ];
  for (const [name, servers, options, reason] of refusals) {
    test(`refusal is ephemeral and creates nothing: ${name}`, async () => {
      const d = deps(await servers());
      const created = spyCreates(d);
      const res = await body(lfgCommand(command(options), d));
      expect(res.type).toBe(ResponseType.CHANNEL_MESSAGE_WITH_SOURCE);
      expect(res.data.flags).toBe(MessageFlags.EPHEMERAL);
      expect(res.data.content).toContain(reason);
      expect(res.data.embeds).toBeUndefined();
      expect(created()).toHaveLength(0);
    });
  }

  test("a second open LFG by the same creator is refused ephemerally (has_open)", async () => {
    const d = deps(await cache());
    const created = spyCreates(d);
    expect((await body(lfgCommand(command([opt("format", "Commander")]), d))).data.flags).toBeUndefined();
    const second = await body(lfgCommand(command([opt("format", "Modern")]), d));
    expect(second.data.flags).toBe(MessageFlags.EPHEMERAL);
    expect(second.data.content).toContain("already have an open LFG");
    expect(created()).toHaveLength(1);
  });

  test("a `server` option without `mode` infers a dedicated-server game at that server", async () => {
    const pick = row({ url: "wss://b.example/ws", name: "Bravo", score: { value: 10 } });
    const d = deps(await cache(10, [row(), pick]));
    const created = spyCreates(d);
    const res = await body(lfgCommand(command([opt("format", "Commander"), opt("server", pick.url)]), d));
    expect(res.data.flags).toBeUndefined();
    expect(created()[0]).toMatchObject({ lfg: { mode: "server", server: { url: pick.url, name: "Bravo" }, seats: 6 } });
  });

  test("mode:server without `server` picks the top-scored eligible row", async () => {
    // Listed score-50 first with the alphabetically earlier URL, so neither
    // "first listed" nor "first by URL" nor "last" picks the score-80 row.
    const low = row({ url: "wss://a50.example/ws", name: "Low", score: { value: 50 } });
    const high = row({ url: "wss://b80.example/ws", name: "High", score: { value: 80 } });
    const d = deps(await cache(10, [low, high]));
    const created = spyCreates(d);
    const res = await body(lfgCommand(command([opt("format", "Commander"), opt("mode", "server")]), d));
    expect(res.data.flags).toBeUndefined();
    expect(created()[0]).toMatchObject({ lfg: { mode: "server", server: { url: high.url } } });
  });
});

describe("LFG buttons (T-comp)", () => {
  async function posted(d: LfgDeps, options: InteractionOption[]): Promise<string> {
    const res = await body(lfgCommand(command(options), d));
    const joinId = res.data.components?.[0].components[0].custom_id;
    if (joinId === undefined) throw new Error(`/lfg refused: ${res.data.content}`);
    return joinId.split(":")[2];
  }

  test("a Join that fills updates the post to ready and pings the seated users once", async () => {
    const d = deps(await cache());
    const id = await posted(d, [opt("format", "Commander"), opt("seats", 3)]);

    const partial = await body(click(d, "join", id, "222"));
    expect(partial.type).toBe(ResponseType.UPDATE_MESSAGE);
    expect(buttonLabels(partial)).toEqual(["Join", "Leave", "Start"]);
    expect(d.pings).toHaveLength(0);

    const full = await body(click(d, "join", id, "333"));
    expect(full.type).toBe(ResponseType.UPDATE_MESSAGE);
    expect(buttonLabels(full)).toEqual(["Get my link"]);
    expect(d.pings).toHaveLength(1);
    const ping = d.pings[0];
    expect(ping).toMatchObject({ appId: "app", token: "tok-333" });
    const mentions = (ping.body as { allowed_mentions: Record<string, unknown> }).allowed_mentions;
    expect(mentions).toEqual({ users: [CREATOR, "222", "333"] });
    expect("parse" in mentions).toBe(false);
  });

  test("the creator's Start on a partly filled LFG makes it ready and pings once", async () => {
    const d = deps(await cache());
    const id = await posted(d, [opt("format", "Commander"), opt("seats", 4)]);
    click(d, "join", id, "222");
    expect(d.pings).toHaveLength(0);

    const started = await body(click(d, "start", id, CREATOR));
    expect(started.type).toBe(ResponseType.UPDATE_MESSAGE);
    expect(buttonLabels(started)).toEqual(["Get my link"]);
    expect(d.pings).toHaveLength(1);
    const mentions = (d.pings[0].body as { allowed_mentions: Record<string, unknown> }).allowed_mentions;
    expect(mentions).toEqual({ users: [CREATOR, "222"] });
  });

  test("a refused click (non-creator Start) is ephemeral and pings nobody", async () => {
    const d = deps(await cache());
    const id = await posted(d, [opt("format", "Commander")]);
    click(d, "join", id, "222");
    const refused = await body(click(d, "start", id, "222"));
    expect(refused).toMatchObject({ type: ResponseType.CHANNEL_MESSAGE_WITH_SOURCE, data: { flags: MessageFlags.EPHEMERAL } });
    expect(d.pings).toHaveLength(0);
  });

  test("Get my link is an ephemeral link-button reply: host link for the creator, guest link otherwise", async () => {
    const d = deps(await cache());
    const id = await posted(d, [opt("format", "Standard")]);
    click(d, "join", id, "222");
    const replies = [await click(d, "link", id, CREATOR).json(), await click(d, "link", id, "222").json()] as {
      type: number;
      data: { flags: number; components: { components: { style: number; url: string }[] }[] };
    }[];
    for (const reply of replies) {
      expect(reply.type).toBe(ResponseType.CHANNEL_MESSAGE_WITH_SOURCE);
      expect(reply.data.flags).toBe(MessageFlags.EPHEMERAL);
      expect(reply.data.components[0].components[0].style).toBe(ButtonStyle.LINK);
    }
    expect(new URL(replies[0].data.components[0].components[0].url).searchParams.has("code")).toBe(true);
    expect(new URL(replies[1].data.components[0].components[0].url).searchParams.has("join")).toBe(true);
  });

  test("a click on a vanished LFG closes the post", async () => {
    const d = deps(await cache());
    const res = await body(click(d, "join", "00000000-0000-0000-0000-000000000000", "222"));
    expect(res.type).toBe(ResponseType.UPDATE_MESSAGE);
    expect(res.data).toMatchObject({ content: "This LFG has ended.", components: [] });
  });
});

describe("/lfg server autocomplete", () => {
  test("offers eligible servers only, filtered by the typed text, value = row url", async () => {
    const d = deps(
      await cache(10, [
        row({ url: "wss://a.example/ws", name: "Alpha" }),
        row({ url: "wss://b.example/ws", name: "Bravo" }),
        row({ url: "wss://old.example/ws", name: "Also old", lobby_protocol_version: 9 }),
      ]),
    );
    const interaction = (typed: string) => ({
      ...command([opt("format", "Commander"), opt("server", typed, true)]),
      type: InteractionType.APPLICATION_COMMAND_AUTOCOMPLETE,
    });
    const all = await body(lfgAutocomplete(interaction(""), d));
    expect(all.type).toBe(ResponseType.APPLICATION_COMMAND_AUTOCOMPLETE_RESULT);
    expect(all.data.choices).toEqual([
      { name: "Alpha (3 online)", value: "wss://a.example/ws" },
      { name: "Bravo (3 online)", value: "wss://b.example/ws" },
    ]);
    expect((await body(lfgAutocomplete(interaction("BRA"), d))).data.choices?.map((c) => c.value)).toEqual([
      "wss://b.example/ws",
    ]);
  });

  test("a broker below lobby protocol 10 offers nothing, even with an eligible lobby-10 server", async () => {
    const d = deps(await cache(9, [row()]));
    const interaction = { ...command([opt("server", "", true)]), type: InteractionType.APPLICATION_COMMAND_AUTOCOMPLETE };
    expect((await body(lfgAutocomplete(interaction, d))).data.choices).toEqual([]);
    // Reach guard: the same row is offered once the broker is at lobby 10.
    const ok = deps(await cache(10, [row()]));
    expect((await body(lfgAutocomplete(interaction, ok))).data.choices?.map((c) => c.value)).toEqual([row().url]);
  });

  test("a cold cache offers nothing", async () => {
    const d = deps(new ServerCache(async () => new Response(null, { status: 500 })));
    const res = await body(
      lfgAutocomplete(
        { ...command([opt("server", "", true)]), type: InteractionType.APPLICATION_COMMAND_AUTOCOMPLETE },
        d,
      ),
    );
    expect(res.data.choices).toEqual([]);
  });
});

// Signed end-to-end requests through handleInteraction.
const keys = (await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"])) as CryptoKeyPair;
const toHex = (bytes: ArrayBuffer) => Buffer.from(bytes).toString("hex");
const PUBLIC_KEY = toHex(await crypto.subtle.exportKey("raw", keys.publicKey));

async function signed(payload: unknown, tamper = false): Promise<Request> {
  const raw = JSON.stringify(payload);
  const timestamp = "1700000000";
  const signature = new Uint8Array(
    await crypto.subtle.sign({ name: "Ed25519" }, keys.privateKey, new TextEncoder().encode(timestamp + raw)),
  );
  if (tamper) signature[0] ^= 0xff;
  return new Request("http://bot/", {
    method: "POST",
    headers: { "X-Signature-Ed25519": toHex(signature.buffer), "X-Signature-Timestamp": timestamp },
    body: raw,
  });
}

describe("handleInteraction", () => {
  // The /card paths fetch R2 / Scryfall; no test may reach the network.
  let network: ReturnType<typeof spyOn>;
  beforeEach(() => {
    network = stubGlobalFetch(async () => {
      throw new Error("network access in a test");
    });
  });
  afterEach(() => network.mockRestore());

  test("routes /lfg commands and /lfg autocomplete by name (T-route)", async () => {
    const d = deps(await cache());
    const ctx = { publicKey: PUBLIC_KEY, lfg: d };

    const auto = await body(
      await handleInteraction(
        await signed({ ...command([opt("server", "", true)]), type: InteractionType.APPLICATION_COMMAND_AUTOCOMPLETE }),
        ctx,
      ),
    );
    expect(auto.type).toBe(ResponseType.APPLICATION_COMMAND_AUTOCOMPLETE_RESULT);
    expect(auto.data.choices?.map((c) => c.value)).toEqual([row().url]);

    const post = await body(await handleInteraction(await signed(command([opt("format", "Commander")])), ctx));
    expect(post.type).toBe(ResponseType.CHANNEL_MESSAGE_WITH_SOURCE);
    expect(buttonLabels(post)).toEqual(["Join", "Leave", "Start"]);
    expect(network).not.toHaveBeenCalled();
  });

  test("routes buttons to the LFG handlers; an unknown custom_id is a 400 (T-comp)", async () => {
    const d = deps(await cache());
    const ctx = { publicKey: PUBLIC_KEY, lfg: d };
    const post = await body(await handleInteraction(await signed(command([opt("format", "Commander")])), ctx));
    const id = post.data.components![0].components[0].custom_id!.split(":")[2];

    const joined = await handleInteraction(await signed(component("join", id, "222")), ctx);
    expect((await body(joined)).type).toBe(ResponseType.UPDATE_MESSAGE);

    const unknown = await handleInteraction(
      await signed({ ...component("join", id, "222"), data: { custom_id: "card:join:x", component_type: 2 } }),
      ctx,
    );
    expect(unknown.status).toBe(400);
  });

  test("the signature gate covers buttons: a tampered click is 401 and changes nothing (T-sig)", async () => {
    const d = deps(await cache());
    const ctx = { publicKey: PUBLIC_KEY, lfg: d };
    const post = await body(await handleInteraction(await signed(command([opt("format", "Commander")])), ctx));
    const id = post.data.components![0].components[0].custom_id!.split(":")[2];

    const tampered = await handleInteraction(await signed(component("join", id, "222"), true), ctx);
    expect(tampered.status).toBe(401);
    // Unchanged: 222 is still not seated, so a direct leave is refused.
    expect(d.store.leave(id, GUILD, "222", T0 + 10_000)).toMatchObject({ kind: "refused", reason: "not_seated" });

    const good = await handleInteraction(await signed(component("join", id, "333")), ctx);
    expect(good.status).toBe(200);
    // Positive guard: the signed click did seat 333.
    expect(d.store.join(id, GUILD, "333", T0 + 11_000)).toMatchObject({ kind: "refused", reason: "already_seated" });

    const unsigned = await handleInteraction(
      new Request("http://bot/", { method: "POST", body: JSON.stringify(component("join", id, "444")) }),
      ctx,
    );
    expect(unsigned.status).toBe(401);
    expect(d.store.leave(id, GUILD, "444", T0 + 12_000)).toMatchObject({ kind: "refused", reason: "not_seated" });
  });

  test("a signed PING is answered with PONG", async () => {
    const d = deps(await cache());
    const res = await handleInteraction(
      await signed({ type: InteractionType.PING, application_id: "app", token: "tok" }),
      { publicKey: PUBLIC_KEY, lfg: d },
    );
    expect(await res.json()).toEqual({ type: ResponseType.PONG });
  });
});

describe("Discord REST helpers", () => {
  let events: string[];
  let sleep: ReturnType<typeof spyOn>;
  beforeEach(() => {
    events = [];
    sleep = spyOn(Bun, "sleep").mockImplementation(async (ms) => void events.push(`sleep ${ms}`));
  });
  afterEach(() => sleep.mockRestore());

  function stubFetch(statuses: number[]) {
    const calls: { url: string; init: RequestInit }[] = [];
    const spy = stubGlobalFetch(async (input, init) => {
      calls.push({ url: String(input), init: init ?? {} });
      events.push("fetch");
      return new Response("{}", { status: statuses.shift() ?? 500 });
    });
    return { calls, restore: () => spy.mockRestore() };
  }

  test("createFollowupMessage waits, then retries a 404 until the message posts", async () => {
    const f = stubFetch([404, 404, 200]);
    try {
      await createFollowupMessage("app", "tok", { content: "hi" });
      expect(f.calls).toHaveLength(3);
      expect(f.calls[0].url).toBe("https://discord.com/api/v10/webhooks/app/tok");
      expect(f.calls[0].init.method).toBe("POST");
      expect(JSON.parse(String(f.calls[0].init.body))).toEqual({ content: "hi" });
      // The first attempt is delayed; each 404 waits before the retry.
      expect(events).toEqual(["sleep 1000", "fetch", "sleep 1000", "fetch", "sleep 1000", "fetch"]);
    } finally {
      f.restore();
    }
  });

  test("createFollowupMessage gives up after 3 attempts", async () => {
    const f = stubFetch([404, 404, 404]);
    try {
      await expect(createFollowupMessage("app", "tok", {})).rejects.toThrow("exhausted retries");
      expect(f.calls).toHaveLength(3);
    } finally {
      f.restore();
    }
  });

  test("editOriginalResponse does not retry a 404", async () => {
    const f = stubFetch([404, 200]);
    try {
      await expect(editOriginalResponse("app", "tok", {})).rejects.toThrow("404");
      expect(f.calls).toHaveLength(1);
      expect(f.calls[0].init.method).toBe("PATCH");
    } finally {
      f.restore();
    }
  });

  test("registerGuildCommands sends every command in one PUT", async () => {
    const f = stubFetch([200]);
    try {
      await registerGuildCommands("app", "guild", "token", [{ name: "card" }, { name: "lfg" }]);
      expect(f.calls).toHaveLength(1);
      expect(f.calls[0].url).toBe("https://discord.com/api/v10/applications/app/guilds/guild/commands");
      expect(f.calls[0].init.method).toBe("PUT");
      expect(JSON.parse(String(f.calls[0].init.body))).toEqual([{ name: "card" }, { name: "lfg" }]);
    } finally {
      f.restore();
    }
  });
});
