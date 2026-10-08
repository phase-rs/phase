import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("idb-keyval", () => ({
  createStore: vi.fn(() => ({})),
  del: vi.fn().mockResolvedValue(undefined),
  get: vi.fn().mockResolvedValue(undefined),
  set: vi.fn().mockResolvedValue(undefined),
}));

import { get as idbGet, set as idbSet } from "idb-keyval";
import {
  commitP2PTerminalResult,
  isValidP2PTerminalResult,
  type P2PTerminalResult,
} from "../p2pTerminalResult";

const result: P2PTerminalResult = {
  key: "p2p-session-1",
  lease: { sessionKey: "p2p-session-1", hostIncarnation: "host-2" },
  recipient: 0,
  revision: 12,
  terminalId: "terminal-1",
  finalStateCommitment: "sha256:abc123",
  display: { winner: 0, reason: "Match conceded" },
};

describe("P2P terminal result persistence", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("pins the first key/lease/revision/terminal-id statement", async () => {
    expect(await commitP2PTerminalResult(result)).toBe(true);
    expect(idbSet).toHaveBeenCalledWith(
      "phase-p2p-terminal:p2p-session-1",
      result,
      expect.anything(),
    );
  });

  it("rejects a later conflicting terminal id for the same key", async () => {
    vi.mocked(idbGet).mockResolvedValueOnce(result);
    expect(
      await commitP2PTerminalResult({ ...result, terminalId: "terminal-2" }),
    ).toBe(false);
    expect(idbSet).not.toHaveBeenCalled();
  });

  it("rejects a result whose lease does not bind its key", () => {
    expect(isValidP2PTerminalResult({
      ...result,
      lease: { ...result.lease, sessionKey: "other-session" },
    })).toBe(false);
  });
});
