import { describe, it } from "vitest";

describe("P2P Protocol - validateMessage", () => {
  it.todo("accepts valid P2P message types");
  it.todo("rejects unknown message types");
});

describe("PeerSession", () => {
  it.todo("send returns false when connection is not open");
  it.todo("onMessage handler receives parsed messages");
  it.todo("buffers messages when no listeners are attached, then flushes on subscribe");
  it.todo("invokes disconnect handlers immediately if subscribed after disconnect");
});
