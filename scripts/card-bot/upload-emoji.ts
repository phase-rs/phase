// Bulk-creates application-owned emoji for MTG mana symbols, then writes the
// symbol → emoji-markup map the renderer uses. Run once after rasterizing PNGs
// (see the tmp/mana-emoji fetch step):
//   bun scripts/card-bot/upload-emoji.ts [dir=tmp/mana-emoji]
//
// Needs CARD_BOT_TOKEN (the bot token) — Bun auto-loads it from .env. Idempotent:
// re-running reuses emoji already created on the app.
//
// Application emoji (up to 2000/app) work in any server the bot is in and don't
// consume per-server emoji slots. Their IDs are stable, so the generated
// manaEmoji.json is committed + baked into the image — the runtime container
// stays secretless (it never needs the token to render pips).

import { discord } from "./config";

const API = "https://discord.com/api/v10";
const DIR = process.argv[2] ?? "tmp/mana-emoji";
const OUT_MAP = "scripts/card-bot/manaEmoji.json";
const APP_ID = discord.appId();
const TOKEN = discord.token();

interface AppEmoji {
  id: string;
  name: string;
}

async function api<T>(method: string, path: string, body?: unknown): Promise<T> {
  for (;;) {
    const res = await fetch(`${API}${path}`, {
      method,
      headers: {
        Authorization: `Bot ${TOKEN}`,
        ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
      },
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    if (res.status === 429) {
      const { retry_after } = (await res.json()) as { retry_after: number };
      await Bun.sleep(Math.ceil(retry_after * 1000) + 100);
      continue;
    }
    if (!res.ok) {
      throw new Error(`${method} ${path} → ${res.status}: ${await res.text()}`);
    }
    return (res.status === 204 ? null : await res.json()) as T;
  }
}

async function listExisting(): Promise<Map<string, string>> {
  const body = await api<{ items: AppEmoji[] }>("GET", `/applications/${APP_ID}/emojis`);
  const map = new Map<string, string>();
  for (const e of body.items ?? []) map.set(e.name, e.id);
  return map;
}

async function dataUri(path: string): Promise<string> {
  const bytes = new Uint8Array(await Bun.file(path).arrayBuffer());
  return `data:image/png;base64,${Buffer.from(bytes).toString("base64")}`;
}

const symbolMap = (await Bun.file(`${DIR}/_symbol-map.json`).json()) as Record<string, string>;
const existing = await listExisting();

const result: Record<string, string> = {};
let created = 0;
let reused = 0;

for (const [symbol, name] of Object.entries(symbolMap)) {
  let id = existing.get(name);
  if (id) {
    reused += 1;
  } else {
    const png = `${DIR}/${name}.png`;
    if (!(await Bun.file(png).exists())) {
      console.warn(`missing PNG for ${symbol} (${name}) — skipping`);
      continue;
    }
    const emoji = await api<AppEmoji>("POST", `/applications/${APP_ID}/emojis`, {
      name,
      image: await dataUri(png),
    });
    id = emoji.id;
    created += 1;
    await Bun.sleep(300); // gentle with the create rate limit
  }
  result[symbol] = `<:${name}:${id}>`;
}

await Bun.write(OUT_MAP, `${JSON.stringify(result, null, 2)}\n`);
console.log(`Application emoji: ${created} created, ${reused} reused.`);
console.log(`Wrote ${Object.keys(result).length} mappings → ${OUT_MAP}`);
