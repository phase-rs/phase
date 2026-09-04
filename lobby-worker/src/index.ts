import { LobbyDO, type LobbyDoEnv } from "./lobby-do";
import {
  checkIngestGate,
  DIRECTORY_WRITE_CORS,
  MAX_ANNOUNCE_BYTES,
  MAX_METRICS_BYTES,
  type IngestLimiter,
} from "./directory";
import { handleImportDeck, type ImportDeckEnv } from "./import-deck";
import { handleTurnCredentials, type TurnEnv } from "./turn";
import { sanitizeTelemetryBatch, toDataPoint } from "./telemetry";

// The DO class must be exported from the Worker entry so the runtime can
// instantiate it for the binding declared in wrangler.toml.
export { LobbyDO };

// `TELEMETRY?` now lives on `LobbyDoEnv` — the DO reads it too, for the
// server-probe mirror — and is inherited here, so the binding list has one
// home. `handleTelemetry` is unchanged.
interface Env extends TurnEnv, ImportDeckEnv, LobbyDoEnv {
  LOBBY: DurableObjectNamespace;
  // Per-IP rate limiters for the two directory write endpoints. Optional like
  // TELEMETRY: a deploy without the binding still serves, and the gate
  // fails OPEN — the allowlist, not the rate limit, is the admission gate.
  // They stay on `Env` rather than `LobbyDoEnv` because the limiter is called
  // here, before the DO is reached; the DO must never see one.
  ANNOUNCE_LIMIT?: RateLimit;
  METRICS_LIMIT?: RateLimit;
}

/** Reject bodies larger than this outright (via Content-Length or read length).
 *  The client batches ≤ 25 events with capped field strings, so a legitimate
 *  batch is well under this. */
const MAX_TELEMETRY_BYTES = 32 * 1024;

const TELEMETRY_CORS_HEADERS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "POST, OPTIONS",
  "Access-Control-Allow-Headers": "Content-Type",
};

/**
 * Write-only, fire-and-forget telemetry ingest. NEVER returns a 5xx — a
 * telemetry failure must not pollute Workers Metrics or surface to the client,
 * so every path resolves to 204. Handles the `OPTIONS` preflight for the client
 * fetch-fallback path.
 */
async function handleTelemetry(request: Request, env: Env): Promise<Response> {
  const ok = () => new Response(null, { status: 204, headers: TELEMETRY_CORS_HEADERS });
  try {
    if (request.method === "OPTIONS" || request.method !== "POST") return ok();

    const contentLength = Number(request.headers.get("content-length") ?? "0");
    if (Number.isFinite(contentLength) && contentLength > MAX_TELEMETRY_BYTES) return ok();

    const text = await request.text();
    if (text.length > MAX_TELEMETRY_BYTES) return ok();

    // Tolerate `text/plain` (the client sends a bare string to skip the CORS
    // preflight); parse defensively.
    let body: unknown = null;
    try {
      body = JSON.parse(text);
    } catch {
      body = null;
    }

    for (const event of sanitizeTelemetryBatch(body)) {
      env.TELEMETRY?.writeDataPoint(toDataPoint(event));
    }
  } catch {
    // Swallow — ingest is best-effort and must never fail the request.
  }
  return ok();
}

/**
 * Gate a directory write: body cap, then per-IP rate limit, then forward.
 *
 * The limiter binding is injected into the pure gate rather than consulted
 * here, so the ORDER — cap before limiter — is unit-testable without a
 * binding. That order matters: an oversize body must not burn a caller's
 * rate-limit budget.
 *
 * The request is forwarded UNMODIFIED. The cap here reads `Content-Length`
 * only, which is a header the caller supplies, so the DO re-checks the length
 * it actually read.
 */
async function gateDirectoryWrite(
  request: Request,
  maxBytes: number,
  keyPrefix: string,
  limiter?: RateLimit,
): Promise<Response | null> {
  const contentLength = request.headers.get("content-length");
  const ip = request.headers.get("CF-Connecting-IP") ?? "";
  const gate = await checkIngestGate({
    contentLength: contentLength === null ? null : Number(contentLength),
    maxBytes,
    // The purpose prefix keeps the two counters disjoint even if the two
    // namespace ids were ever collapsed.
    key: `${keyPrefix}:${ip}`,
    limiter: limiter as IngestLimiter | undefined,
  });
  if (gate.kind === "accept") return null;
  const status = gate.reason === "too_large" ? 413 : 429;
  return Response.json({ error: gate.reason }, { status, headers: DIRECTORY_WRITE_CORS });
}

export default {
  async fetch(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
    const url = new URL(request.url);

    // Ephemeral TURN credentials endpoint (HTTP, not the WS lobby).
    if (url.pathname === "/turn-credentials") {
      return handleTurnCredentials(request, env);
    }

    // Deck import service — fetches Moxfield/Archidekt server-side and returns
    // canonical decklist text. CORS-free for browser clients; CF-cached so a
    // hot deck costs one upstream call.
    if (url.pathname === "/import-deck") {
      return handleImportDeck(request, env, ctx);
    }

    // Client telemetry ingest (Analytics Engine). Routed BEFORE the DO
    // catch-all so it never touches the lobby DO. Write-only + fire-and-forget.
    if (url.pathname === "/telemetry") {
      return handleTelemetry(request, env);
    }

    // Server-directory writes. Gated HERE — the rate-limit bindings are
    // Worker-scoped and the DO must never see one — then forwarded to the DO,
    // which owns the storage and every verdict. `GET /servers` needs no route:
    // the DO catch-all below already forwards it, and a route that only
    // forwarded would be a no-op layer.
    if (url.pathname === "/servers/announce" || url.pathname === "/servers/metrics") {
      if (request.method === "OPTIONS") {
        return new Response(null, { status: 204, headers: DIRECTORY_WRITE_CORS });
      }
      const isAnnounce = url.pathname === "/servers/announce";
      const refusal = await gateDirectoryWrite(
        request,
        isAnnounce ? MAX_ANNOUNCE_BYTES : MAX_METRICS_BYTES,
        isAnnounce ? "announce" : "metrics",
        isAnnounce ? env.ANNOUNCE_LIMIT : env.METRICS_LIMIT,
      );
      // Metrics ingest is fire-and-forget like /telemetry: a refusal is still
      // a 204, so a rate-limited client never sees an error it cannot act on.
      // An announce refusal IS reported — an announcer must know it was not
      // listed.
      if (refusal) {
        return isAnnounce
          ? refusal
          : new Response(null, { status: 204, headers: DIRECTORY_WRITE_CORS });
      }
      // Awaited, not `ctx.waitUntil`: the DO write must not be dropped.
      const lobby = env.LOBBY.idFromName("global");
      return env.LOBBY.get(lobby).fetch(request);
    }

    // Single global lobby: every other request routes to the one DO instance
    // named "global". (Cloudflare multi-homes a single DO at the edge; there is
    // no second instance to fragment the pool — see plan §4/§5.)
    const id = env.LOBBY.idFromName("global");
    return env.LOBBY.get(id).fetch(request);
  },
};
