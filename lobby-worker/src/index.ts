import { LobbyDO } from "./lobby-do";
import { handleImportDeck, type ImportDeckEnv } from "./import-deck";
import { handleTurnCredentials, type TurnEnv } from "./turn";
import { sanitizeTelemetryBatch, toDataPoint } from "./telemetry";

// The DO class must be exported from the Worker entry so the runtime can
// instantiate it for the binding declared in wrangler.toml.
export { LobbyDO };

interface Env extends TurnEnv, ImportDeckEnv {
  LOBBY: DurableObjectNamespace;
  // Analytics Engine binding for client telemetry. Optional so deploys without
  // the binding keep working — ingest just drops the writes.
  TELEMETRY?: AnalyticsEngineDataset;
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

    // Single global lobby: every other request routes to the one DO instance
    // named "global". (Cloudflare multi-homes a single DO at the edge; there is
    // no second instance to fragment the pool — see plan §4/§5.)
    const id = env.LOBBY.idFromName("global");
    return env.LOBBY.get(id).fetch(request);
  },
};
