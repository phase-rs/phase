import { describe, expect, it } from "vitest";

import {
  OFFICIAL_TURN_CREDENTIALS_URL,
  resolveTurnCredentialsUrl,
} from "../turnCredentials";

describe("resolveTurnCredentialsUrl", () => {
  it("uses a self-hosted endpoint when the build supplies one", () => {
    const endpoint = "https://turn.example.test/credentials";
    expect(resolveTurnCredentialsUrl(endpoint)).toBe(endpoint);
  });

  it("keeps the official endpoint when the variable is unset", () => {
    expect(resolveTurnCredentialsUrl(undefined)).toBe(OFFICIAL_TURN_CREDENTIALS_URL);
    expect(resolveTurnCredentialsUrl("")).toBe(OFFICIAL_TURN_CREDENTIALS_URL);
  });
});
