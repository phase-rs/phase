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

test("rejects blocked player names on host, join, lookup, and tournament-join frames", () => {
  for (const type of [
    "CreateGameWithSettings",
    "JoinGameWithPassword",
    "LookupJoinTarget",
    "JoinTournament",
  ]) {
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

// Tournament frames broadcast `name` (via TournamentSummary) and `display_name`
// (via the entrant list) to every lobby subscriber, exactly like `room_name` and
// a game's `display_name`, so they carry the same public-lobby policy. Both
// fields are required (non-Option) on the Rust side, hence the non-optional
// validator in both cases.
test("allows ordinary tournament and entrant names", () => {
  assert.equal(
    moderationErrorForLobbyFrame(frame("CreateTournament", {
      name: "Friday Night Modern",
      arity: "Singles",
      scoring: "MatchPoints",
      bracket: "SwissRounds",
    })),
    null,
  );
  assert.equal(
    moderationErrorForLobbyFrame(frame("JoinTournament", {
      code: "ABCD",
      player_key: "player-key-1",
      display_name: "Alice",
    })),
    null,
  );
});

test("rejects blocked tournament names on create frames", () => {
  assert.equal(
    moderationErrorForLobbyFrame(frame("CreateTournament", {
      name: "kill yourself invitational",
      arity: "Singles",
      scoring: "MatchPoints",
      bracket: "SwissRounds",
    })),
    "Tournament name is not allowed on the public lobby.",
  );
});

test("rejects blocked entrant names on tournament join frames", () => {
  assert.equal(
    moderationErrorForLobbyFrame(frame("JoinTournament", {
      code: "ABCD",
      player_key: "player-key-1",
      display_name: "f4gg0t",
    })),
    "Player name is not allowed on the public lobby.",
  );
});

test("enforces tournament display length limits", () => {
  assert.equal(
    moderationErrorForLobbyFrame(frame("CreateTournament", {
      name: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      arity: "Singles",
      scoring: "MatchPoints",
      bracket: "SwissRounds",
    })),
    "Tournament name must be 40 characters or fewer.",
  );
  assert.equal(
    moderationErrorForLobbyFrame(frame("JoinTournament", {
      code: "ABCD",
      player_key: "player-key-1",
      display_name: "abcdefghijklmnopqrstu",
    })),
    "Player name must be 20 characters or fewer.",
  );
});
