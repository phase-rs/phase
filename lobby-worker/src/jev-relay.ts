// `POST /jev/systemone`: a relay from the browser to TypeSafe's Jev System One
// API, which refuses browser CORS. This is the Worker twin of `phase-server`'s
// `jev_relay` (`crates/phase-server/src/jev_relay.rs`): the official lobby
// hostnames (production and preview) are served by THIS Worker, not by the Rust
// server, so a client whose default relay is the lobby it connects to needs the
// route here too. Keep the two behaviorally identical.
//
// The browser sends a CORS *simple request* — `POST` with
// `Content-Type: text/plain;charset=UTF-8` and no `Authorization` or other
// custom header — so no preflight is issued and a permissive
// `Access-Control-Allow-Origin` is enough for the answer to be readable. The
// body is the envelope `{"apiKey": "<key>", "request": { …System One body… }}`.
// The relay forwards `request` upstream with `Authorization: Bearer <apiKey>`
// and hands back the upstream's status, `Content-Type` and body.
//
// Security invariants:
// - The key comes only from the envelope. No request header is read, so an
//   incoming `Authorization` header is structurally ignored.
// - The key is never logged, stored, echoed in a response, or forwarded as a
//   body field: it exists for one request, only on this handler's stack and in
//   the upstream `Authorization` header. Envelope parse errors are discarded
//   unformatted.
// - The upstream URL comes only from the Worker's own `TYPESAFE_API_URL` var
//   (when set and valid), never from the request.
// - Redirects are not followed, and no upstream header other than
//   `Content-Type` is copied into the response — in particular never
//   `Location`, which would make the browser re-send the key-bearing envelope
//   to an upstream-chosen URL.

import { readBoundedText } from "./directory";

export interface JevRelayEnv {
  /**
   * Per-IP throttle. Optional like the directory limiters: a deployment without
   * the binding still serves, and the gate fails OPEN, since the relay holds no
   * secret of ours and the caller's own key is what pays for each call.
   */
  JEV_LIMIT?: RateLimit;
  /** Overrides the upstream. `https://`, or `http://` on a loopback host (tests). */
  TYPESAFE_API_URL?: string;
}

/** Upstream used when `TYPESAFE_API_URL` is unset or unusable. */
export const JEV_DEFAULT_UPSTREAM = "https://api.typesafe.ai/v1/systemone";

/** Largest envelope the relay accepts; larger bodies are answered 413. */
export const JEV_MAX_BODY_BYTES = 512 * 1024;

/** Total time allowed for one upstream call. */
export const JEV_UPSTREAM_TIMEOUT_MS = 20_000;

/**
 * Upstream calls allowed in flight at once in one isolate; more are answered 503,
 * as `phase-server`'s relay does. A per-isolate bound, so it caps what this
 * route can take from the Worker's concurrency, not the fleet's total.
 */
export const JEV_MAX_IN_FLIGHT = 64;

let inFlight = 0;

const BAD_REQUEST_BODY = "invalid Jev relay request";
const TOO_LARGE_BODY = "Jev relay request body too large";
const BUSY_BODY = "Jev relay busy";
const RATE_LIMITED_BODY = "Jev relay rate limited";
const TIMEOUT_BODY = "Jev upstream timed out";
const UNREACHABLE_BODY = "Jev upstream unreachable";

const CORS_HEADERS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "POST, OPTIONS",
  "Access-Control-Allow-Headers": "Content-Type",
  "Access-Control-Max-Age": "7200",
};

export interface JevRelayOptions {
  /** Test seam; production uses {@link JEV_UPSTREAM_TIMEOUT_MS}. */
  timeoutMs?: number;
}

/** Fixed text only: no part of the request is ever echoed. */
function refuse(status: number, body: string, extra: Record<string, string> = {}): Response {
  return new Response(body, {
    status,
    headers: { ...CORS_HEADERS, "Content-Type": "text/plain; charset=utf-8", ...extra },
  });
}

function isLoopbackHost(hostname: string): boolean {
  return hostname === "localhost" || hostname === "127.0.0.1" || hostname === "[::1]";
}

/** The configured upstream, or the default when the override is absent or unsafe. */
export function resolveJevUpstream(env: JevRelayEnv): string {
  const override = env.TYPESAFE_API_URL?.trim();
  if (!override) return JEV_DEFAULT_UPSTREAM;
  try {
    const url = new URL(override);
    // The key travels in the Authorization header: never in clear text off-box.
    const safe = url.protocol === "https:" || (url.protocol === "http:" && isLoopbackHost(url.hostname));
    return safe && !url.username && !url.password ? url.toString() : JEV_DEFAULT_UPSTREAM;
  } catch {
    return JEV_DEFAULT_UPSTREAM;
  }
}

interface Envelope {
  apiKey: string;
  request: Record<string, unknown>;
}

/** The envelope, or `null` for anything else. Mirrors the Rust `deny_unknown_fields` shape. */
function parseEnvelope(text: string): Envelope | null {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record);
  if (keys.length !== 2 || !keys.includes("apiKey") || !keys.includes("request")) return null;
  const { apiKey, request } = record;
  if (typeof apiKey !== "string" || apiKey.trim() === "") return null;
  if (typeof request !== "object" || request === null || Array.isArray(request)) return null;
  return { apiKey, request: request as Record<string, unknown> };
}

