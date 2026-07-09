// Usage-analytics functional core for the lobby Durable Object.
//
// Everything here is storage-key definitions plus pure folds / derivations over
// data the DO shell reads from `ctx.storage` — no I/O, so it is unit-testable
// under `node --test` the same way `hello-gate.ts` is. The DO owns all
// `ctx.storage` reads/writes and `Response` construction; this module owns the
// stored shapes and the math that turns them into the `/stats` payload.

/** Storage keys for the durable, monotonic analytics facts. Live gauges
 *  (players online, active games) are never stored — they are computed on read
 *  by the DO from the socket set + broker — so only these persist. */
export const GAMES_CREATED_KEY = "stats:games_created"; // total GameCreated (rooms hosted)
export const GAMES_JOINED_KEY = "stats:games_joined"; // total PeerInfo (P2P matches begun)
export const PLAYERS_PEAK_KEY = "stats:players_peak"; // high-water concurrent connections
export const DAILY_PREFIX = "stats:daily:"; // per-UTC-day buckets, key `${prefix}YYYY-MM-DD`

/** Newest-N daily buckets returned in the `/stats` series. */
export const DAILY_SERIES_LIMIT = 30;

/** Per-day rollup stored at `${DAILY_PREFIX}${date}`. */
export interface DailyStat {
  created: number;
  joined: number;
}

/** One chronological day in the `/stats` series (a `DailyStat` tagged with its
 *  UTC date). */
export interface DailyStatPoint extends DailyStat {
  date: string;
}

/** The `/stats` response body: live gauges + durable totals / peak / series. */
export interface StatsPayload {
  players_online: number;
  players_peak: number;
  active_games: number;
  games_created_total: number;
  games_joined_total: number;
  games_created_today: number;
  games_joined_today: number;
  daily: DailyStatPoint[];
}

/** The subset of a broker outbound side effect that the analytics fold reads.
 *  Structurally compatible with the DO's `OutboundDto`, kept local so this
 *  module has no dependency on the DO shell. */
interface GameOutbound {
  kind: string;
  msg?: unknown;
}

/** Fold the broker's outbounds into game counts: `GameCreated` = a room was
 *  hosted; `PeerInfo` = a guest was handed the host's peer id, i.e. a P2P match
 *  actually began. Only `ToSelf` outbounds carry these; everything else (lobby
 *  broadcasts, subscriber registry ops) is ignored. */
export function countGameOutbounds(
  outbounds: readonly GameOutbound[],
): { created: number; joined: number } {
  let created = 0;
  let joined = 0;
  for (const o of outbounds) {
    if (o.kind !== "ToSelf") continue;
    const type = (o.msg as { type?: string } | undefined)?.type;
    if (type === "GameCreated") created += 1;
    else if (type === "PeerInfo") joined += 1;
  }
  return { created, joined };
}

/** UTC date bucket (YYYY-MM-DD) for `nowMs` — the daily-series key suffix. */
export function dayBucket(nowMs: number): string {
  return new Date(nowMs).toISOString().slice(0, 10);
}

/** Inputs to {@link buildStatsPayload}: the durable scalars, the live gauges,
 *  the raw (newest-first, prefix-keyed) daily buckets, and an explicit clock. */
export interface BuildStatsPayloadArgs {
  playersOnline: number;
  playersPeak: number;
  activeGames: number;
  gamesCreatedTotal: number;
  gamesJoinedTotal: number;
  /** Reverse-listed (newest-first) daily buckets keyed by full storage key
   *  (`${DAILY_PREFIX}YYYY-MM-DD`), as returned by `ctx.storage.list`. */
  daily: Map<string, DailyStat>;
  /** Explicit `Date.now()` so the today-lookup is deterministic under test. */
  nowMs: number;
}

/** Assemble the `/stats` body: strip the key prefix off the newest-first daily
 *  map, cap at {@link DAILY_SERIES_LIMIT}, flip to chronological for charting,
 *  and derive today's created/joined from the series via `nowMs`. */
export function buildStatsPayload(args: BuildStatsPayloadArgs): StatsPayload {
  const series: DailyStatPoint[] = [...args.daily.entries()]
    .slice(0, DAILY_SERIES_LIMIT)
    .map(([key, value]) => ({
      date: key.slice(DAILY_PREFIX.length),
      created: value.created,
      joined: value.joined,
    }))
    .reverse();
  const today = dayBucket(args.nowMs);
  const todayStat = series.find((s) => s.date === today) ?? { created: 0, joined: 0 };
  return {
    players_online: args.playersOnline,
    players_peak: args.playersPeak,
    active_games: args.activeGames,
    games_created_total: args.gamesCreatedTotal,
    games_joined_total: args.gamesJoinedTotal,
    games_created_today: todayStat.created,
    games_joined_today: todayStat.joined,
    daily: series,
  };
}
