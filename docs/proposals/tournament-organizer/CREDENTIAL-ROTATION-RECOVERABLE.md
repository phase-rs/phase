# Recoverable tournament credential rotation — bounded overlap (server-first)

**Status:** design accepted; implementation pending. Follow-up to the deferred rotation half of
#8782. **Decision:** **bounded old-token overlap** (Option A below), chosen over idempotent-replay.
**As-of:** `upstream/main` after #8782, `LOBBY_PROTOCOL_VERSION = 8` (bump → 9).

## Problem (the #8782 [HIGH])

`renew_credential` (`crates/lobby-broker/src/tournament.rs`) is destructive and non-idempotent: it
mints a new secret, verifies the presented (old) secret is `Accepted`, then **replaces** the stored
credential and returns **only** the new secret (single `ToSelf` reply in `broker.rs`). A
timeout/abort/connection-loss **after the commit but before delivery** leaves the client holding a
secret the broker just invalidated — and an expired/rotated credential is **unrenewable**, so the
authority is **permanently stranded**. Not fixable in the client alone: reusing the old secret on an
uncertain result races the commit.

Model today: `TournamentCredential { secret: String, expires_at_ms: u64 }`;
`verdict(presented, now) → Accepted | Expired | Mismatch`; `mint()` → random secret + `now + TTL`
(TTL = 7 days). Organizer holds one token; each player holds one.

## Decision: bounded old-token overlap

On rotation, keep the **previous** secret valid for a short grace window instead of invalidating it
instantly. This makes the exact client behavior #8782's review flagged as unsafe — falling back to
the old token on an uncertain renewal result — *correct*: during overlap the old secret still
authorizes, and the client's next proactive near-expiry renew recovers a confirmed new secret.

**Chosen over idempotent-replay** because it is localized server-side, needs **no wire-request
change**, and lets the #8782 client half re-land nearly unmodified — no nonce lifecycle, no
client-side pending-rotation persistence, no broker replay store/eviction. Replay's only edge is the
idle-and-offline-past-overlap case; a generous, tunable overlap window makes that negligible for a
live event, and replay can be revisited if that ever proves insufficient.

## Server changes (`crates/lobby-broker`)

1. **Credential model** (`tournament.rs`):
   ```
   TournamentCredential {
     secret: String,
     expires_at_ms: u64,
     previous: Option<PreviousSecret>,   // NEW
   }
   PreviousSecret { secret: String, valid_until_ms: u64 }
   ```
2. **`verdict(presented, now)`** accepts the current secret (until `expires_at_ms`) **or**
   `previous.secret` (until `previous.valid_until_ms`); else `Expired`/`Mismatch` as today. Keep the
   constant-time compare; check current first, then overlap. `Expired` only when neither is live.
3. **`renew_credential`**: verify presented is `Accepted` (current-or-overlap), then
   `previous = { old_secret, now + TOURNAMENT_CREDENTIAL_OVERLAP_MS }`, `current = minted`. Reply is
   the minted secret + expiry, unchanged.
4. **`TOURNAMENT_CREDENTIAL_OVERLAP_MS`** — new const, ~5–15 min. Long enough to cover a client's
   next action/retry after a lost reply; short enough to bound dual-validity. Sized against the
   7-day TTL, this is a small window.
5. **`LOBBY_PROTOCOL_VERSION` 8 → 9.** The wire *shape* is unchanged (no message/field change), but
   rotation *semantics* change (old secret now briefly honored), so peers should negotiate the new
   behavior explicitly rather than infer it.

## Mirror + gates

- `server-core` (`ClientMessage` / `client_message_wire_guard`) and `phase-server`
  (`to_lobby_client_message`) move in lockstep with the broker **only if the wire shape changes** —
  it does **not** here (overlap is server-internal). So the mirror likely needs no variant change;
  confirm the roundtrip test still holds.
- `scripts/check-protocol-version.mjs`: update `EXPECTED_LOBBY_PROTOCOL_VERSION` to 9 (+ the client
  `LOBBY_PROTOCOL_VERSION` export). No new floor constant needed.

## Client (re-land from `pr8782-fullrotation`)

- Restore the sender `renewTournamentCredentialOver` + proactive `maybeRenewNearExpiry` +
  `shouldRenewCredential`, driven off the stored `expires_at_ms`, plus the `TournamentCreatedReply`/
  `TournamentJoinedReply`/`TournamentCredentialRenewed` `expires_at_ms` and the per-token expiry on
  the (now sessionStorage-backed) credential model.
- The "return the held token on an uncertain result" fallback is now **safe** under overlap — keep
  it, and let the next near-expiry check recover a confirmed secret.
- Credentials already live in sessionStorage (#8782); no additional client persistence needed.

## Tests

- **Server (unit):** overlap window — old secret `Accepted` until `valid_until_ms`, `Expired` after;
  rotation sets `previous`; a second rotation within overlap still works; the **lost-reply recovery**
  path (rotate, "drop" the reply, old secret still authorizes the next action, re-rotate recovers).
- **Client:** uncertain-result path keeps the held token and the next near-expiry renew succeeds.
- The regression #8782's review explicitly asked for: drive a server rotation, lose the reply, prove
  the next gated action / recovery retains the authority — at both layers.

## Logistics

- **Not frontend-only.** Rust across `lobby-broker` (+ mirror check) → **claim the cargo-lock row in
  WORKLIST.md before any compiling cargo command**, release after. Engine clippy is slow; run
  detached and poll. Tilt is not running locally — verify via direct cargo.
- Preserved client code + original client design: `pr8782-fullrotation` tag / #8782 history.
- Ships as its own protocol-versioned PR (server + client together, since the semantics pair).