export async function handleJevRelay(
  request: Request,
  env: JevRelayEnv,
  options: JevRelayOptions = {},
): Promise<Response> {
  if (request.method === "OPTIONS") {
    return new Response(null, { status: 204, headers: CORS_HEADERS });
  }
  if (request.method !== "POST") {
    return refuse(405, BAD_REQUEST_BODY, { Allow: "POST, OPTIONS" });
  }

  // Before the body is read, so a flood costs a counter lookup and nothing more.
  if (env.JEV_LIMIT) {
    const ip = request.headers.get("CF-Connecting-IP") ?? "unknown";
    try {
      const { success } = await env.JEV_LIMIT.limit({ key: `jev:${ip}` });
      if (!success) return refuse(429, RATE_LIMITED_BODY);
    } catch {
      // A failing limiter must not take the feature down with it.
      console.error({ event: "jev_rate_limit_failed" });
    }
  }

  const declared = Number(request.headers.get("content-length") ?? "0");
  if (Number.isFinite(declared) && declared > JEV_MAX_BODY_BYTES) {
    return refuse(413, TOO_LARGE_BODY);
  }
  // Counting what was actually read: `Content-Length` is caller-supplied and
  // absent under chunked transfer.
  const text = request.body ? await readBoundedText(request.body, JEV_MAX_BODY_BYTES) : "";
  if (text === null) return refuse(413, TOO_LARGE_BODY);

  const envelope = parseEnvelope(text);
  if (!envelope) return refuse(400, BAD_REQUEST_BODY);

  // A key that is not a legal header value (e.g. a control character) is refused.
  let headers: Headers;
  try {
    headers = new Headers({
      Authorization: `Bearer ${envelope.apiKey}`,
      "Content-Type": "application/json",
    });
  } catch {
    return refuse(400, BAD_REQUEST_BODY);
  }

  if (inFlight >= JEV_MAX_IN_FLIGHT) return refuse(503, BUSY_BODY);
  inFlight += 1;
  // The slot is held until the upstream call is OVER, which for a streamed answer
  // is when its body ends, errors or is cancelled, not when its headers arrive.
  // Released exactly once, on every path.
  let held = true;
  const release = () => {
    if (held) {
      held = false;
      inFlight -= 1;
    }
  };
  const timeoutMs = options.timeoutMs ?? JEV_UPSTREAM_TIMEOUT_MS;
  const started = Date.now();
  let upstream: Response;
  try {
    upstream = await fetch(resolveJevUpstream(env), {
      method: "POST",
      headers,
      body: JSON.stringify(envelope.request),
      // A followed redirect would carry the bearer key to a URL the upstream
      // chose; the 3xx is returned (without `Location`) instead.
      redirect: "manual",
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch (error) {
    release();
    const timedOut = error instanceof Error && (error.name === "TimeoutError" || error.name === "AbortError");
    console.log({ event: "jev_relay", outcome: timedOut ? "timeout" : "unreachable", latencyMs: Date.now() - started });
    return timedOut ? refuse(504, TIMEOUT_BODY) : refuse(502, UNREACHABLE_BODY);
  }

  const body = slotBoundBody(upstream.body, release, Math.max(0, timeoutMs - (Date.now() - started)));
  try {
    // Status and log line only: the key and the upstream body never reach a log.
    console.log({ event: "jev_relay", status: upstream.status, latencyMs: Date.now() - started });

    // Only `Content-Type` is copied from the upstream.
    const response = new Response(body, {
      status: upstream.status,
      headers: { ...CORS_HEADERS, "Cache-Control": "no-store" },
    });
    const contentType = upstream.headers.get("Content-Type");
    if (contentType) response.headers.set("Content-Type", contentType);
    return response;
  } catch (error) {
    // No body reached the runtime, so nothing else will release the slot or stop
    // the wrapper's deadline timer: cancelling does both and closes the upstream.
    release();
    void body?.cancel().catch(() => {});
    throw error;
  }
}

/**
 * `body`, passed through unchanged, that calls `release` exactly once when it
 * ends, errors or is cancelled, or when `deadlineMs` runs out (the answer's total
 * time is bounded, so a stalled stream cannot pin a slot). A body-less answer
 * releases at once.
 */
function slotBoundBody(
  body: ReadableStream<Uint8Array> | null,
  release: () => void,
  deadlineMs: number,
): ReadableStream<Uint8Array> | null {
  if (body === null) {
    release();
    return null;
  }
  const reader = body.getReader();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const finish = () => {
    if (timer !== undefined) clearTimeout(timer);
    release();
  };
  return new ReadableStream<Uint8Array>({
    start(controller) {
      timer = setTimeout(() => {
        finish();
        controller.error(new Error("Jev upstream body timed out"));
        reader.cancel().catch(() => {});
      }, deadlineMs);
    },
    async pull(controller) {
      try {
        const { done, value } = await reader.read();
        if (done) {
          finish();
          controller.close();
        } else {
          controller.enqueue(value);
        }
      } catch (error) {
        finish();
        controller.error(error);
      }
    },
    cancel(reason) {
      finish();
      return reader.cancel(reason);
    },
  });
}
