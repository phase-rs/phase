import { afterEach, beforeEach, describe, expect, it } from "vitest";

import {
  claimP2PHostLease,
  hasExactP2PAuthority,
  isP2PAuthorityStamp,
  ownsP2PHostLease,
  releaseP2PHostLease,
} from "../p2pSession";

const originalLocalStorageDescriptor = Object.getOwnPropertyDescriptor(
  globalThis,
  "localStorage",
);

function setMemoryLocalStorage(): void {
  const items = new Map<string, string>();
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => items.get(key) ?? null,
      setItem: (key: string, value: string) => items.set(key, value),
      removeItem: (key: string) => items.delete(key),
    },
  });
}

beforeEach(() => {
  setMemoryLocalStorage();
});

afterEach(() => {
  if (originalLocalStorageDescriptor) {
    Object.defineProperty(globalThis, "localStorage", originalLocalStorageDescriptor);
  }
});

describe("P2P host leases", () => {
  it("requires both stable-session and incarnation values to match", () => {
    const authority = { sessionKey: "session", hostIncarnation: "current" };

    expect(hasExactP2PAuthority(authority, authority)).toBe(true);
    expect(hasExactP2PAuthority({ ...authority, hostIncarnation: "stale" }, authority)).toBe(false);
    expect(hasExactP2PAuthority({ ...authority, sessionKey: "other" }, authority)).toBe(false);
    expect(hasExactP2PAuthority(undefined, authority)).toBe(false);
  });

  it("accepts only the exact authority object shape", () => {
    expect(isP2PAuthorityStamp({ sessionKey: "session", hostIncarnation: "current" })).toBe(true);
    expect(isP2PAuthorityStamp({ sessionKey: "session", hostIncarnation: "current", extra: true })).toBe(false);
    expect(isP2PAuthorityStamp({ sessionKey: "session", hostIncarnation: 1 })).toBe(false);
    expect(isP2PAuthorityStamp(null)).toBe(false);
  });

  it("fences a stale host and does not revive it when the current host cleans up", () => {
    const stale = claimP2PHostLease("shared-session-key");
    const current = claimP2PHostLease("shared-session-key");

    expect(ownsP2PHostLease(stale)).toBe(false);
    expect(ownsP2PHostLease(current)).toBe(true);

    releaseP2PHostLease(stale);
    expect(ownsP2PHostLease(current)).toBe(true);

    releaseP2PHostLease(current);
    expect(ownsP2PHostLease(stale)).toBe(false);
  });
});
