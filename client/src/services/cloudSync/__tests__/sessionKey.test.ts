import { describe, expect, it } from "vitest";

import { supabaseSessionKeyFromUrl } from "../sessionKey";

describe("supabaseSessionKeyFromUrl", () => {
  it("matches supabase-js's project-ref storage key", () => {
    expect(supabaseSessionKeyFromUrl("https://abc-123.supabase.co")).toBe(
      "sb-abc-123-auth-token",
    );
  });

  it("returns null for an unconfigured or malformed URL", () => {
    expect(supabaseSessionKeyFromUrl("")).toBeNull();
    expect(supabaseSessionKeyFromUrl("not a URL")).toBeNull();
  });
});
