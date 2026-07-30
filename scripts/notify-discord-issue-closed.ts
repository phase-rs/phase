#!/usr/bin/env bun

import { readFileSync } from "node:fs";

const API = "https://discord.com/api/v10";

interface Args {
  dryRun: boolean;
  event?: string;
}

interface GithubIssue {
  body?: string | null;
  html_url?: string;
  number?: number;
  title?: string;
  // GitHub sends one of "completed" | "not_planned" | "duplicate" on a closed
  // issue. Only "completed" means a fix shipped and is worth asking users to test.
  state_reason?: string | null;
}

interface GithubEventPayload {
  issue?: GithubIssue;
}

interface DiscordRef {
  threadId: string;
  messageId?: string;
  source: string;
}

interface DiscordPostedMessage {
  id: string;
}

class DiscordRequestError extends Error {
  status: number;
  body: string;
  discordCode?: number;

  constructor(status: number, statusText: string, method: string, path: string, body: string) {
    super(`${status} ${statusText} for ${method} ${path}: ${body}`);
    this.status = status;
    this.body = body;
    try {
      const parsed = JSON.parse(body) as { code?: number };
      this.discordCode = parsed.code;
    } catch {
      this.discordCode = undefined;
    }
  }
}

function parseArgs(argv: string[]): Args {
  const args: Args = { dryRun: false };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--dry-run") {
      args.dryRun = true;
    } else if (arg === "--event") {
      args.event = argv[++i];
    } else if (arg.startsWith("--event=")) {
      args.event = arg.slice("--event=".length);
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }
  return args;
}

function readIssue(args: Args): GithubIssue {
  const eventPath = args.event ?? Bun.env.GITHUB_EVENT_PATH;
  if (eventPath !== undefined && eventPath !== "") {
    const payload = JSON.parse(readFileSync(eventPath, "utf8")) as GithubEventPayload;
    if (payload.issue === undefined) {
      throw new Error(`No issue object found in event payload ${eventPath}`);
    }
    return payload.issue;
  }

  if (Bun.env.ISSUE_BODY !== undefined) {
    return {
      body: Bun.env.ISSUE_BODY,
      html_url: Bun.env.ISSUE_URL ?? "",
      number: Number(Bun.env.ISSUE_NUMBER ?? 0),
      title: Bun.env.ISSUE_TITLE ?? "",
    };
  }

  throw new Error("Expected GITHUB_EVENT_PATH, --event, or ISSUE_BODY");
}

function extractDiscordRef(body: string): DiscordRef | null {
  const patterns: Array<{
    name: string;
    regex: RegExp;
    threadIndex: number;
    messageIndex?: number;
  }> = [
    {
      name: "phase-discord-thread-id comment",
      regex: /<!--\s*phase-discord-thread-id:\s*(\d+)\s*-->/i,
      threadIndex: 1,
    },
    {
      name: "visible Discord thread id field",
      regex: /\*\*Discord thread id:\*\*\s*`?(\d+)`?/i,
      threadIndex: 1,
    },
    {
      name: "legacy discord footer",
      regex: /discord:\s*`?(\d+)\/(\d+)`?/i,
      threadIndex: 1,
      messageIndex: 2,
    },
    {
      name: "legacy report_id footer",
      regex: /report_id:\s*`?discord:(\d+):(\d+):\d+`?/i,
      threadIndex: 1,
      messageIndex: 2,
    },
    {
      name: "Discord message URL",
      regex: /https:\/\/discord\.com\/channels\/\d+\/(\d+)\/(\d+)/i,
      threadIndex: 1,
      messageIndex: 2,
    },
  ];

  for (const pattern of patterns) {
    const match = body.match(pattern.regex);
    if (match === null) continue;
    return {
      threadId: match[pattern.threadIndex],
      messageId: pattern.messageIndex === undefined ? undefined : match[pattern.messageIndex],
      source: pattern.name,
    };
  }

  return null;
}

// An unset GitHub Actions variable arrives as "" (not undefined), so treat empty
// the same as missing and fall back to a plain, non-linking channel name.
function resolveBugreportsMention(): string {
  const channelId = Bun.env.DISCORD_BUGREPORTS_CHANNEL_ID;
  if (channelId !== undefined && channelId !== "") {
    return `<#${channelId}>`;
  }
  return "#bugreports";
}

