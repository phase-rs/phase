// Mints short-lived Cloudflare Realtime TURN credentials on demand, so the
// client never ships static TURN credentials in its bundle (the prior Metered
// setup hardcoded them — anyone could extract and burn the quota).
//
// The client GETs /turn-credentials; this handler POSTs to Cloudflare's TURN
// API with the secret token and forwards the resulting ICE servers.
// Docs: https://developers.cloudflare.com/realtime/turn/generate-credentials/

import {
  createRateLimiter,
  parseTurnRateLimitPerHour,
} from "./turn-rate-limit";

const TURN_RATE_WINDOW_MS = 60 * 60 * 1000;
const turnRateLimiter = createRateLimiter({
  maxRequests: 30,
  windowMs: TURN_RATE_WINDOW_MS,
});

export interface TurnEnv {
  /** Cloudflare Realtime TURN key ID (var, not secret). */
  TURN_KEY_ID?: string;
  /** Cloudflare Realtime TURN API token (secret: `wrangler secret put`). */
  TURN_KEY_API_TOKEN?: string;
  /** Credential lifetime in seconds (max 48h). Default 24h. */
  TURN_TTL_SECONDS?: string;
  /** Comma-separated origin allowlist, or "*" (default) to allow any. */
  ALLOWED_ORIGINS?: string;
  /** Max credential mints per client IP per hour (default 30, max 120). */
  TURN_RATE_LIMIT_PER_HOUR?: string;
}

function corsHeaders(request: Request, env: TurnEnv): Record<string, string> {
  const allow = (env.ALLOWED_ORIGINS ?? "*").trim();
  let allowOrigin = "*";
  if (allow !== "*") {
    const origin = request.headers.get("Origin") ?? "";
    const list = allow.split(",").map((s) => s.trim()).filter(Boolean);
    allowOrigin = list.includes(origin) ? origin : (list[0] ?? "");
  }
  return {
    "Access-Control-Allow-Origin": allowOrigin,
    "Access-Control-Allow-Methods": "GET, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type",
    Vary: "Origin",
  };
}

/**
 * Client network context for relay attribution + diagnostics, read from
 * `request.cf` (Cloudflare edge metadata). `customIdentifier` is sent to the
 * TURN API so relay egress/ingress bytes become queryable per client network in
 * the GraphQL analytics — surfacing which carriers (notably CGNAT / symmetric-
 * NAT mobile networks) actually drive relay demand, and flagging a single
 * network burning the quota. The same fields are logged on every mint so the
 * Worker Logs view tells you *who* requested relay even before bytes flow.
 */
function clientContext(request: Request): {
  colo: string;
  country: string;
  asn: number;
  customIdentifier: string;
} {
  const cf = request.cf as IncomingRequestCfProperties | undefined;
  const colo = cf?.colo ?? "unknown";
  const country = cf?.country ?? "XX";
  const asn = cf?.asn ?? 0;
  return { colo, country, asn, customIdentifier: `${country}-AS${asn}` };
}

function clientIp(request: Request): string {
  // Rate-limit keys must come from Cloudflare edge metadata only; X-Forwarded-For
  // is client-controlled and would let callers rotate spoofed IPs to bypass limits.
  return request.headers.get("CF-Connecting-IP") ?? "unknown";
}

/**
 * When `ALLOWED_ORIGINS` is tightened, reject browser requests whose `Origin`
 * header is present but not on the allowlist. Missing `Origin` is allowed so
 * Tauri/desktop fetches (no Origin) keep working.
 */
function rejectDisallowedOrigin(
  request: Request,
  env: TurnEnv,
  cors: Record<string, string>,
): Response | null {
  const allow = (env.ALLOWED_ORIGINS ?? "*").trim();
  if (allow === "*") return null;
  const origin = request.headers.get("Origin");
  if (!origin) return null;
  const list = allow.split(",").map((s) => s.trim()).filter(Boolean);
  if (list.includes(origin)) return null;
  return Response.json(
    { error: "Origin not allowed" },
    { status: 403, headers: cors },
  );
}

function rejectRateLimited(
  request: Request,
  env: TurnEnv,
  cors: Record<string, string>,
): Response | null {
  const maxRequests = parseTurnRateLimitPerHour(env.TURN_RATE_LIMIT_PER_HOUR);
  const result = turnRateLimiter.check(clientIp(request), Date.now(), maxRequests);
  if (result.allowed) return null;
  return Response.json(
    { error: "TURN credential rate limit exceeded" },
    {
      status: 429,
      headers: {
        ...cors,
        "Retry-After": String(result.retryAfterSeconds),
      },
    },
  );
}

export async function handleTurnCredentials(
  request: Request,
  env: TurnEnv,
): Promise<Response> {
  const cors = corsHeaders(request, env);

  if (request.method === "OPTIONS") {
    return new Response(null, { status: 204, headers: cors });
  }

  const originBlock = rejectDisallowedOrigin(request, env, cors);
  if (originBlock) return originBlock;

  const rateBlock = rejectRateLimited(request, env, cors);
  if (rateBlock) return rateBlock;

  if (!env.TURN_KEY_ID || !env.TURN_KEY_API_TOKEN) {
    // Not configured yet — the client falls back to STUN-only (direct/STUN
    // connections still work; symmetric-NAT peers won't relay until this is set).
    // Hitting this in production means relay is silently off for the whole app.
    console.error({ event: "turn_unconfigured" });
    return Response.json(
      {
        error:
          "TURN not configured: set TURN_KEY_ID (var) and TURN_KEY_API_TOKEN (secret).",
      },
      { status: 503, headers: cors },
    );
  }

  const ttl = Number(env.TURN_TTL_SECONDS ?? "86400");
  const ctx = clientContext(request);
  const cfRes = await fetch(
    `https://rtc.live.cloudflare.com/v1/turn/keys/${env.TURN_KEY_ID}/credentials/generate-ice-servers`,
    {
      method: "POST",
      headers: {
        Authorization: `Bearer ${env.TURN_KEY_API_TOKEN}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify({ ttl, customIdentifier: ctx.customIdentifier }),
    },
  );

  if (!cfRes.ok) {
    // The failure mode that drops a symmetric-NAT peer to STUN-only and breaks
    // them. Logged as an error so it stands out in Workers Logs (the Metrics
    // view counts this handled 502 as a "Success" invocation).
    console.error({ event: "turn_mint_failed", upstreamStatus: cfRes.status, ...ctx });
    return Response.json(
      { error: `TURN credential generation failed (${cfRes.status})` },
      { status: 502, headers: cors },
    );
  }

  console.log({ event: "turn_mint_ok", ...ctx });

  // CF returns `{ iceServers: [ {stun}, {turn, username, credential} ] }` —
  // already an RTCIceServer[] the client drops straight into RTCConfiguration.
  const data = (await cfRes.json()) as { iceServers: unknown };
  return Response.json({ iceServers: data.iceServers }, { status: 200, headers: cors });
}
