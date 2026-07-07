export interface RateLimitConfig {
  maxRequests: number;
  windowMs: number;
}

interface Bucket {
  count: number;
  resetAt: number;
}

/** In-memory per-key sliding window limiter (per isolate). */
export function createRateLimiter(config: RateLimitConfig) {
  const buckets = new Map<string, Bucket>();

  return {
    check(
      key: string,
      now = Date.now(),
      maxRequests = config.maxRequests,
    ): { allowed: true } | { allowed: false; retryAfterSeconds: number } {
      const entry = buckets.get(key);
      if (!entry || now >= entry.resetAt) {
        buckets.set(key, { count: 1, resetAt: now + config.windowMs });
        return { allowed: true };
      }
      if (entry.count >= maxRequests) {
        return {
          allowed: false,
          retryAfterSeconds: Math.max(1, Math.ceil((entry.resetAt - now) / 1000)),
        };
      }
      entry.count += 1;
      return { allowed: true };
    },

    reset(): void {
      buckets.clear();
    },
  };
}

export function parseTurnRateLimitPerHour(raw: string | undefined): number {
  const parsed = Number(raw ?? "30");
  if (!Number.isFinite(parsed)) return 30;
  return Math.min(120, Math.max(1, Math.floor(parsed)));
}
