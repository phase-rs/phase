// Minimal Discord interactions plumbing, dependency-free (matches scripts/).
// Ed25519 verification uses Bun's WebCrypto; REST uses fetch.

const API = "https://discord.com/api/v10";

/** Interaction request types (Discord `InteractionType`). */
export const InteractionType = {
  PING: 1,
  APPLICATION_COMMAND: 2,
  /** A button (or other message component) was pressed. */
  MESSAGE_COMPONENT: 3,
  APPLICATION_COMMAND_AUTOCOMPLETE: 4,
} as const;

/** Interaction response types (Discord `InteractionResponseType`). */
export const ResponseType = {
  PONG: 1,
  CHANNEL_MESSAGE_WITH_SOURCE: 4,
  DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE: 5,
  /** Edit the message a component is attached to (component interactions only). */
  UPDATE_MESSAGE: 7,
  APPLICATION_COMMAND_AUTOCOMPLETE_RESULT: 8,
} as const;

/** Slash-command option types we use. */
export const OptionType = {
  STRING: 3,
  INTEGER: 4,
} as const;

/** Message flags we set. EPHEMERAL messages are visible only to the invoker; a
 *  message's ephemeral state is fixed when it is first sent. */
export const MessageFlags = {
  EPHEMERAL: 1 << 6,
} as const;

/** Message component types (legacy action rows; no IS_COMPONENTS_V2 flag). */
export const ComponentType = {
  ACTION_ROW: 1,
  BUTTON: 2,
} as const;

/** Button styles. A LINK button carries a `url` and never a `custom_id`. */
export const ButtonStyle = {
  PRIMARY: 1,
  SECONDARY: 2,
  SUCCESS: 3,
  LINK: 5,
} as const;

export interface InteractionOption {
  name: string;
  type: number;
  value?: string | number | boolean;
  focused?: boolean;
}

export interface DiscordUser {
  id: string;
  username: string;
  global_name?: string | null;
}

interface InteractionBase {
  application_id: string;
  token: string;
  guild_id?: string;
  /** Sent when the interaction is invoked in a guild. */
  member?: { user: DiscordUser };
  /** Sent when the interaction is invoked in a DM. */
  user?: DiscordUser;
}

export interface CommandData {
  name: string;
  options?: InteractionOption[];
}

export interface ComponentData {
  custom_id: string;
  component_type: number;
}

/** An inbound interaction, discriminated on `type`. */
export type Interaction =
  | (InteractionBase & { type: typeof InteractionType.PING })
  | (InteractionBase & {
      type:
        | typeof InteractionType.APPLICATION_COMMAND
        | typeof InteractionType.APPLICATION_COMMAND_AUTOCOMPLETE;
      data: CommandData;
    })
  | (InteractionBase & { type: typeof InteractionType.MESSAGE_COMPONENT; data: ComponentData });

export type CommandInteraction = Extract<Interaction, { data: CommandData }>;
export type ComponentInteraction = Extract<Interaction, { data: ComponentData }>;

/** The invoking user's id: `member.user` in a guild, `user` in a DM. */
export function invokerId(interaction: Interaction): string | undefined {
  return interaction.member?.user.id ?? interaction.user?.id;
}

/** A JSON HTTP response (the interaction reply to Discord). */
export function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

/** A string option's value, if present. */
export function stringOption(
  options: readonly InteractionOption[] | undefined,
  name: string,
): string | undefined {
  const opt = options?.find((o) => o.name === name);
  return typeof opt?.value === "string" ? opt.value : undefined;
}

/** An integer option's value, if present. */
export function integerOption(
  options: readonly InteractionOption[] | undefined,
  name: string,
): number | undefined {
  const opt = options?.find((o) => o.name === name);
  return typeof opt?.value === "number" ? opt.value : undefined;
}

function hexToBytes(hex: string): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i++) {
    bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return bytes;
}

