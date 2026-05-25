// One-time (re)registration of the /card guild command. Run after changing the
// command shape: `bun scripts/card-bot/register.ts`.
//
// Guild-scoped → instant propagation on the single community server.

import { discord } from "./config";
import { OptionType, registerGuildCommand } from "./discord";

const command = {
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

await registerGuildCommand(discord.appId(), discord.guildId(), discord.token(), command);
console.log("Registered /card guild command.");
