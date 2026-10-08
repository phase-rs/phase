# Recoverable tournament credential rotation — idempotent replay (server-first)

**Status:** implemented, PR phase-rs/phase#8827. **Decision:** **idempotent replay via a
client-minted nonce**. An earlier draft chose bounded old-token overlap; maintainer review found that
mechanism permits a **capability-escalation takeover** (below), so it was replaced with replay.
**As-of:** `upstream/main` after #8782, `LOBBY_PROTOCOL_VERSION` 8 → 9.

## Problem (the #8782 [HIGH])

`renew_credential` (`crates/lobby-broker/src/tournament.rs`) was destructive and non-idempotent: it
minted a new secret, verified the presented (old) secret is `Accepted`, then **replaced** the stored
credential and returned **only** the new secret (single `ToSelf` reply in `broker.rs`). A
timeout/abort/connection-loss **after the commit but before delivery** left the client holding a
secret the broker just invalidated — and an expired/rotated credential is **unrenewable**, so the
authority was **permanently stranded**. Not fixable in the client alone: reusing the old secret on an
uncertain result races the commit.

Model before: `TournamentCredential { secret, expires_at_ms }`;
`verdict(presented, now) → Accepted | Expired | Mismatch`; `mint()` → random secret + `now + TTL`
(TTL = 7 days). Organizer holds one token; each player holds one.

## Why not bounded overlap (the rejected first cut)

The first implementation kept the superseded secret valid for a grace window so the client could
fall back to it on an uncertain result. Maintainer review (a **[HIGH]**) showed this is a
takeover: an accepted overlap secret can *mint and receive* a fresh primary. After the owner rotates
A→B, a holder of the still-valid old A can renew A→C, **invalidating B and obtaining C** — bounded
recovery becomes primary-authority takeover. Making the old token non-refreshable does not fix it;
the *first* A→C rotation inside the window already escalates. The lesson: recovery must never let a
superseded secret produce a primary.

## Decision: idempotent replay via a client nonce

Recovery comes from replaying the *already-committed* rotation, not from a superseded secret minting
a new one.

- `RenewTournamentCredential` gains a client-minted `rotation_nonce`.
- A rotation from the **current** secret mints a new secret and records
  `RotationRecord { superseded_secret, nonce }` (the one, most-recent rotation).
- Presenting the **superseded** secret **with the recorded nonce** REPLAYS the already-committed
  current secret — no second mint, no advance. Presenting a superseded secret with a wrong/absent
  nonce (a thief without the initiator's nonce) is `Mismatch`: it can neither mint nor obtain a
  credential. Only the current secret can mint.

So a lost reply is recovered by the client retrying with the same `(token, nonce)`; a stolen
superseded secret cannot take over, because it lacks the nonce and cannot mint. `verdict` reverts to
**current-secret-only** — a superseded secret never authorizes an action, only ever replays.

## Server changes (`crates/lobby-broker`)

1. **Credential model** (`tournament.rs`): `TournamentCredential { secret, expires_at_ms,
   last_rotation: Option<RotationRecord> }`; `RotationRecord { superseded_secret, nonce }`.
2. **`renew(presented, nonce, env) → RenewOutcome`** (`Minted | Replayed | Expired | Mismatch`),
   dispatched by a read-only `renew_kind` probe so the player-scan can attribute a replay (whose
   presented secret `verdict` alone would call a mismatch) to its owning entrant.
3. **`renew_credential`** takes `nonce`, routes organizer/player through `renew`/`renew_kind`.
4. **`LOBBY_PROTOCOL_VERSION` 8 → 9** — a field is added (`rotation_nonce`, `#[serde(default)]`), the
   standard "a lobby field is added" trigger. Purely additive: `MIN_SUPPORTED_LOBBY_PROTOCOL` stays
   at 2 (only v9+ clients send the frame; a pre-9 broker ignores the field; an omitted nonce
   deserializes empty → can only mint, never replay). `PROTOCOL_VERSION` does not move.
5. `rotation_nonce` is bounds-checked like `token` (a client string the broker stores); empty is
   allowed (mint-only).

## Mirror + gates

- `server-core` (`ClientMessage` + `client_message_wire_guard`) and `phase-server`
  (`to_lobby_client_message`) gain the `rotation_nonce` field in lockstep; the shared
  `validate_renew_tournament_credential_fields` bounds it.
- `scripts/check-protocol-version.mjs`: `EXPECTED_LOBBY_PROTOCOL_VERSION` → 9, client
  `LOBBY_PROTOCOL_VERSION` → 9, and `MIN_LOBBY_PROTOCOL_FOR_RECOVERABLE_ROTATION` (client floor)
  registered in the authored-literals classifier (not value-pinned, like the match-type floor).

## Client (`multiplayerStore.ts`, `tournamentClient.ts`, `ws-adapter.ts`)

- `renewTournamentCredentialOver` sends `rotation_nonce`. `maybeRenewNearExpiry` mints a nonce
  (reusing a persisted one from a prior uncertain attempt), retries once in-call with the SAME nonce
  on an uncertain result, and **persists** the nonce on the credential
  (`organizerPendingRotationNonce` / `playerPendingRotationNonce`) until a rotation confirms — so a
  give-up-then-later-action recovers by replay instead of minting a fresh nonce the broker refuses.
- **Version gate:** proactive rotation runs only against a broker at/above
  `MIN_LOBBY_PROTOCOL_FOR_RECOVERABLE_ROTATION` (9); below it there is no replay, so the client does
  not rotate (preserving pre-feature behavior rather than risking a strand).
- Concurrent near-expiry actions on one authority share a single in-flight renewal (dedup).

## Tests

- **Server (unit):** mint-then-replay idempotency; a superseded secret + wrong/absent nonce is
  `Mismatch`; the **owner-B / attacker-A takeover regression** the maintainer asked for (B stays
  authoritative, A with a fresh nonce cannot mint or obtain a primary, owner can still advance); the
  lost-reply recovery via same-nonce replay, both roles.
- **Client:** the renew sender carries `rotation_nonce`; version-gate + skip paths; the uncertain
  result retries with the same nonce and persists it; a later attempt reuses the persisted nonce and
  clears it on recovery; concurrent-rotation dedup.

## Logistics

- **Not frontend-only.** Rust across `lobby-broker` + mirror crates → **claim the cargo-lock row in
  WORKLIST.md before any compiling cargo command**, release after. Tilt is not running locally —
  verify via direct cargo. Ships as one protocol-versioned PR (server + client together).
- Residual: a rotation whose reply is lost AND whose two in-call retries are lost, followed by no
  further action before the credential's TTL, is the narrow uncovered tail — the persisted nonce
  otherwise recovers on the next action.
