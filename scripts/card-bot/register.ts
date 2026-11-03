// One-time (re)registration of the /card and /lfg guild commands. Run after
// changing either command's shape: `bun scripts/card-bot/register.ts`.
//
// Guild-scoped → instant propagation on the single community server. The PUT
// replaces the whole guild command set, so both commands go in one call.

import { BUILDS, discord, LFG_DEFAULT_BUILD } from "./config";
import { OptionType, registerGuildCommands } from "./discord";
import { MAX_SEATS } from "./formats";
import { LFG_FORMAT_OPTION } from "./lfgInteractions";

const cardCommand = {
  name: "card",
  description: "Show how the phase.rs engine parses a card",
  options: [
    {
      type: OptionType.STRING,
      name: "name",
      description: "Card name",
      required: true,
      autocomplete: true,
    },
    {
      type: OptionType.STRING,
      name: "build",
      description: "Which build's parse data to read (default: preview)",
      required: false,
      choices: [
        { name: "preview", value: "preview" },
        { name: "release", value: "release" },
      ],
    },
  ],
};

// Discord requires required options before optional ones.
const lfgCommand = {
  name: "lfg",
  description: "Find players for a phase.rs multiplayer game",
  options: [
    LFG_FORMAT_OPTION,
    {
      type: OptionType.INTEGER,
      name: "seats",
      description: "Players including you (default: 4 for Commander, else format maximum)",
      required: false,
      min_value: 2,
      max_value: MAX_SEATS,
    },
    {
      type: OptionType.STRING,
      name: "description",
      description: "Game details, e.g. Commander bracket, power level, or deck preferences",
      required: false,
      max_length: 500,
    },
    {
      type: OptionType.STRING,
      name: "mode",
      description: "Who hosts (default: p2p; server when a server is picked)",
      required: false,
      choices: [
        { name: "Peer-to-peer (host's browser)", value: "p2p" },
        { name: "Dedicated server", value: "server" },
      ],
    },
    {
      type: OptionType.STRING,
      name: "build",
      description: `Which site everyone plays on (default: ${LFG_DEFAULT_BUILD})`,
      required: false,
      choices: BUILDS.map((b) => ({ name: b, value: b })),
    },
    {
      type: OptionType.STRING,
      name: "server",
      description: "Dedicated server (implies mode: server)",
      required: false,
      autocomplete: true,
    },
  ],
};

await registerGuildCommands(discord.appId(), discord.guildId(), discord.token(), [
  cardCommand,
  lfgCommand,
]);
console.log("Registered /card and /lfg guild commands.");
