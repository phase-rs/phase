// One-time, idempotent setup: `bun scripts/card-bot/create-lfg-roles.ts`.
// Creates the opt-in Discord roles used by Carl-bot and /lfg.

import { discord } from "./config";
import { FORMATS } from "./formats";
import { lfgRoleName } from "./lfgRoles";

const url = `https://discord.com/api/v10/guilds/${discord.guildId()}/roles`;
const authorization = `Bot ${discord.token()}`;

async function request(method: "GET" | "POST", body?: object): Promise<unknown> {
  for (;;) {
    const response = await fetch(url, {
      method,
      headers: {
        Authorization: authorization,
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (response.status === 429) {
      const rateLimit = (await response.json()) as { retry_after: number };
      await Bun.sleep(Math.ceil(rateLimit.retry_after * 1000));
      continue;
    }
    if (!response.ok) throw new Error(`${method} guild roles → ${response.status}: ${await response.text()}`);
    return response.json();
  }
}

const existing = (await request("GET")) as { name: string }[];
const names = new Set(existing.map((role) => role.name));
for (const format of FORMATS) {
  const name = lfgRoleName(format);
  if (names.has(name)) {
    console.log(`exists: ${name}`);
    continue;
  }
  await request("POST", { name, permissions: "0", mentionable: true, hoist: false });
  names.add(name);
  console.log(`created: ${name}`);
}
