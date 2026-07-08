const API = "https://discord.com/api/v10";

export interface DiscordThread {
  id: string;
  name: string;
  parent_id: string;
  type: number;
  owner_id: string | null;
  last_message_id?: string | null;
  message_count: number | null;
  total_message_sent: number | null;
  thread_metadata?: {
    archived?: boolean;
    locked?: boolean;
    archive_timestamp?: string;
  };
}

export interface DiscordMessage {
  id: string;
  type: number;
  timestamp: string;
  edited_timestamp: string | null;
  author: {
    id: string;
    username: string;
    global_name: string | null;
    bot?: boolean;
  };
  content: string;
  attachments: Array<{
    id: string;
    filename: string;
    url: string;
    content_type?: string;
    size: number;
  }>;
  embeds: Array<{
    title?: string;
    description?: string;
    url?: string;
    type?: string;
  }>;
  mentions: Array<{
    id: string;
    username: string;
    global_name: string | null;
    bot?: boolean;
  }>;
  referenced_message?: { id: string } | null;
  referenced_message_id: string | null;
}

async function discordRequest<T>(
  method: "GET" | "POST" | "PUT" | "PATCH" | "DELETE",
  path: string,
  body?: unknown,
): Promise<T | null> {
  const token = Bun.env.DISCORD_BOT_TOKEN;
  for (;;) {
    const response = await fetch(`${API}${path}`, {
      method,
      headers: {
        Authorization: `Bot ${token}`,
        ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
      },
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });

    if (response.status === 429) {
      const retry = (await response.json()) as { retry_after: number };
      await Bun.sleep(Math.ceil(retry.retry_after * 1000));
      continue;
    }

    if (!response.ok) {
      throw new Error(
        `${response.status} ${response.statusText} for ${method} ${path}: ${await response.text()}`,
      );
    }

    if (response.status === 204) return null;
    return response.json() as Promise<T>;
  }
}

export async function discordGet<T>(path: string): Promise<T> {
  return (await discordRequest<T>("GET", path)) as T;
}

// PUT /channels/{channel.id}/messages/{message.id}/reactions/{emoji}/@me — CR n/a, Discord API.
// `channelId` is the thread id for thread reactions; emoji must be URL-encoded.
export async function addReaction(
  channelId: string,
  messageId: string,
  emoji: string,
): Promise<void> {
  const encoded = encodeURIComponent(emoji);
  await discordRequest<null>(
    "PUT",
    `/channels/${channelId}/messages/${messageId}/reactions/${encoded}/@me`,
  );
}

export interface DiscordPostedMessage {
  id: string;
  channel_id: string;
}

// DELETE /channels/{channel.id}/messages/{message.id}/reactions/{emoji}/@me
export async function removeOwnReaction(
  channelId: string,
  messageId: string,
  emoji: string,
): Promise<void> {
  const encoded = encodeURIComponent(emoji);
  await discordRequest<null>(
    "DELETE",
    `/channels/${channelId}/messages/${messageId}/reactions/${encoded}/@me`,
  );
}

// DELETE /channels/{channel.id}/messages/{message.id}
export async function deleteMessage(
  channelId: string,
  messageId: string,
): Promise<void> {
  await discordRequest<null>(
    "DELETE",
    `/channels/${channelId}/messages/${messageId}`,
  );
}

// PATCH /channels/{thread.id} — toggles archived state. Requires MANAGE_THREADS.
// Used to unarchive a thread before writing into it (Discord rejects reactions
// and messages on archived threads with error 50083).
export async function setThreadArchived(
  threadId: string,
  archived: boolean,
): Promise<void> {
  await discordRequest<unknown>(
    "PATCH",
    `/channels/${threadId}`,
    { archived },
  );
}

// POST /channels/{channel.id}/messages — when `channelId` is a thread id, this posts inside the thread.
export async function createMessage(
  channelId: string,
  content: string,
): Promise<DiscordPostedMessage> {
  const posted = await discordRequest<DiscordPostedMessage>(
    "POST",
    `/channels/${channelId}/messages`,
    { content },
  );
  if (posted === null) {
    throw new Error(`createMessage to channel ${channelId} returned no body`);
  }
  return posted;
}

