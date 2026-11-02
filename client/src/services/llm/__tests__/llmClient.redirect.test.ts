// @vitest-environment node
//
// Real sockets and Node's own fetch: the redirect algorithm is the thing under
// test, so a stubbed `fetch` could not tell a transport that refuses to follow
// from one that merely asks to.
import { createServer, type IncomingMessage, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { executeLlmRequest, LlmTransportError } from "../llmClient";
import type { LlmHttpRequestSpec } from "../types";

const KEY = "jev-key-SECRET-31f9";

interface Listener {
  server: Server;
  origin: string;
  requests: { method: string; body: string }[];
}

async function listen(
  handler: (request: IncomingMessage, respond: (status: number, headers?: Record<string, string>) => void) => void,
  requests: Listener["requests"],
): Promise<Listener> {
  const server = createServer((request, response) => {
    const chunks: Buffer[] = [];
    request.on("data", (chunk: Buffer) => chunks.push(chunk));
    request.on("end", () => {
      requests.push({ method: request.method ?? "", body: Buffer.concat(chunks).toString("utf8") });
      handler(request, (status, headers = {}) => {
        response.writeHead(status, headers);
        response.end('{"answers":{}}');
      });
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as AddressInfo;
  return { server, origin: `http://127.0.0.1:${port}`, requests };
}

let relay: Listener;
let target: Listener;

beforeEach(async () => {
  target = await listen((_request, respond) => respond(200), []);
  relay = await listen((request, respond) => {
    const status = request.url === "/permanent" ? 308 : 307;
    respond(status, { location: `${target.origin}/collect` });
  }, []);
});

afterEach(async () => {
  await Promise.all(
    [relay, target].map(
      ({ server }) => new Promise<void>((resolve) => server.close(() => resolve())),
    ),
  );
});

function envelope(path: string, redirect?: "follow" | "error"): LlmHttpRequestSpec {
  return {
    url: `${relay.origin}${path}`,
    method: "POST",
    headers: [{ name: "content-type", value: "text/plain;charset=UTF-8" }],
    body: JSON.stringify({ apiKey: KEY, request: { model: "jev-latest" } }),
    ...(redirect ? { redirect } : {}),
  };
}

describe("a credential-bearing request refuses to follow a redirect", () => {
  it.each([
    ["307", "/temporary"],
    ["308", "/permanent"],
  ])("sends nothing to a %s Location target", async (_label, path) => {
    await expect(executeLlmRequest(envelope(path, "error"))).rejects.toBeInstanceOf(
      LlmTransportError,
    );

    expect(relay.requests).toHaveLength(1);
    // Zero requests, so zero key bytes, reached the origin the redirect named.
    expect(target.requests).toEqual([]);
  });

  // The control that makes the test above discriminating: under fetch's
  // default, the same server pair replays the key-bearing body to the target.
  it.each([
    ["307", "/temporary"],
    ["308", "/permanent"],
  ])("would have replayed the body after a %s without the policy", async (_label, path) => {
    await executeLlmRequest(envelope(path, "follow"));

    expect(target.requests).toHaveLength(1);
    expect(target.requests[0].body).toContain(KEY);
  });

  it("treats a spec with no policy as fetch's default", async () => {
    await executeLlmRequest(envelope("/temporary"));

    expect(target.requests).toHaveLength(1);
  });
});
