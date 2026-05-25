// Minimal Discord interactions plumbing, dependency-free (matches scripts/).
// Ed25519 verification uses Bun's WebCrypto; REST uses fetch.

const API = "https://discord.com/api/v10";

/** Interaction request types (Discord `InteractionType`). */
export const InteractionType = {
  PING: 1,
  APPLICATION_COMMAND: 2,
  APPLICATION_COMMAND_AUTOCOMPLETE: 4,
} as const;

/** Interaction response types (Discord `InteractionResponseType`). */
export const ResponseType = {
  PONG: 1,
  CHANNEL_MESSAGE_WITH_SOURCE: 4,
  DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE: 5,
  APPLICATION_COMMAND_AUTOCOMPLETE_RESULT: 8,
} as const;

/** Slash-command option types we use. */
export const OptionType = {
  STRING: 3,
} as const;

export interface InteractionOption {
  name: string;
  type: number;
  value?: string | number | boolean;
  focused?: boolean;
}

export interface Interaction {
  type: number;
  application_id: string;
  token: string;
  data?: {
    name: string;
    options?: InteractionOption[];
  };
}

function hexToBytes(hex: string): Uint8Array {
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

/** Bulk-overwrites the guild's command set (instant propagation). */
export async function registerGuildCommand(
  appId: string,
  guildId: string,
  token: string,
  command: unknown,
): Promise<void> {
  const res = await fetch(`${API}/applications/${appId}/guilds/${guildId}/commands`, {
    method: "PUT",
    headers: {
      Authorization: `Bot ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify([command]),
  });
  if (!res.ok) {
    throw new Error(`registerGuildCommand → ${res.status}: ${await res.text()}`);
  }
}

/** Edits the original (deferred) interaction response with the final content. */
export async function editOriginalResponse(
  appId: string,
  interactionToken: string,
  body: unknown,
): Promise<void> {
  const url = `${API}/webhooks/${appId}/${interactionToken}/messages/@original`;
  for (let attempt = 0; attempt < 3; attempt++) {
    const res = await fetch(url, {
      method: "PATCH",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(10000),
    });
    if (res.status === 429) {
      const { retry_after } = (await res.json()) as { retry_after: number };
      await Bun.sleep(Math.ceil(retry_after * 1000) + 100);
      continue;
    }
    if (!res.ok) {
      throw new Error(`editOriginalResponse → ${res.status}: ${await res.text()}`);
    }
    return;
  }
  throw new Error("editOriginalResponse → exhausted retries");
}