const keyCache = new Map<string, Promise<CryptoKey>>();
function importKey(publicKeyHex: string): Promise<CryptoKey> {
  let key = keyCache.get(publicKeyHex);
  if (!key) {
    key = crypto.subtle.importKey(
      "raw",
      hexToBytes(publicKeyHex),
      { name: "Ed25519" },
      false,
      ["verify"],
    );
    keyCache.set(publicKeyHex, key);
  }
  return key;
}

/**
 * Verifies a Discord interaction request signature over `timestamp + rawBody`.
 * Returns false on any malformed input — never throws.
 */
export async function verifyRequest(
  publicKeyHex: string,
  signatureHex: string | null,
  timestamp: string | null,
  rawBody: string,
): Promise<boolean> {
  if (!signatureHex || !timestamp) return false;
  try {
    const key = await importKey(publicKeyHex);
    const message = new TextEncoder().encode(timestamp + rawBody);
    return await crypto.subtle.verify(
      { name: "Ed25519" },
      key,
      hexToBytes(signatureHex),
      message,
    );
  } catch {
    return false;
  }
}

/**
 * Bulk-overwrites the guild's command set (instant propagation). The PUT
 * REPLACES every guild command, so all commands must be registered in one call —
 * registering one alone deletes the others.
 */
export async function registerGuildCommands(
  appId: string,
  guildId: string,
  token: string,
  commands: readonly unknown[],
): Promise<void> {
  const res = await fetch(`${API}/applications/${appId}/guilds/${guildId}/commands`, {
    method: "PUT",
    headers: {
      Authorization: `Bot ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(commands),
  });
  if (!res.ok) {
    throw new Error(`registerGuildCommands → ${res.status}: ${await res.text()}`);
  }
}

/** Pause between retries of a status listed in `retryOn`. */
const RETRY_DELAY_MS = 1000;

/**
 * One interaction-webhook request, up to 3 attempts: a 429 waits Discord's
 * `retry_after`; a status in `retryOn` waits RETRY_DELAY_MS; any other non-2xx
 * throws.
 */
async function webhookRequest(
  method: "PATCH" | "POST",
  url: string,
  body: unknown,
  label: string,
  retryOn: readonly number[],
): Promise<void> {
  for (let attempt = 0; attempt < 3; attempt++) {
    const res = await fetch(url, {
      method,
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(10000),
    });
    if (res.status === 429) {
      const { retry_after } = (await res.json()) as { retry_after: number };
      await Bun.sleep(Math.ceil(retry_after * 1000) + 100);
      continue;
    }
    if (retryOn.includes(res.status)) {
      await Bun.sleep(RETRY_DELAY_MS);
      continue;
    }
    if (!res.ok) {
      throw new Error(`${label} → ${res.status}: ${await res.text()}`);
    }
    return;
  }
  throw new Error(`${label} → exhausted retries`);
}

/** Edits the original (deferred) interaction response with the final content. */
export async function editOriginalResponse(
  appId: string,
  interactionToken: string,
  body: unknown,
): Promise<void> {
  await webhookRequest(
    "PATCH",
    `${API}/webhooks/${appId}/${interactionToken}/messages/@original`,
    body,
    "editOriginalResponse",
    [],
  );
}

/**
 * Wait before a follow-up's first attempt. The follow-up is sent in the same tick
 * the initial interaction response is returned, and Discord does not document
 * whether a follow-up may precede its processing of that response; waiting ~1 s
 * (and retrying a 404) avoids depending on the ordering.
 */
const FOLLOWUP_DELAY_MS = 1000;

/** Posts a follow-up message on an interaction (e.g. the /lfg ready ping). */
export async function createFollowupMessage(
  appId: string,
  interactionToken: string,
  body: unknown,
): Promise<void> {
  await Bun.sleep(FOLLOWUP_DELAY_MS);
  await webhookRequest(
    "POST",
    `${API}/webhooks/${appId}/${interactionToken}`,
    body,
    "createFollowupMessage",
    [404],
  );
}
