# PR 7 — Move tournament bearer credentials off localStorage (+ deferred rotation)

**As-of base:** `upstream/main` (includes #8673). Frontend-only: no Rust, no protocol bump.

This PR ships the **safe half** of the original credential-rotation plan — moving the bearer
secrets off `localStorage` — and **defers credential rotation** to a follow-up, because rotation
cannot be made safe on the current server protocol without a server-side change (see §3).

---

## 1. What this PR does

Tournament bearer secrets (`organizer_token` / `player_token`) were persisted to **localStorage**
via the `phase-multiplayer` Zustand `persist` (`partialize` included `tournamentCredentials`, and
`merge` rehydrated them). Secrets at rest in localStorage are readable by any same-origin script
for the life of the browser profile.

They now live in **sessionStorage** instead:
- Dropped from the localStorage persist `partialize`; `merge` no longer hydrates them from
  localStorage (it forces the in-memory initial so a stray/pre-migration copy can't win).
- Owned by a dedicated sessionStorage sync: `hydrateSessionTournamentCredentials()` (runs once at
  module load, after localStorage hydration) + a store-change subscription that mirrors the map to
  sessionStorage (removing the key when the map empties). Reads go through the existing
  `normalizeTournamentCredentials` validator/cap.
- Store persist **v6 → v7** migration strips any credentials a pre-v7 build wrote to localStorage,
  placed before the early-returning legacy-`serverAddress` arm so it always runs.

**Trade-off (accepted, per maintainer decision):** sessionStorage survives a refresh (an organizer
keeps authority) but is cleared when the tab closes — a narrower lifetime than localStorage, which
is the point. A credential lost this way is re-earned by re-create/re-join, which the model already
treats as possible.

Tests: sessionStorage-not-localStorage persistence, hydrate round-trip, empty-map key removal,
malformed-drop + cap on hydrate, and v6→v7 (and v5→v7) credential strip.

---

## 2. What is DEFERRED (rotation) and why

The original plan also added the `RenewTournamentCredential` client sender + a proactive
near-expiry rotation trigger. That was implemented and reviewed on #8782, and a **[HIGH]** review
finding (matthewevans, corroborated by CodeRabbit) showed it is **unsafe on the current server
protocol** — so it is removed from this PR and deferred.

---

## 3. The blocking finding: rotation is destructive and non-idempotent

`renew_credential` (`crates/lobby-broker/src/tournament.rs:2055-2102`) **replaces the stored token
before replying**, and the reply (`broker.rs:1377-1389`) carries **only** the freshly minted
secret. So a timeout / abort / connection loss *after the server commit but before the reply is
delivered* leaves the client holding a credential the broker has already invalidated — and an
expired/rotated credential is **unrenewable**, so the authority is **permanently stranded**. The
uncertain-failure results are indistinguishable at the client (`tournamentClient.ts` `requestOver`).

This is **not safe to solve in the client alone** (a client that reuses the old secret after an
uncertain result races the server commit either way). A safe rotation needs one of:
- **Idempotent rotation** — the request carries a client-minted correlation id/nonce; replaying it
  returns the *already-minted* replacement instead of rotating again, so a lost reply is
  recoverable by retry.
- **Bounded old-token overlap** — the broker keeps the previous secret valid for a short grace
  window after rotation, so a lost reply doesn't immediately strand the holder.

Either is a **server-side (`lobby-broker` + mirror crates) change with a lobby-protocol bump** —
no longer frontend-only. The follow-up PR does that first, then adds the client half (sender +
proactive near-expiry trigger driven off the stored `expires_at_ms`) on top, with a production-path
regression that drives server rotation, drops the reply, and proves the authority survives.

The full client-side rotation design (sender shape, `shouldRenewCredential`, `maybeRenewNearExpiry`,
7-day TTL sizing) is preserved on the `pr8782-fullrotation` tag / #8782 history for that follow-up.

---

## 4. Relationship to #8673
#8673 (v6 client-consumption) merged; this PR is rebased onto that `main`. Independent feature;
file overlap was only in the (now-deferred) rotation code — the storage move touches
`multiplayerStore.ts` and its tests only.
