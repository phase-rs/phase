/**
 * Changelog ("What's New") data access.
 *
 * Two payloads, two cadences:
 *  - changelog-meta.json — tiny `{ latestId }`, fetched on every app load to
 *    drive the unread dot. Constant size forever, so the hot path never grows.
 *  - changelog.json — the full entry list, fetched lazily only when the user
 *    opens the modal.
 *
 * Both resolve through build-time defines (`__CHANGELOG_*_URL__`): the R2 prefix
 * on deploy, site-root paths in local dev — matching every other data consumer.
 */

/**
 * Closed set of entry tags, aligned 1:1 with the emoji sections the `changelog`
 * skill emits. A closed union (not an open string) keeps the tag→label/color
 * lookup exhaustive and lets TypeScript catch a stray tag at the call site.
 */
export type ChangelogTag =
  | "new-cards"
  | "card-fixes"
  | "gameplay"
  | "interface"
  | "localization"
  | "ai"
  | "multiplayer";

export interface ChangelogEntry {
  /** Stable, append-only integer id. Newest entry has the highest id. */
  id: number;
  /** ISO date (YYYY-MM-DD) of the release batch. */
  date: string;
  title: string;
  tags: ChangelogTag[];
  /** Plain text, emoji-sectioned, newline-separated. Rendered as text — never HTML. */
  body: string;
  /** Optional deep link to the matching Discord #announcements post. */
  discordUrl?: string;
}

export interface ChangelogMeta {
  /** Highest entry id currently published. Drives the unread comparison. */
  latestId: number;
}

let metaCache: ChangelogMeta | null = null;
let metaPromise: Promise<ChangelogMeta | null> | null = null;
let entriesCache: ChangelogEntry[] | null = null;

/**
 * Fetch the tiny meta pointer. Resolves to null on any failure (offline, 404)
 * — a missing pointer simply means "no unread dot", never an error surfaced to
 * the user. Cached for the session.
 */
export function fetchChangelogMeta(): Promise<ChangelogMeta | null> {
  if (metaCache) return Promise.resolve(metaCache);
  if (!metaPromise) {
    metaPromise = fetch(__CHANGELOG_META_URL__)
      .then((res) => (res.ok ? (res.json() as Promise<ChangelogMeta>) : null))
      .then((data) => {
        if (typeof data?.latestId === "number") metaCache = data;
        return metaCache;
      })
      .catch(() => null);
  }
  return metaPromise;
}

/**
 * Fetch the full entry list (lazy — modal open only). Throws on a failed fetch
 * so the caller can keep the unread watermark intact: we only mark entries seen
 * once the user has actually loaded them. Cached for the session on success.
 */
export async function fetchChangelogEntries(): Promise<ChangelogEntry[]> {
  if (entriesCache) return entriesCache;
  const res = await fetch(__CHANGELOG_URL__);
  if (!res.ok) {
    throw new Error(`Failed to load changelog: ${res.status}`);
  }
  const { entries } = (await res.json()) as { entries: ChangelogEntry[] };
  entriesCache = entries;
  return entries;
}

/**
 * Pure unread predicate. There is "something new" only when we have a published
 * latestId AND a prior watermark to compare against AND the latest is strictly
 * newer. A null watermark means a first-run / freshly-upgraded user whose
 * watermark the hook silently seeds to latestId — they get no dot for entries
 * that predate their first visit.
 */
export function computeHasUnread(
  latestId: number | null | undefined,
  lastSeenId: number | null | undefined,
): boolean {
  if (latestId == null) return false;
  if (lastSeenId == null) return false;
  return latestId > lastSeenId;
}