// PATCH /channels/{channel.id}/messages/{message.id} — edit the bot's own message.
export async function editMessage(
  channelId: string,
  messageId: string,
  content: string,
): Promise<void> {
  await discordRequest<DiscordPostedMessage>(
    "PATCH",
    `/channels/${channelId}/messages/${messageId}`,
    { content },
  );
}

// POST /channels/{channel.id}/messages/{message.id}/crosspost — publish a message
// from an Announcement/News channel (type 5) to all servers following it.
// Authoring the message only requires SEND_MESSAGES (MANAGE_MESSAGES is needed
// only to crosspost someone else's message).
export async function crosspostMessage(
  channelId: string,
  messageId: string,
): Promise<void> {
  await discordRequest<DiscordPostedMessage>(
    "POST",
    `/channels/${channelId}/messages/${messageId}/crosspost`,
  );
}

export async function fetchActiveThreads(
  guildId: string,
  channelId: string,
): Promise<DiscordThread[]> {
  const body = await discordGet<{ threads: DiscordThread[] }>(
    `/guilds/${guildId}/threads/active`,
  );
  return body.threads.filter((t) => t.parent_id === channelId);
}

export async function fetchArchivedThreads(
  channelId: string,
  type: "public" | "private",
  afterArchiveTimestamp?: string,
): Promise<DiscordThread[]> {
  const threads: DiscordThread[] = [];
  let before: string | undefined;

  for (;;) {
    const params = new URLSearchParams({ limit: "100" });
    if (before !== undefined) params.set("before", before);

    const body = await discordGet<{ threads: DiscordThread[]; has_more: boolean }>(
      `/channels/${channelId}/threads/archived/${type}?${params}`,
    );
    threads.push(
      ...body.threads.filter((thread) => {
        const archivedAt = thread.thread_metadata?.archive_timestamp;
        return (
          afterArchiveTimestamp === undefined ||
          archivedAt === undefined ||
          archivedAt > afterArchiveTimestamp
        );
      }),
    );

    if (!body.has_more || body.threads.length === 0) break;

    before = body.threads.at(-1)?.thread_metadata?.archive_timestamp;
    if (!before) break;
    if (afterArchiveTimestamp !== undefined && before <= afterArchiveTimestamp) break;
  }

  return threads;
}

export async function fetchMessages(
  channelId: string,
  after?: string,
): Promise<DiscordMessage[]> {
  const messages: DiscordMessage[] = [];
  let before: string | undefined;

  for (;;) {
    const params = new URLSearchParams({ limit: "100" });
    if (before !== undefined) params.set("before", before);

    const batch = await discordGet<DiscordMessage[]>(
      `/channels/${channelId}/messages?${params}`,
    );

    const existingIndex =
      after !== undefined ? batch.findIndex((m) => m.id === after) : -1;

    if (existingIndex !== -1) {
      messages.push(...batch.slice(0, existingIndex));
      break;
    }

    messages.push(...batch);

    if (batch.length < 100) break;

    before = batch.at(-1)?.id;
  }

  // Return in chronological order with normalized shape
  return messages.reverse().map((m) => ({
    id: m.id,
    type: m.type,
    timestamp: m.timestamp,
    edited_timestamp: m.edited_timestamp,
    author: {
      id: m.author?.id ?? "",
      username: m.author?.username ?? "",
      global_name: m.author?.global_name ?? null,
      bot: m.author?.bot ?? false,
    },
    content: m.content,
    attachments: (m.attachments ?? []).map((a) => ({
      id: a.id,
      filename: a.filename,
      url: a.url,
      content_type: a.content_type ?? null,
      size: a.size,
    })),
    embeds: (m.embeds ?? []).map((e) => ({
      title: e.title ?? null,
      description: e.description ?? null,
      url: e.url ?? null,
      type: e.type ?? null,
    })),
    mentions: (m.mentions ?? []).map((u) => ({
      id: u.id,
      username: u.username,
      global_name: u.global_name ?? null,
      bot: u.bot ?? false,
    })),
    referenced_message_id: m.referenced_message?.id ?? m.referenced_message_id ?? null,
  }));
}
