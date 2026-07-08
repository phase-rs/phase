// Telemetry ingest functional core for the lobby Worker.
//
// Same shape as `stats.ts`: pure functions with no I/O, unit-testable under
// `node --test`. The Worker shell (`index.ts`) owns the request read, the
// `env.TELEMETRY.writeDataPoint` calls, and `Response` construction; this
// module owns the allow-listed event schema, envelope validation, and the
// stable Analytics Engine column layout.
//
// The design mirrors how the lobby DO gates inbound frames: a per-event field
// allow-list. Unknown events and unknown fields are dropped silently (never an
// error) so a newer client can add events without a Worker deploy, and a
// malformed body degrades to an empty batch rather than a 5xx.

/** Max characters kept for any event-specific field string. Matches the client
 *  enqueue cap; enforced again here because ingest cannot trust the client. */
const MAX_FIELD_STR = 300;
/** Max characters kept for a list field flattened to a comma-joined blob (e.g.
 *  `unimplemented_oracle_ids`). Larger than {@link MAX_FIELD_STR} so the client's
 *  20-id cap survives the join (20 UUID-ish ids ≈ 800 chars); still far under
 *  AE's 16 KB per-point blob budget. */
const MAX_LIST_BLOB = 1000;
/** Max characters kept for an envelope string (version / hash / platform). */
const MAX_ENVELOPE_STR = 64;
/** Max events accepted from a single batch — the rest are dropped. */
export const MAX_EVENTS_PER_BATCH = 25;

/**
 * Per-event allow-list. `blobs` are the event-specific string columns (in
 * order); `doubles` are the numeric/boolean columns (in order). Both the Tier 1
 * crash/usage events and the Tier 2 report/usage events are accepted. Array
 * fields (e.g. `unimplemented_oracle_ids`) are flattened to a single
 * comma-joined blob.
 *
 * Column layout for every point (documented so the AE schema is stable):
 *   indexes: [event]
 *   blobs:   [event, app_version, build_hash, platform, ...schema.blobs]
 *   doubles: [...schema.doubles]
 * AE hard limits respected by construction: ≤ 20 blobs, ≤ 20 doubles, exactly
 * 1 index, ≤ 16 KB total blob bytes (guarded by the per-field string caps).
 *
 * Note on `turn`/`turn_count`: `turn_number` is 1-based, and `toDouble` coerces
 * a missing/null turn to 0 — so a `turn: 0` (or `turn_count: 0`) column means
 * "unknown", never turn zero.
 */
export const EVENT_SCHEMAS: Record<string, { blobs: string[]; doubles: string[] }> = {
  engine_panic: { blobs: ["reason", "panic", "game_mode"], doubles: ["fatal", "turn"] },
  stuck_decision: { blobs: ["waiting_for_kind", "game_mode", "phase"], doubles: [] },
  js_error: { blobs: ["name", "message", "top_frame", "source", "route"], doubles: [] },
  chunk_reload: { blobs: ["reason", "chunk"], doubles: ["deferred"] },
  game_end: {
    blobs: [
      "result",
      "winner_kind",
      "game_mode",
      "unimplemented_oracle_ids",
      "pending_trigger_abandons",
    ],
    doubles: ["turn_count"],
  },
  card_report: {
    blobs: ["oracle_id", "face_name", "name", "zone", "game_mode"],
    doubles: ["turn", "supported", "total"],
  },
  session_start: { blobs: ["route"], doubles: [] },
  game_start: { blobs: ["game_mode"], doubles: ["player_count", "ai_count"] },
  route_view: { blobs: ["route"], doubles: [] },
};

/** A validated, column-resolved event ready to become an AE data point. */
export interface SanitizedEvent {
  event: string;
  appVersion: string;
  buildHash: string;
  platform: string;
  /** Event-specific string columns, in `EVENT_SCHEMAS[event].blobs` order. */
  blobs: string[];
  /** Event-specific numeric columns, in `EVENT_SCHEMAS[event].doubles` order. */
  doubles: number[];
}

function truncate(value: string, max: number): string {
  return value.length > max ? value.slice(0, max) : value;
}

/** Coerce an arbitrary field value into a bounded blob string. Arrays are
 *  comma-joined; missing/other values become "". */
function toBlob(value: unknown): string {
  if (typeof value === "string") return truncate(value, MAX_FIELD_STR);
  if (Array.isArray(value)) {
    return truncate(value.filter((v) => typeof v === "string").join(","), MAX_LIST_BLOB);
  }
  return "";
}

/** Coerce an arbitrary field value into a double. Booleans → 1/0; numbers pass
 *  through (non-finite → 0); everything else → 0. */
function toDouble(value: unknown): number {
  if (typeof value === "boolean") return value ? 1 : 0;
  if (typeof value === "number" && Number.isFinite(value)) return value;
  return 0;
}

/** Read a string envelope field with the 64-char cap; "" when absent. */
function envelopeString(source: Record<string, unknown>, key: string): string {
  const value = source[key];
  return typeof value === "string" ? truncate(value, MAX_ENVELOPE_STR) : "";
}

/**
 * Validate and sanitize a raw request body into the accepted events. Returns
 * `[]` for any malformed input (not an object, wrong schema version, missing
 * events array). Drops unknown events, unknown fields, and events beyond
 * {@link MAX_EVENTS_PER_BATCH}.
 */
export function sanitizeTelemetryBatch(body: unknown): SanitizedEvent[] {
  if (!body || typeof body !== "object") return [];
  const batch = body as Record<string, unknown>;
  if (batch.schema !== 1) return [];
  if (!Array.isArray(batch.events)) return [];

  const appVersion = envelopeString(batch, "app_version");
  const buildHash = envelopeString(batch, "build_hash");
  const platform = envelopeString(batch, "platform");

  const out: SanitizedEvent[] = [];
  for (const raw of batch.events.slice(0, MAX_EVENTS_PER_BATCH)) {
    if (!raw || typeof raw !== "object") continue;
    const event = (raw as Record<string, unknown>).event;
    if (typeof event !== "string") continue;
    const schema = EVENT_SCHEMAS[event];
    if (!schema) continue; // unknown event — dropped, not an error

    const fields = raw as Record<string, unknown>;
    out.push({
      event,
      appVersion,
      buildHash,
      platform,
      blobs: schema.blobs.map((key) => toBlob(fields[key])),
      doubles: schema.doubles.map((key) => toDouble(fields[key])),
    });
  }
  return out;
}

/** Analytics Engine data point shape (mirrors `AnalyticsEngineDataPoint`). */
export interface TelemetryDataPoint {
  indexes: string[];
  blobs: string[];
  doubles: number[];
}

/** Project a sanitized event onto the stable AE column layout. */
export function toDataPoint(e: SanitizedEvent): TelemetryDataPoint {
  return {
    indexes: [e.event],
    blobs: [e.event, e.appVersion, e.buildHash, e.platform, ...e.blobs],
    doubles: e.doubles,
  };
}
