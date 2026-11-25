import { afterEach, describe, expect, it, vi } from "vitest";

const { createClientMock } = vi.hoisted(() => ({
  createClientMock: vi.fn(() => ({})),
}));

vi.mock("@supabase/supabase-js", () => ({
  createClient: createClientMock,
}));

// SUPABASE_CONFIG is resolved once at module load, so every case sets
// window.__PHASE_CONFIG__ (and, where relevant, the build-time defines) first
// and then imports fresh copies of the modules that consume it.
async function loadWith(config: unknown) {
  vi.resetModules();
  if (config === undefined) {
    delete (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__;
  } else {
    (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__ = config;
  }
  const [client, sessionKey] = await Promise.all([
    import("../supabaseClient"),
    import("../sessionKey"),
  ]);
  return { client, sessionKey };
}

function stubBuildDefines(url: string, anonKey: string) {
  vi.stubGlobal("__SUPABASE_URL__", url);
  vi.stubGlobal("__SUPABASE_ANON_KEY__", anonKey);
}

const BUILD_URL = "https://official-ref.supabase.co";
const BUILD_KEY = "sb_publishable_official";
const SELF_HOSTED_URL = "https://selfhosted-ref.supabase.co";
const SELF_HOSTED_KEY = "sb_publishable_selfhosted";

afterEach(() => {
  delete (window as { __PHASE_CONFIG__?: unknown }).__PHASE_CONFIG__;
  vi.unstubAllGlobals();
  vi.resetModules();
  createClientMock.mockClear();
});

describe("Supabase runtime override", () => {
  it("leaves cloud sync disabled when neither the build nor the runtime configures it", async () => {
    const { client, sessionKey } = await loadWith({});
    expect(client.isSupabaseConfigured()).toBe(false);
    expect(sessionKey.getSupabaseSessionKey()).toBeNull();
  });

  it("uses the build-time project when no runtime config is present", async () => {
    stubBuildDefines(BUILD_URL, BUILD_KEY);
    const { client, sessionKey } = await loadWith(undefined);

    expect(client.isSupabaseConfigured()).toBe(true);
    client.getSupabaseClient();
    expect(createClientMock).toHaveBeenCalledWith(BUILD_URL, BUILD_KEY, expect.anything());
    expect(sessionKey.getSupabaseSessionKey()).toBe("sb-official-ref-auth-token");
  });

  it("points the client and its session key at the runtime project over the build-time one", async () => {
    stubBuildDefines(BUILD_URL, BUILD_KEY);
    const { client, sessionKey } = await loadWith({
      supabaseUrl: SELF_HOSTED_URL,
      supabaseAnonKey: SELF_HOSTED_KEY,
    });

    expect(client.isSupabaseConfigured()).toBe(true);
    client.getSupabaseClient();
    expect(createClientMock).toHaveBeenCalledWith(
      SELF_HOSTED_URL,
      SELF_HOSTED_KEY,
      expect.anything(),
    );
    // The legacy-migration and desktop-backup paths read the session through
    // this key, so it must name the same project the client signs in to.
    expect(sessionKey.getSupabaseSessionKey()).toBe("sb-selfhosted-ref-auth-token");
  });

  it("enables cloud sync on a build that shipped without Supabase", async () => {
    const { client } = await loadWith({
      supabaseUrl: SELF_HOSTED_URL,
      supabaseAnonKey: SELF_HOSTED_KEY,
    });

    expect(client.isSupabaseConfigured()).toBe(true);
    client.getSupabaseClient();
    expect(createClientMock).toHaveBeenCalledWith(
      SELF_HOSTED_URL,
      SELF_HOSTED_KEY,
      expect.anything(),
    );
  });

  // The URL and key identify one project; pairing one project's URL with
  // another's key would fail every request with nothing to say why.
  it.each([
    ["only a URL", { supabaseUrl: SELF_HOSTED_URL }],
    ["only an anon key", { supabaseAnonKey: SELF_HOSTED_KEY }],
    ["a non-http(s) URL", { supabaseUrl: "wss://selfhosted-ref.supabase.co", supabaseAnonKey: SELF_HOSTED_KEY }],
    ["a malformed URL", { supabaseUrl: "selfhosted-ref.supabase.co", supabaseAnonKey: SELF_HOSTED_KEY }],
    ["an empty anon key", { supabaseUrl: SELF_HOSTED_URL, supabaseAnonKey: "" }],
    ["a non-string anon key", { supabaseUrl: SELF_HOSTED_URL, supabaseAnonKey: 1234 }],
  ])("ignores %s and keeps the build-time project", async (_label, config) => {
    stubBuildDefines(BUILD_URL, BUILD_KEY);
    const { client, sessionKey } = await loadWith(config);

    client.getSupabaseClient();
    expect(createClientMock).toHaveBeenCalledWith(BUILD_URL, BUILD_KEY, expect.anything());
    expect(sessionKey.getSupabaseSessionKey()).toBe("sb-official-ref-auth-token");
  });
});
