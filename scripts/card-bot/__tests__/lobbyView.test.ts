import { describe, expect, test } from "bun:test";

import { BUILD_ENDPOINTS, BUILDS } from "../config";
import { ButtonStyle, ComponentType } from "../discord";
import { BUILD_COLORS } from "../lfgView";
import { escapeMarkdown, type LobbyRoom, renderLobbyPost } from "../lobbyView";

function room(overrides: Partial<LobbyRoom> = {}): LobbyRoom {
  return {
    code: "AB12CD",
    createdAt: 1_700_000_000,
    format: "Commander",
    name: "Casual pod",
    host: "alice",
    current: 2,
    max: 4,
    ...overrides,
  };
}

describe("renderLobbyPost", () => {
  test("the title carries the format label, the seats and the build tag", () => {
    const post = renderLobbyPost("preview", room({ format: "DuelCommander", current: 1, max: 2 }));
    expect(post.embeds[0].title).toBe("Duel Commander · 1/2 · PREVIEW");
    expect(renderLobbyPost("release", room()).embeds[0].title).toBe("Commander · 2/4 · RELEASE");
  });

  test("each build has its own color", () => {
    for (const build of BUILDS) {
      expect(renderLobbyPost(build, room()).embeds[0].color).toBe(BUILD_COLORS[build]);
    }
    expect(BUILD_COLORS.release).not.toBe(BUILD_COLORS.preview);
  });

  test("the Join button links the code at the build's official lobby", () => {
    for (const build of BUILDS) {
      const { site, lobbyWs } = BUILD_ENDPOINTS[build];
      expect(renderLobbyPost(build, room()).components).toEqual([
        {
          type: ComponentType.ACTION_ROW,
          components: [
            {
              type: ComponentType.BUTTON,
              style: ButtonStyle.LINK,
              label: "Join",
              url: `${site}/multiplayer?${new URLSearchParams({ join: `AB12CD@${lobbyWs}` })}`,
            },
          ],
        },
      ]);
    }
  });

  test("the description names the room, its host and the site; mentions are off", () => {
    const post = renderLobbyPost("release", room());
    expect(post.embeds[0].description).toBe(
      "Casual pod\nHost: alice\nSite: release (phase-rs.dev)\nOpen in the lobby — press Join to take a seat.",
    );
    expect(renderLobbyPost("release", room({ name: "alice", host: null })).embeds[0].description).toBe(
      "alice\nSite: release (phase-rs.dev)\nOpen in the lobby — press Join to take a seat.",
    );
    expect(post.allowed_mentions).toEqual({ parse: [] });
  });

  test("a full room says so and has no Join button", () => {
    const post = renderLobbyPost("release", room({ current: 4, max: 4 }));
    expect(post.components).toEqual([]);
    expect(post.embeds[0].title).toBe("Commander · 4/4 · RELEASE");
    expect(post.embeds[0].description?.split("\n").at(-1)).toBe("Full — waiting for the host to start.");
  });

  test("player-chosen names are markdown-escaped", () => {
    const post = renderLobbyPost("release", room({ name: "**big** _pod_", host: "<@1> ~x~" }));
    expect(post.embeds[0].description).toStartWith("\\*\\*big\\*\\* \\_pod\\_\nHost: \\<@1\\> \\~x\\~\n");
  });

  test("an unknown format shows its key; no format shows Game", () => {
    expect(renderLobbyPost("release", room({ format: "NewFormat" })).embeds[0].title).toBe("NewFormat · 2/4 · RELEASE");
    expect(renderLobbyPost("release", room({ format: null })).embeds[0].title).toBe("Game · 2/4 · RELEASE");
  });
});

test("escapeMarkdown escapes every markdown metacharacter and nothing else", () => {
  expect(escapeMarkdown("\\*_~`|>[]#-<")).toBe("\\\\\\*\\_\\~\\`\\|\\>\\[\\]\\#\\-\\<");
  expect(escapeMarkdown("Tom's room 2.0 (casual)!")).toBe("Tom's room 2.0 (casual)!");
});
