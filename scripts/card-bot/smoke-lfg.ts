// Local /lfg walk — no Discord credentials, no network.
//   bun scripts/card-bot/smoke-lfg.ts [p2p|server] [release|preview]
//
// Drives /lfg → Join until full → Get my link (host and a guest) through the real
// handlers with synthetic interactions, a stubbed lobby (a lobby-protocol-10
// broker plus one eligible dedicated server), and a recording follow-up, printing
// each response, the ready ping and both links.
//
// The LFG database is in-memory. To inspect a file instead, opt in explicitly
// with LFG_SMOKE_DB_PATH=<file> (never the bot's own CARD_BOT_DB_PATH).

import { BUILD_ENDPOINTS, isBuild, LFG_DEFAULT_BUILD, type Build } from "./config";
import {
  type CommandInteraction,
  type ComponentInteraction,
  InteractionType,
  OptionType,
} from "./discord";
import type { LfgMode } from "./formats";
import { LfgStore } from "./lfg";
import { type LfgDeps, lfgCommand, lfgComponent } from "./lfgInteractions";
import { customId, type LfgAction } from "./lfgView";
import { DIRECTORY_VERSION, type FetchFn, ServerCache } from "./servers";

const mode: LfgMode = process.argv[2] === "server" ? "server" : "p2p";
const buildArg = process.argv[3] ?? LFG_DEFAULT_BUILD;
const build: Build = isBuild(buildArg) ? buildArg : LFG_DEFAULT_BUILD;

const GUILD = "smoke-guild";
const USERS = ["100000000000000001", "100000000000000002", "100000000000000003"];

const stubFetch: FetchFn = async (url) => {
  if (url.endsWith("/health")) {
    return Response.json({ mode: "LobbyOnly", protocol_version: 76, lobby_protocol_version: 10 });
  }
  return Response.json({
    directory_version: DIRECTORY_VERSION,
    servers: [
      {
        url: `wss://smoke-${build}.example/ws`,
        name: `Smoke ${build} server`,
        mode: "Full",
        protocol_version: 76,
        lobby_protocol_version: 10,
        current_players: 0,
        score: { value: 90 },
      },
    ],
  });
};

const servers = new ServerCache(stubFetch);
await servers.refresh();

const pings: unknown[] = [];
let clock = Date.now();
const deps: LfgDeps = {
  store: new LfgStore(Bun.env.LFG_SMOKE_DB_PATH ?? ":memory:"),
  servers,
  now: () => (clock += 1000),
  followup: async (_appId, _token, body) => void pings.push(body),
  editOriginal: async () => {},
  threads: null,
};

const base = (userId: string) => ({
  application_id: "smoke-app",
  token: "smoke-token",
  guild_id: GUILD,
  member: { user: { id: userId, username: `user-${userId.slice(-1)}` } },
});

async function show(label: string, response: Response): Promise<unknown> {
  const body = await response.json();
  console.log(`\n== ${label} (HTTP ${response.status})\n${JSON.stringify(body, null, 2)}`);
  return body;
}

const command: CommandInteraction = {
  ...base(USERS[0]),
  type: InteractionType.APPLICATION_COMMAND,
  data: {
    name: "lfg",
    options: [
      { name: "format", type: OptionType.STRING, value: "Commander" },
      { name: "seats", type: OptionType.INTEGER, value: USERS.length },
      { name: "mode", type: OptionType.STRING, value: mode },
      { name: "build", type: OptionType.STRING, value: build },
    ],
  },
};
const post = (await show(`/lfg (${mode}, ${build})`, lfgCommand(command, deps))) as {
  data?: { components?: { components: { custom_id?: string }[] }[] };
};
const joinId = post.data?.components?.[0]?.components[0]?.custom_id;
if (joinId === undefined) throw new Error("the /lfg post has no Join button (refused?)");
const lfgId = joinId.split(":")[2];

function click(userId: string, action: LfgAction): Response {
  const interaction: ComponentInteraction = {
    ...base(userId),
    type: InteractionType.MESSAGE_COMPONENT,
    data: { custom_id: customId(action, lfgId), component_type: 2 },
  };
  return lfgComponent(interaction, { action, id: lfgId }, deps);
}

for (const user of USERS.slice(1)) await show(`Join by ${user}`, click(user, "join"));
console.log(`\n== ready ping\n${JSON.stringify(pings, null, 2)}`);

for (const [role, user] of [["host", USERS[0]], ["guest", USERS[1]]] as const) {
  const reply = (await show(`Get my link (${role})`, click(user, "link"))) as {
    data: { components: { components: { url: string }[] }[] };
  };
  console.log(`${role} link: ${reply.data.components[0].components[0].url}`);
}
console.log(`\nsite: ${BUILD_ENDPOINTS[build].site}`);
