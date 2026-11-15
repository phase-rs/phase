#!/usr/bin/env node

import fs from "node:fs/promises";

const API = "https://discord.com/api/v10";
const THREADS_OUTPUT = "tmp/discord-threads.json";
const THREAD_MESSAGES_OUTPUT = "tmp/discord-thread-messages.json";

async function readJsonIfExists(path) {
  try {
    return JSON.parse(await fs.readFile(path, "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") {
      return null;
    }

    throw error;
  }
}

async function loadDotEnv(path) {
  const text = await fs.readFile(path, "utf8");

  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) {
      continue;
    }

    const equals = line.indexOf("=");
    if (equals === -1) {
      continue;
    }

    const key = line.slice(0, equals).trim();
    let value = line.slice(equals + 1).trim();

    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }

    process.env[key] ??= value;
  }
}

async function discordGet(path) {
  for (;;) {
    const response = await fetch(`${API}${path}`, {
      headers: {
        Authorization: `Bot ${process.env.DISCORD_BOT_TOKEN}`,
      },
    });

    if (response.status === 429) {
      const body = await response.json();
      await new Promise((resolve) =>
        setTimeout(resolve, Math.ceil(body.retry_after * 1000)),
      );
      continue;
    }

    if (!response.ok) {
      throw new Error(
        `${response.status} ${response.statusText} for ${path}: ${await response.text()}`,
      );
    }

    return response.json();
  }
}

async function getArchivedThreads(path, cutoff) {
  const threads = [];
  let before;

  for (;;) {
    const params = new URLSearchParams({ limit: "100" });
    if (before) {
      params.set("before", before);
    }

    const body = await discordGet(`${path}?${params}`);
    const recentThreads = body.threads.filter((thread) => {
      const archiveTimestamp = thread.thread_metadata?.archive_timestamp;
      return archiveTimestamp && new Date(archiveTimestamp) >= cutoff;
    });
    threads.push(...recentThreads);

    if (recentThreads.length !== body.threads.length) {
      break;
    }

    if (!body.has_more || body.threads.length === 0) {
      break;
    }

    before = body.threads.at(-1).thread_metadata?.archive_timestamp;
    if (!before) {
      break;
    }
  }

  return threads;
}

async function getMessages(channelId, { after } = {}) {
  const messages = [];
  let before;

  for (;;) {
    const params = new URLSearchParams({ limit: "100" });
    if (before) {
      params.set("before", before);
    }

    const batch = await discordGet(`/channels/${channelId}/messages?${params}`);
    const existingIndex = after
      ? batch.findIndex((message) => message.id === after)
      : -1;
    if (existingIndex !== -1) {
      messages.push(...batch.slice(0, existingIndex));
      break;
    }

    messages.push(...batch);

    if (batch.length < 100) {
      break;
    }

    before = batch.at(-1).id;
  }

  return messages.reverse().map((message) => ({
    id: message.id,
    type: message.type,
    timestamp: message.timestamp,
    edited_timestamp: message.edited_timestamp,
    author: {
      id: message.author?.id ?? null,
      username: message.author?.username ?? null,
      global_name: message.author?.global_name ?? null,
      bot: message.author?.bot ?? false,
    },
    content: message.content,
    attachments: message.attachments.map((attachment) => ({
      id: attachment.id,
      filename: attachment.filename,
      url: attachment.url,
      content_type: attachment.content_type ?? null,
      size: attachment.size,
    })),
    embeds: message.embeds.map((embed) => ({
      title: embed.title ?? null,
      description: embed.description ?? null,
      url: embed.url ?? null,
      type: embed.type ?? null,
    })),
    mentions: message.mentions.map((user) => ({
      id: user.id,
      username: user.username,
      global_name: user.global_name ?? null,
      bot: user.bot ?? false,
    })),
    referenced_message_id: message.referenced_message?.id ?? null,
  }));
}

function mergeMessages(existingMessages, newMessages) {
  const messagesById = new Map();
  for (const message of [...existingMessages, ...newMessages]) {
    messagesById.set(message.id, message);
  }

  return [...messagesById.values()].sort((left, right) =>
    left.id === right.id ? 0 : BigInt(left.id) < BigInt(right.id) ? -1 : 1,
  );
}

