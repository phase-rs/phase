import assert from "node:assert/strict";
import test from "node:test";

import { moderationErrorForLobbyFrame } from "../src/name-filter.ts";

function frame(type, data) {
  return JSON.stringify({ type, data });
}

test("allows unrelated broker frames", () => {
  assert.equal(moderationErrorForLobbyFrame(frame("SubscribeLobby", {})), null);
});

test("allows ordinary player and room names", () => {
  assert.equal(
    moderationErrorForLobbyFrame(frame("CreateGameWithSettings", {
      display_name: "Alice",
      room_name: "Friday Commander",
    })),
    null,
  );
});

test("rejects blocked player names on host, join, and lookup frames", () => {
  for (const type of ["CreateGameWithSettings", "JoinGameWithPassword", "LookupJoinTarget"]) {
    assert.equal(
      moderationErrorForLobbyFrame(frame(type, { display_name: "f4gg0t" })),
      "Player name is not allowed on the public lobby.",
    );
  }
});

test("rejects blocked room names on host frames", () => {
  assert.equal(
    moderationErrorForLobbyFrame(frame("CreateGameWithSettings", {
      display_name: "Alice",
      room_name: "kill yourself table",
    })),
    "Room name is not allowed on the public lobby.",
  );
});

test("rejects links and control characters", () => {
  assert.equal(
    moderationErrorForLobbyFrame(frame("CreateGameWithSettings", {
      display_name: "Alice",
      room_name: "www.example.test",
    })),
    "Room name cannot include links.",
  );
  assert.equal(
    moderationErrorForLobbyFrame(frame("JoinGameWithPassword", {
      display_name: "Alice\u0000",
    })),
    "Player name contains unsupported characters.",
  );
});

test("enforces public display length limits", () => {
  assert.equal(
    moderationErrorForLobbyFrame(frame("JoinGameWithPassword", {
      display_name: "abcdefghijklmnopqrstu",
    })),
    "Player name must be 20 characters or fewer.",
  );
  assert.equal(
    moderationErrorForLobbyFrame(frame("CreateGameWithSettings", {
      display_name: "Alice",
      room_name: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    })),
    "Room name must be 40 characters or fewer.",
  );
});
