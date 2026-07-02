// Card-bot configuration. Dependency-free: reads everything from Bun.env,
// mirroring the rest of scripts/ (no package.json, no node_modules).

/**
 * Which R2-published build to read parse data from.
 * - `release`  → bucket root          (https://<r2>/coverage-data.json)
 * - `preview`  → the preview/staging site's data (https://<r2>/staging/coverage-data.json)
 *
 * These map to the same paths the deployed frontends read (release.yml → root,
 * deploy.yml staging → /staging). The Scryfall image is build-independent.
 */
export type Build = "release" | "preview";

export const BUILDS: readonly Build[] = ["release", "preview"] as const;

export function isBuild(value: string): value is Build {
  return (BUILDS as readonly string[]).includes(value);
}

/** Public R2 base, overridable for self-host / testing. No trailing slash. */
const R2_BASE = (
  Bun.env.CARD_BOT_R2_BASE ?? "https://data.phase-rs.dev"
).replace(/\/+$/, "");

/** Per-build path prefix under the R2 base. */
const BUILD_PREFIX: Record<Build, string> = {
  release: "",
  preview: "/staging",
};

/** URL of the coverage export (carries per-card `parse_details`) for a build. */
export function coverageUrl(build: Build): string {
  return `${R2_BASE}${BUILD_PREFIX[build]}/coverage-data.json`;
}

/** URL of the tiny build-provenance manifest (commit, mtgjson date) for a build. */
export function metaUrl(build: Build): string {
  return `${R2_BASE}${BUILD_PREFIX[build]}/card-data-meta.json`;
}

/**
 * URL of the Scryfall image/metadata export — the same file the app reads
 * (services/scryfall.ts). Build-independent (it's Scryfall bulk data, not engine
 * output), so we just read the freshest copy from the default build's path.
 */
export function scryfallDataUrl(): string {
  return `${R2_BASE}${BUILD_PREFIX[DEFAULT_BUILD]}/scryfall-data.json`;
}

/** Default build when the user omits the option — preview tracks latest main. */
export const DEFAULT_BUILD: Build = "preview";

function required(name: string): string {
  const value = Bun.env[name];
  if (!value) throw new Error(`Missing required env var ${name}`);
  return value;
}

// Public identifiers for the dedicated card-bot Discord app. Both are non-secret
// (the public key exists to be shared for signature verification; the app id
// appears in every interaction), so they're baked in as overridable defaults —
// only the bot token and guild id need to be supplied per host.
const DEFAULT_APP_ID = "1508547877331271892";
const DEFAULT_PUBLIC_KEY =
  "7cde1d5e7a717be0d222f93526e40aeb17165e02521a2471e5b6794e2e0f328e";
// The phase.rs community server. Non-secret (a guild id is visible to members);
// baked in so registration needs no config beyond the bot token.
const DEFAULT_GUILD_ID = "1485498006781427802";

/** Discord application credentials (the dedicated card-bot app, not the bug bot). */
export const discord = {
  /** Bot token (secret) — only needed to register slash commands (register.ts). */
  token: () => required("CARD_BOT_TOKEN"),
  /** Ed25519 public key — verifies inbound interaction signatures. */
  publicKey: () => Bun.env.CARD_BOT_PUBLIC_KEY || DEFAULT_PUBLIC_KEY,
  /** Application (client) id. */
  appId: () => Bun.env.CARD_BOT_APP_ID || DEFAULT_APP_ID,
  /** Guild to register the command in (instant propagation, single-server bot). */
  guildId: () => Bun.env.CARD_BOT_GUILD_ID || DEFAULT_GUILD_ID,
};

/** HTTP port the interactions server listens on (behind nginx on 127.0.0.1). */
export const PORT = Number(Bun.env.CARD_BOT_PORT ?? 9375);

/** Identifies the bot to Scryfall per their API etiquette. */
export const SCRYFALL_USER_AGENT =
  Bun.env.CARD_BOT_USER_AGENT ?? "phase-rs-card-bot/1.0 (+https://phase-rs.dev)";