async function main() {
  await loadDotEnv(".env");

  const sinceDays = 14;
  const cutoff = new Date(Date.now() - sinceDays * 24 * 60 * 60 * 1000);

  const { DISCORD_BOT_TOKEN, DISCORD_GUILD_ID, DISCORD_CHANNEL_ID } = process.env;
  if (!DISCORD_BOT_TOKEN || !DISCORD_GUILD_ID || !DISCORD_CHANNEL_ID) {
    throw new Error(
      "Expected DISCORD_BOT_TOKEN, DISCORD_GUILD_ID, and DISCORD_CHANNEL_ID in .env",
    );
  }

  const activeBody = await discordGet(`/guilds/${DISCORD_GUILD_ID}/threads/active`);
  const active = activeBody.threads.filter(
    (thread) => thread.parent_id === DISCORD_CHANNEL_ID,
  );

  const publicArchived = await getArchivedThreads(
    `/channels/${DISCORD_CHANNEL_ID}/threads/archived/public`,
    cutoff,
  );

  let privateArchived = [];
  try {
    privateArchived = await getArchivedThreads(
      `/channels/${DISCORD_CHANNEL_ID}/threads/archived/private`,
      cutoff,
    );
  } catch (error) {
    console.error(`Skipping private archived threads: ${error.message}`);
  }

  const threadsById = new Map();
  for (const thread of [...active, ...publicArchived, ...privateArchived]) {
    threadsById.set(thread.id, thread);
  }

  const threads = [...threadsById.values()]
    .filter((thread) => {
      const archiveTimestamp = thread.thread_metadata?.archive_timestamp;
      return archiveTimestamp && new Date(archiveTimestamp) >= cutoff;
    })
    .map((thread) => ({
      id: thread.id,
      name: thread.name,
      parent_id: thread.parent_id,
      type: thread.type,
      archived: thread.thread_metadata?.archived ?? false,
      locked: thread.thread_metadata?.locked ?? false,
      archive_timestamp: thread.thread_metadata?.archive_timestamp ?? null,
      owner_id: thread.owner_id ?? null,
      message_count: thread.message_count ?? null,
      total_message_sent: thread.total_message_sent ?? null,
    }));

  await fs.mkdir("tmp", { recursive: true });
  await fs.writeFile(THREADS_OUTPUT, `${JSON.stringify(threads, null, 2)}\n`);

  console.log(
    `Wrote ${threads.length} threads since ${cutoff.toISOString()} to ${THREADS_OUTPUT}`,
  );

  const existingThreadMessages = await readJsonIfExists(THREAD_MESSAGES_OUTPUT);
  const existingByThreadId = new Map(
    (existingThreadMessages ?? []).map((threadMessages) => [
      threadMessages.thread.id,
      threadMessages,
    ]),
  );
  const threadMessages = [];
  for (const [index, thread] of threads.entries()) {
    const existing = existingByThreadId.get(thread.id);
    const existingMessages = existing?.messages ?? [];
    const after = existingMessages.at(-1)?.id;
    const newMessages = await getMessages(thread.id, { after });
    const messages = mergeMessages(existingMessages, newMessages);
    threadMessages.push({
      thread,
      messages,
    });
    console.log(
      `Fetched ${newMessages.length} new messages for thread ${index + 1}/${threads.length}: ${thread.name}`,
    );
  }

  await fs.writeFile(
    THREAD_MESSAGES_OUTPUT,
    `${JSON.stringify(threadMessages, null, 2)}\n`,
  );

  const messageCount = threadMessages.reduce(
    (total, thread) => total + thread.messages.length,
    0,
  );
  const emptyContentCount = threadMessages.reduce(
    (total, thread) =>
      total + thread.messages.filter((message) => message.content === "").length,
    0,
  );

  if (messageCount > 0 && emptyContentCount === messageCount) {
    console.error(
      "Warning: every fetched message has empty content. Enable the Message Content Intent in the Discord Developer Portal, then rerun this script.",
    );
  }

  console.log(
    `Wrote ${messageCount} messages across ${threadMessages.length} threads to ${THREAD_MESSAGES_OUTPUT}`,
  );
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
