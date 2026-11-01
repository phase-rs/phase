import "../../../test/helpers/persistedStorage";

import { beforeEach, describe, expect, it, vi } from "vitest";

import type * as LlmClient from "../llmClient";
import type { LlmProfile } from "../types";

const adapter = vi.hoisted(() => ({ query: vi.fn() }));
const transport = vi.hoisted(() => ({ executeLlmRequest: vi.fn() }));

vi.mock("../../../adapter/wasm-adapter", () => ({ getSharedAdapter: () => adapter }));
vi.mock("../llmClient", async (importOriginal) => ({
  ...(await importOriginal<typeof LlmClient>()),
  executeLlmRequest: transport.executeLlmRequest,
}));

import { testLlmEndpoint } from "../probe";

const PROFILE: LlmProfile = {
  id: "p",
  name: "",
  provider: "OpenAi",
  baseUrl: null,
  apiKey: "k",
  model: "m",
  maxOutputTokens: null,
  temperature: null,
  enabled: true,
};

describe("testLlmEndpoint", () => {
  beforeEach(() => {
    adapter.query.mockReset();
    transport.executeLlmRequest.mockReset();
    transport.executeLlmRequest.mockResolvedValue({ status: 200, body: "{}" });
  });

  it("passes when the engine accepts the reply", async () => {
    adapter.query
      .mockResolvedValueOnce({ request: { url: "https://api.example" } })
      .mockResolvedValueOnce({ ok: true });

    await expect(testLlmEndpoint(PROFILE)).resolves.toEqual({ ok: true });
    expect(adapter.query).toHaveBeenLastCalledWith("validateLlmProbeResponse", "OpenAi", 200, "{}");
  });

  it("reports the engine unavailable when it cannot build the request", async () => {
    adapter.query.mockRejectedValueOnce(new Error("worker gone"));

    await expect(testLlmEndpoint(PROFILE)).resolves.toEqual({
      ok: false,
      code: "engineUnavailable",
      detail: "worker gone",
    });
    expect(transport.executeLlmRequest).not.toHaveBeenCalled();
  });

  it("reports the engine unavailable when it stops answering during the request", async () => {
    adapter.query
      .mockResolvedValueOnce({ request: { url: "https://api.example" } })
      .mockRejectedValueOnce(new Error("worker gone"));

    await expect(testLlmEndpoint(PROFILE)).resolves.toEqual({
      ok: false,
      code: "engineUnavailable",
      detail: "worker gone",
    });
  });
});
