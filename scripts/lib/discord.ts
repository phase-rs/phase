const API = "https://discord.com/api/v10";

export interface DiscordThread {
  id: string;
  name: string;
  parent_id: string;
  type: number;
  owner_id: string | null;
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

export async function discordGet<T>(path: string): Promise<T> {
  const token = Bun.env.DISCORD_BOT_TOKEN;
  for (;;) {
    const response = await fetch(`${API}${path}`, {
      headers: { Authorization: `Bot ${token}` },
    });

    if (response.status === 429) {
      const body = (await response.json()) as { retry_after: number };
      await Bun.sleep(Math.ceil(body.retry_after * 1000));
      continue;
    }

    if (!response.ok) {
      throw new Error(
        `${response.status} ${response.statusText} for ${path}: ${await response.text()}`,
      );
    }

    return response.json() as Promise<T>;
  }
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
): Promise<DiscordThread[]> {
  const threads: DiscordThread[] = [];
  let before: string | undefined;

  for (;;) {
    const params = new URLSearchParams({ limit: "100" });
    if (before !== undefined) params.set("before", before);

    const body = await discordGet<{ threads: DiscordThread[]; has_more: boolean }>(
      `/channels/${channelId}/threads/archived/${type}?${params}`,
    );
    threads.push(...body.threads);

    if (!body.has_more || body.threads.length === 0) break;

    before = body.threads.at(-1)?.thread_metadata?.archive_timestamp;
    if (!before) break;
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
