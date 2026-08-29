/**
 * Operator status message data access.
 *
 * A single, tiny JSON object the maintainer publishes out of band (never by a
 * deploy) so an outage notice, a maintenance window, or a "the lobby is down"
 * heads-up reaches every player without a release. It resolves through the
 * build-time `__STATUS_URL__` define: the R2 prefix on deploy, the site-root
 * path in local dev — matching every other data consumer.
 *
 * Unlike `changelog.ts` there is deliberately NO module-level session cache:
 * the hook re-polls so a message published (or cleared) mid-session is picked
 * up, and a cache would defeat that entirely.
 */

/**
 * Closed set of severities, mirroring `ChangelogTag`'s closed-union discipline
 * so the severity→tone/role lookup stays exhaustive and a stray value from a
 * hand-authored payload is rejected rather than rendered untoned.
 */
export type StatusSeverity = "info" | "warning" | "critical";

export interface StatusLink {
  /** http(s) only — routed through `openExternal`, which re-validates. */
  url: string;
  label: string;
}

export interface StatusMessage {
  /** Epoch ms at publish time. Compared for EQUALITY against the dismissal
   * watermark, so every new publish re-shows even after a dismissal. */
  id: number;
  severity: StatusSeverity;
  title: string;
  /** Plain text — rendered as text, never HTML (same rule as `ChangelogEntry.body`). */
  body: string;
  /** Whether the author allows the player to dismiss this message. */
  dismissible: boolean;
  /** ISO 8601 instant after which the message stops rendering. Absent ⇒ shows
   * until the maintainer clears it. */
  expiresAt?: string;
  /** Optional deep link (Discord post, status page, …). */
  link?: StatusLink;
}

function isSeverity(value: unknown): value is StatusSeverity {
  return value === "info" || value === "warning" || value === "critical";
}

function isStatusLink(value: unknown): boolean {
  if (typeof value !== "object" || value === null) return false;
  const link = value as Record<string, unknown>;
  return typeof link.url === "string" && typeof link.label === "string";
}

/**
 * ONE UNIFORM RULE: any contract violation means the payload is not a
 * `StatusMessage`, so there is nothing to render and `fetchStatus` yields null.
 * No per-field "drop the field and carry on" special cases.
 *
 * Every declared field is checked, not just the ones that drive the tone:
 *  - a non-string `body` is the severe one — React THROWS on an object/array
 *    child, and the nearest boundary is the `<ErrorBoundary>` wrapping the whole
 *    `<Routes>` tree, so one bad field in hand-authored JSON would replace the
 *    entire app with "Something went wrong".
 *  - a string `id` would break the equality-compared dismissal watermark and be
 *    handed to a `(id: number)` setter.
 *  - a string `dismissible` ("false" is truthy) would render a dismiss button on
 *    a message the operator marked undismissible.
 *  - an unparseable `expiresAt` fails CLOSED (reject the message). Fail-open
 *    would instead give a permanent banner clearable only by unpublishing.
 */
function isStatusMessage(value: unknown): value is StatusMessage {
  if (typeof value !== "object" || value === null) return false;
  const msg = value as Record<string, unknown>;
  if (typeof msg.id !== "number") return false;
  if (!isSeverity(msg.severity)) return false;
  if (typeof msg.title !== "string" || msg.title.trim().length === 0) return false;
  if (typeof msg.body !== "string") return false;
  if (typeof msg.dismissible !== "boolean") return false;
  if (msg.link !== undefined && !isStatusLink(msg.link)) return false;
  if (
    msg.expiresAt !== undefined &&
    (typeof msg.expiresAt !== "string" || Number.isNaN(Date.parse(msg.expiresAt)))
  ) {
    return false;
  }
  return true;
}

/**
 * Fetch the published status message. Resolves to null on ANY failure (404 —
 * the normal "nothing published" state — offline, malformed JSON, or a contract
 * violation) and never throws: a status banner is chrome, and no failure of it
 * may surface to the player.
 */
export function fetchStatus(): Promise<StatusMessage | null> {
  return fetch(__STATUS_URL__)
    .then((res) => (res.ok ? (res.json() as Promise<unknown>) : null))
    .then((data) => (isStatusMessage(data) ? data : null))
    .catch(() => null);
}

/**
 * Pure expiry predicate, evaluated against a caller-supplied clock so the
 * polling hook can re-check a still-mounted message on every tick. A message
 * with no `expiresAt` shows until the maintainer clears it. `expiresAt` is
 * guaranteed parseable — {@link isStatusMessage} rejects anything else.
 */
export function isStatusLive(message: StatusMessage, now: number): boolean {
  if (message.expiresAt === undefined) return true;
  return Date.parse(message.expiresAt) > now;
}