function buildFollowupMessage(issue: GithubIssue, bugreportsMention: string): string {
  const issueNumber = Number(issue.number ?? 0);
  const issueLabel = issueNumber > 0 ? `#${issueNumber}` : "The GitHub issue";
  const issueUrl = issue.html_url ?? "";
  const closeLine = `${issueLabel} tracking this report was closed${issueUrl === "" ? "." : `: ${issueUrl}`}`;

  // Only a "completed" close means a fix shipped — that's the one case where we
  // ask the reporter to retest. Every other reason (not_planned, duplicate)
  // gets neutral wording with no "please test" ask. Either way the thread is
  // resolved (archived by main()), so we point future reports at a fresh thread.
  if (issue.state_reason === "completed") {
    return [
      closeLine,
      `Please test the fix in the latest build. If it's still broken, open a **new thread** in ${bugreportsMention} — this thread is now resolved.`,
    ].join("\n");
  }
  return [
    closeLine,
    `This thread is now resolved. If you have new information, please open a **new thread** in ${bugreportsMention}.`,
  ].join("\n");
}

async function discordRequest<T>(
  token: string,
  method: "POST" | "PATCH",
  path: string,
  body: unknown,
): Promise<T> {
  for (;;) {
    const response = await fetch(`${API}${path}`, {
      method,
      headers: {
        Authorization: `Bot ${token}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(body),
    });

    if (response.status === 429) {
      const retry = (await response.json()) as { retry_after: number };
      await Bun.sleep(Math.ceil(retry.retry_after * 1000));
      continue;
    }

    if (!response.ok) {
      throw new DiscordRequestError(
        response.status,
        response.statusText,
        method,
        path,
        await response.text(),
      );
    }

    return response.json() as Promise<T>;
  }
}

async function createMessage(
  token: string,
  threadId: string,
  content: string,
): Promise<DiscordPostedMessage> {
  return discordRequest<DiscordPostedMessage>(
    token,
    "POST",
    `/channels/${threadId}/messages`,
    { content },
  );
}

// PATCH a thread's archived state. Unarchiving a non-locked thread only needs
// send permission; ARCHIVING a thread the bot does not own requires
// MANAGE_THREADS in the channel — callers that archive must tolerate failure.
async function setThreadArchived(token: string, threadId: string, archived: boolean): Promise<void> {
  await discordRequest<unknown>(
    token,
    "PATCH",
    `/channels/${threadId}`,
    { archived },
  );
}

async function postFollowup(
  token: string,
  threadId: string,
  content: string,
): Promise<DiscordPostedMessage> {
  try {
    return await createMessage(token, threadId, content);
  } catch (error) {
    if (!(error instanceof DiscordRequestError)) throw error;
    if (error.discordCode !== 50083 && !error.body.includes("archived thread")) {
      throw error;
    }
    console.log(`Thread ${threadId} is archived; unarchiving before posting follow-up.`);
    await setThreadArchived(token, threadId, false);
    return createMessage(token, threadId, content);
  }
}

async function main(): Promise<void> {
  const args = parseArgs(process.argv.slice(2));
  const issue = readIssue(args);
  const discordRef = extractDiscordRef(issue.body ?? "");

  if (discordRef === null) {
    console.log("No Discord thread id found in the issue body; skipping Discord follow-up.");
    return;
  }

  const content = buildFollowupMessage(issue, resolveBugreportsMention());
  if (args.dryRun) {
    console.log(
      `Dry run: would post to Discord thread ${discordRef.threadId} (${discordRef.source}), then archive it to mark it resolved.`,
    );
    console.log(content);
    return;
  }

  const token = Bun.env.DISCORD_BOT_TOKEN;
  if (token === undefined || token === "") {
    throw new Error("DISCORD_BOT_TOKEN is required to post the Discord follow-up");
  }

  const posted = await postFollowup(token, discordRef.threadId, content);
  console.log(`Posted Discord issue-close follow-up ${posted.id} to thread ${discordRef.threadId}.`);

  // Archive the thread to mark it resolved. This is best-effort: if the bot
  // lacks MANAGE_THREADS the follow-up message has already posted, so a failed
  // archive must not fail the workflow (which would leave partial state and
  // could double-post on a rerun).
  try {
    await setThreadArchived(token, discordRef.threadId, true);
    console.log(`Archived thread ${discordRef.threadId} to mark the report resolved.`);
  } catch (error) {
    console.warn(
      `Could not archive thread ${discordRef.threadId} (does the bot have MANAGE_THREADS?): ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
}

main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exit(1);
});
