# Phase 61 — Swiss-Pairing Tournament Organizer — CONTEXT

## Why this matters

phase.rs has rich casual matchmaking (`lobby-broker`) but no support for
organized multi-round events. Running a Swiss tournament for a playgroup today
means tracking pairings and standings by hand outside the app. This has
already been proposed once (issue **phase-rs/phase#4612**), attempted once
(PR **phase-rs/phase#4615**, closed unmerged after review found real bugs —
not because the architecture was wrong), and is now being picked back up via a
second design pass, GitHub Discussion **phase-rs/phase#5314**, which grounds
the scoring/tiebreaker rules in the actual current Magic Tournament Rules
(MTR, effective 2026-02-27) instead of inventing convention from memory.

This document is a from-scratch, independent verification of every
repo-architecture claim in #5314 against the code actually on `main` today,
plus the real (not paraphrased) review findings from #4615, plus an informed
opinion on whether tournament scoring policy should hook into the sibling
custom-format-engine research effort (phase 58).

**Bottom line up front: the discussion's architecture claims all check out.**
Nothing in #5314 mischaracterizes the codebase in a way that would change the
proposed design. One phrase ("the existing `lobby_subscribers` broadcast
channel") is imprecise about which crate that channel actually lives in, but
the underlying mechanism it's pointing at is real and the proposed reuse is
still correct. Full citations are in RESEARCH.md; this file is the synthesis.

**Scope expansion (this update, per direct instruction): this design must
cover both head-to-head (Standard/Modern/etc.) and Commander/multiplayer pod
tournaments, not head-to-head only.** Every design section up to this point
implicitly assumed 2 players per pairing — the Swiss algorithm pairs, the
`ScoringPolicy`/tiebreak math assumes a winner-vs-loser binary, and the
single-elimination path is an adjacent-pair bracket. None of that is wrong
for what it covers; it just doesn't cover Commander pods (typically 4
players per match, one winner, MTR's own dedicated Multiplayer Addendum
governing scoring/tiebreaks/pairing differently from 1v1). This pass adds
finding #9 below (the Multiplayer Addendum to the MTR, "MSTR" — full detail
in RESEARCH.md §13) and generalizes PLAN.md's pairing/scoring/tiebreak
design over a new `MatchArity` parameter so `arity = 2` is the existing
design, not a fork of a new one — see PLAN.md §1-§2 for the resulting
shape.

**Re-verification pass (an earlier update): still holds, plus one real
update and one real find.** `main` has moved substantially since the
original research (commit `d65111246`) — re-checked every "confirmed fact"
below directly
against current `main` rather than trusting the earlier pass. Everything
held except `PROTOCOL_VERSION`'s specific value, which changed for a good
architectural reason (finding #4, updated below) — and one thing the
original pass missed entirely: `crates/draft-core` already has a working,
tested Swiss-pairing algorithm that sidesteps #4615's confirmed backtracking
bug by construction (finding #8, new below). Nothing here changes the
"architecture claims all check out" conclusion; it strengthens it with more
current evidence and one genuinely useful piece of prior art.

## Confirmed facts — verified against code this session

Everything below was checked directly against `main` (via a worktree at
`myfork/main`, commit `d65111246` at time of research) unless marked
otherwise — findings marked "UPDATED" or "NEW" were re-verified or newly
found against current `main` in a later pass. See RESEARCH.md for exact
file:line evidence.

1. **Greenfield within `lobby-broker` specifically, confirmed — REVISED,
   maintainer review: the original wording overreached to "or elsewhere in
   the workspace," which contradicts finding #8's own `draft-core` citation
   below and is corrected here, not merely reworded.** No `tournament.rs`,
   `TournamentManager`, or any lobby-scoped tournament type exists anywhere
   in `crates/lobby-broker/src/` on `main` today. `ls
   crates/lobby-broker/src/` shows only `broker.rs`, `env.rs`,
   `inbound_guard.rs`, `lib.rs`, `lobby.rs`, `protocol.rs`,
   `reservation_auth.rs`, `validation.rs`. #4615 was closed without
   merging, so none of its `lobby-broker`-side work landed. This crate's
   `TournamentManager` is a true from-scratch design, not a partially-built
   stub — but `crates/draft-core` DOES already have a working, tested
   Swiss-pairing implementation for a different purpose (draft-pod
   mini-tournaments, finding #8 below) that this proposal explicitly draws
   on as prior art. Do not read this finding as "no tournament-pairing code
   exists in the workspace" — only "none exists in `lobby-broker`,"  which
   is the crate this proposal actually adds to.

2. **`LobbyManager` + `Broker::handle` + `ConnState` + functional-core pattern
   — confirmed exactly as described.** `crates/lobby-broker/src/lobby.rs:96`
   defines `pub struct LobbyManager`. `crates/lobby-broker/src/broker.rs:136`
   defines `pub fn handle(&mut self, conn: &mut ConnState, msg:
   LobbyClientMessage, env: &impl BrokerEnv) -> Vec<Outbound>` — single entry
   point, pure function of `(state, conn, msg, env)` returning ordered side
   effects, exactly the "no I/O, no locking, no tokio" functional core the
   module doc comment (`broker.rs:1-7`) claims. `ConnState`
   (`broker.rs:46-58`) already has the "ownership stamp" idiom the discussion
   cites: `host_game: Option<String>` records which game this connection
   registered as host, torn down on disconnect. A `tournament_organizer` /
   `joined_tournaments` pair would be the direct sibling extension — but see
   finding #3 below on *how* that extension should be shaped (as tokens, per
   the #4615 review).

3. **The "existing `lobby_subscribers` broadcast channel" claim is correct in
   substance but the name lives one layer up from where the discussion
   implies.** There is no `lobby_subscribers` identifier inside
   `crates/lobby-broker`. The broker core is transport-agnostic: it emits
   abstract `Outbound::AddSubscriber` / `Outbound::RemoveSubscriber` /
   `Outbound::ToSubscribers(LobbyServerMessage)` (`broker.rs:63-76`), and each
   shell owns the actual registry. The *native* shell's registry is named
   `lobby_subscribers` — confirmed at `crates/phase-server/src/main.rs:84`
   (`type SharedLobbySubscribers = Arc<Mutex<Vec<mpsc::UnboundedSender<...>>>>`)
   and instantiated at `main.rs:824`. So "reuse `lobby_subscribers`, no new
   subscriber registry needed" is true for the native server, and the
   Cloudflare Worker shell has its own equivalent DO-connection fan-out — but
   a `TournamentManager` reuses this by emitting the *same* `Outbound`
   variants the lobby core already emits, not by directly touching a
   `lobby_subscribers` field that lives inside `lobby-broker` itself (no such
   field exists there). This is a precision correction, not a substantive
   correction — the reuse plan in #5314 is still exactly right.

4. **UPDATED (re-verified against current `main`, not just `d65111246`):
   `PROTOCOL_VERSION` is now 33, and — more consequential than the number
   itself — the lobby now has its OWN separate wire version.** Re-checked
   directly this session: `crates/lobby-broker/src/protocol.rs:104` (`pub
   const PROTOCOL_VERSION: u32 = 33;`). A commit that landed *after* this
   phase's original research, `6db58810b` ("fix(protocol): give the lobby
   its own wire version, decoupled from the full game (#7606)"), introduced
   `pub const LOBBY_PROTOCOL_VERSION: u32 = 1;` (`protocol.rs:133`) as a
   genuinely separate constant from `PROTOCOL_VERSION` — confirmed by commit
   message and by both constants existing side-by-side today, not a rename.

   **This changes PR 2's plan for the better, not just the number.** Since
   `TournamentManager` lives in `lobby-broker` and its new message variants
   are lobby-scoped (per this phase's own architecture, matching how
   `LobbyManager` already works), PR 2 should bump `LOBBY_PROTOCOL_VERSION`
   specifically, not the general `PROTOCOL_VERSION` — decoupling tournament
   wire changes from unrelated game-protocol churn, which is exactly the
   separation `6db58810b` was introduced to enable. This wasn't an option
   when the original research ran; it is now, and it's the more precise
   choice given `LOBBY_PROTOCOL_VERSION` didn't exist as a concept before.
   The original finding's phrasing ("PR 2 must bump from 13→14, not 12→13")
   is retracted, not just updated — the correct instruction is now "bump
   `LOBBY_PROTOCOL_VERSION`, currently 1, not the unrelated
   `PROTOCOL_VERSION`, currently 33."

   **Re-verified again — maintainer review caught that PLAN.md's PR 2 text
   was never actually updated to match this finding's own retraction above,
   and that current `main` has moved further still.** As of commit
   `d9c2a7874` (checked directly this pass): `PROTOCOL_VERSION = 35`
   (`protocol.rs:110`), `LOBBY_PROTOCOL_VERSION = 1`, unchanged since #1880
   (`protocol.rs:139`) — both numbers already stale relative to what this
   finding cited above (33/1), confirming these are fast-moving constants
   that will have moved again by whenever this proposal is actually
   implemented. **The durable takeaway is the architectural rule, not any
   specific number**: bump `LOBBY_PROTOCOL_VERSION` by one from its
   then-current value at implementation time, never `PROTOCOL_VERSION`.
   PLAN.md §4's PR 2 description is corrected to state the rule this way
   rather than citing a number that will be wrong again by the time anyone
   reads it.

5. **The Cloudflare Worker `is_empty()` predicate genuinely only checks lobby
   games today — bug #4 in the discussion is real and reproducible against
   current `main`.** `lobby-worker/broker-wasm/src/lib.rs:168-170`:
   ```rust
   pub fn is_empty(&self) -> bool {
       self.inner.lobby().is_empty()
   }
   ```
   `Broker` (`crates/lobby-broker/src/broker.rs:108-111`) currently has a
   single field, `lobby: LobbyManager`. Adding a `tournaments: TournamentManager`
   field to `Broker` without updating this predicate would reproduce exactly
   the bug #4615's review caught: a Durable Object holding only tournament
   state would report `is_empty() == true` and stop rescheduling its cleanup
   alarm. This must be fixed as part of PR 3 in the rollout, not deferred.

6. **Draft-pod session reconnect genuinely uses per-seat tokens, not socket
   identity — confirmed, and it's a good analogy.**
   `crates/server-core/src/draft_session.rs:27-28` stores `player_tokens:
   Vec<String>` per seat, generated via `generate_player_token()`
   (`crate::session::SessionManager`, imported at line 18). Lookup is by
   token: `seat_for_token(&self, token: &str) -> Option<usize>`
   (`draft_session.rs:44-47`) scans `player_tokens` for a match, and action
   dispatch (`draft_session.rs:274-290`) resolves the acting seat via
   `seat_for_token` before applying anything — never via the connection's
   socket identity. This is a real, load-bearing precedent in the same crate
   family (`server-core`) for "authority survives a socket bounce," and
   directly supports the discussion's proposed organizer/player-token fix.

7. **All seven of #4615's review findings are real bugs in the actual closed
   PR diff, not a summary embellishment.** Fetched via `gh api
   repos/phase-rs/phase/pulls/4615/reviews` and `.../comments` (the real
   review, from human reviewer `matthewevans` plus an automated Gemini
   pass) — not just the discussion's paraphrase. Cross-checked each against
   `gh pr diff 4615`:
   - **Backtracking base case bug — confirmed exactly.** The diff's
     `backtrack_pair` (added code, diff line ~2602) has:
     ```rust
     if players.is_empty() { return Some(acc.clone()); }
     if players.len() == 1 { return None; }
     ```
     For any odd-sized bracket, every recursive branch bottoms out at
     `players.len() == 1` and returns `None`, so the *entire* backtracking
     search fails for odd brackets and falls back to a greedy pairing that
     can produce avoidable rematches. The reviewer's suggested fix
     (`return Some(acc.clone())` for the one-remaining-player case, treating
     it as the float) is correct and cheap — a one-line base-case fix, not a
     redesign.
   - **Missing winner validation — confirmed exactly.** The diff's
     `validate_match_result` (diff line ~2219) only checks (a) the winner is
     one of the two paired players and (b) a draw has no winner. It never
     compares `winner_player_key` against which player actually has more
     game wins. A client can report `player_a_wins: 2, player_b_wins: 0,
     winner_player_key: Some(player_b)` and it passes validation, corrupting
     standings.
   - **Expiry keyed off `created_at`, never updated — confirmed exactly.**
     The diff's `check_expired` (diff line ~2179) filters
     `t.created_at < cutoff` with no `last_activity_at` field anywhere in the
     diff's `TournamentMeta`. The native reaper calls
     `broker.reap_expired(300, &SysEnv)` today for lobby entries
     (`crates/phase-server/src/main.rs:1000`, confirmed still 300s on
     current `main`) — the PR wired the same 300-second timeout to
     tournaments via `check_expired`, so any tournament whose *rounds* run
     longer than 5 minutes total (essentially all of them) would be deleted
     out from under active players.
   - **The four WebSocket-leak findings and the i18n finding** are all
     frontend-only (`TournamentLandingPage.tsx`, `TournamentPage.tsx`,
     `tournamentClient.ts`) and independently confirmed present in the actual
     diff at the cited lines. Not re-verified line-by-line here since the
     rollout plan (PR 4) rebuilds the frontend from scratch rather than
     reusing #4615's frontend code, but they're real, and PR 4 must not
     reintroduce them.

8. **NEW — a working, tested Swiss-pairing implementation already exists in
   `crates/draft-core`, missed by the original research pass entirely.**
   `crates/draft-core/src/session.rs` has a real `TournamentFormat::Swiss`
   variant (`crates/draft-core/src/types.rs:12`) and a `generate_swiss_pairings`
   function, used to run mini-tournaments among players who just finished a
   draft pod (Premier/Traditional/Sealed `DraftKind`s). Confirmed by direct
   read, not a grep hit taken on faith:
   - **Its pairing algorithm is architecturally simpler than what #4615
     attempted, and — this is the actionable part — the shape difference
     avoids #4615's confirmed backtracking-base-case bug (finding #7 above)
     by construction, not by a base-case fix.** It sorts players into
     score-group "brackets" by match wins, shuffles within each bracket for
     pairing variety, then greedily pairs within a bracket while avoiding a
     `prior_pairs: HashSet<(PlayerId, PlayerId)>` rematch set, and — critically
     — if a bracket has an odd player out, it *carries that one player down to
     the next bracket* (`carry: Option<(PlayerId, u8)>`) rather than
     backtracking to try a different pairing within the same bracket. An
     unpaired player after the last bracket takes a bye, credited as a match
     win by the caller. There is no recursive backtracking anywhere in this
     function — the entire bug class #4615 hit (the `players.len() == 1`
     base case returning `None` and killing the whole search for odd
     brackets) cannot occur in this shape, because there's no search to fail.
   - **Tested**: `test_swiss_pairings_8_players` and
     `swiss_pairings_include_bot_filled_seats` (`session.rs`, same file)
     exercise this directly.
   - **Scope difference, so this is prior art to learn from, not a
     drop-in reuse candidate as-is**: this implementation is keyed on
     `DraftSession`/`PlayerId`/`DraftPairing` types scoped to a single draft
     pod (typically 8 seats, `UnsupportedTournamentSize` enforces exactly 8
     for its sibling single-elimination path), not the arbitrary,
     lobby-wide player counts (4 to 128+, per the discussion's own MTR
     Appendix E table) the tournament-organizer needs to support. Literal
     code sharing would need real type-level unification across two crates
     with different lifecycles (`draft-core`'s pod exists only for the
     draft's duration; `lobby-broker`'s tournament outlives any single
     match) — not attempted here, and not free.
   - **Recommendation for whoever picks up PR 1**: adopt the same
     *algorithm shape* (greedy-within-bracket + carry-one-down, no
     backtracking) for `TournamentManager`'s Swiss pairing, explicitly
     citing this existing implementation as the precedent, rather than
     re-deriving a backtracking search and re-discovering #4615's bug from
     scratch. Whether that's implemented as literal shared code (a small
     crate, or moving the algorithm into a place both `draft-core` and
     `lobby-broker` can depend on) or as two independently-tested copies of
     the same shape is an implementation-time call PR 1 should make
     explicitly, not leave implicit.

9. **NEW — Commander/multiplayer pod tournaments are governed by a separate
   convention (the "Multiplayer Addendum to the MTR," referred to here as
   "MSTR") with different scoring, tiebreak, and pairing rules than 1v1 —
   not just a bigger pod on the same math. CORRECTED, maintainer review:
   this is an unofficial, independent-judge-authored document, NOT a
   Wizards of the Coast publication — the source page's own disclaimer
   says so explicitly, and this proposal's earlier framing ("official
   ruleset," "mirror of the official text") was wrong to imply Wizards
   authorship.** It fills a real gap (the official MTR has no multiplayer
   section of its own) as a widely-adopted community convention this
   proposal deliberately chooses to follow, not an official rule this
   proposal is merely restating. Fetched directly this session
   (juizes-mtg-portugal.github.io — Portuguese judge community — its own
   page states "This is an unofficial rules document written by
   independent judges. This is not official Wizards of the Coast
   documentation"), cross-referenced against BeNeLux cEDH's own community
   copy and TopDeck.gg's reference copy — full citations in RESEARCH.md
   §13 — not paraphrased from memory, and not claimed as anything more
   authoritative than what it actually is:
   - **Scoring generalizes cleanly, not by coincidence**: MSTR's win-point
     formula is `2n - 1` for pod size `n` — 7 points for a 4-player pod's
     win. At `n = 2` this is exactly `3`, the MTR §2.1 value #5312 already
     cites. This means `ScoringPolicy`'s existing 3/1/0 default is a special
     case of one general formula, not a separate convention — see PLAN.md
     §1's `default_for_arity`.
   - **Tiebreak order is genuinely different, not just re-scaled.** MSTR
     drops the "opponents' game-win %" axis entirely (pods are single-game;
     there's no per-player game-win count to average) and adds an
     "opponents' average match points" axis 1v1 doesn't have. The floor
     value both rulesets use (0.33 for 1v1, ≈0.14 for 4-player pods) is the
     *same* formula, `1 / win_points`, evaluated at each ruleset's own
     `win_points` — another clean unification, unlike the tiebreak order
     itself.
   - **Pairing is non-backtracking in MSTR's own convention too,
     independently confirming finding #8's recommendation.** MSTR's own
     algorithm is top-to-bottom
     assignment by current standing, with an iterative *swap* repair step
     for players who can't be placed without a rematch — no recursive
     search, no base case to get wrong. This is the same algorithmic
     *shape* `draft-core`'s existing 1v1 pairing already uses (greedy +
     carry-down repair, finding #8), just generalized to N-player pods
     instead of pairs. One algorithm now needs designing, not two — see
     PLAN.md §2.
   - **Uneven player counts get a short pod, not more byes.** MSTR:
     "pods may consist of a minimum of 3 players to avoid multiple byes"
     for a nominal 4-player event, with fairness tracking so the same
     player isn't shorted twice before everyone else has been shorted once.
     This has no analog in 1v1 (a bye is the only "can't fill the pairing"
     case there) and is a genuinely new piece of state (`had_short_pod`,
     PLAN.md §2), not a renamed existing field.
   - **Cross-checked against production practice, not just the rules
     text**: TopDeck.gg's own "Running Commander Tournaments" help page
     confirms this isn't academic — "Pods seat four by default. Odd fields
     produce a short pod or a bye" — matching MSTR's stated preference, in
     a platform actually running Commander events today.
   - **Scope note**: MSTR's own round-count table (4-5 players → single
     elimination only, 6-16 → 2 Swiss rounds + Top 4, etc.) is a *different*
     table from the MTR Appendix E table #5312 already cites for 1v1 — both
     are default lookups keyed off `(arity, player_count)`, not a single
     shared table, per PLAN.md §1's `total_rounds` framing.

## What this session did NOT re-verify (explicitly out of scope per the task)

The Magic Tournament Rules citations in #5314 (match points 3/1/0 at MTR
§2.1, tiebreaker order and 0.33 floors at MTR §3.1/Appendix C, bye scoring at
Appendix C, the round-count table at Appendix E, self-reporting at §2.4) are
taken as given from an external, official source already cited with URLs.
This is a code-architecture verification pass, not an MTR fact-check.

## Open questions for the maintainer

These are the discussion's own open questions, unchanged by this
verification pass — nothing found this session resolves them unilaterally:

1. **Is a flat `ScoringPolicy{win,draw,loss}` sufficient for v1**, or should
   it anticipate hooking into the custom-format-engine research (phase 58)
   if/when that lands? **This session's opinion, argued in PLAN.md: no — keep
   them separate.** `ScoringPolicy` is tournament-administration state that
   lives in `lobby-broker` and never touches a `GameState`; phase 58's
   `LegacyRuleSet` is in-game CR-rule-variation state that lives in the
   `engine` crate's `FormatConfig` and is consumed mid-game. They sit in
   different crates, different lifecycles (one tournament-scoped, one
   game-scoped), and different CR categories (MTR tournament administration
   vs. Comprehensive Rules game mechanics). Phase 58's own CONTEXT.md
   independently reached the same conclusion from the other side (see
   "Relationship to adjacent work" below) — this is not a contested call.

   **NEW this pass — checked what real MTG tournament platforms (TopDeck.gg,
   Melee.gg) actually expose as configuration, per direct request, since
   citing the MTR alone doesn't show what organizers actually ask for beyond
   the official rules.** Full detail in RESEARCH.md §12. Summary: both
   platforms confirm win/draw points ARE a real, requested configuration
   surface (not a hypothetical) — TopDeck.gg exposes explicit "points per
   win"/"points per draw" fields with organizer-set custom values (published
   before round 1 so players know the stakes), and Melee.gg exposes a
   simpler binary "Enable Draws" toggle (some organizers just disallow draws
   outright, a different axis than customizing their point value). **This
   surfaces one thing a flat `ScoringPolicy{win,draw,loss}` doesn't capture
   that maybe should be a fourth field: TopDeck.gg's bye handling diverges
   from the MTR text #5314 already cites.** MTR (Appendix C, already quoted
   in the discussion) says a bye scores as a win AND is *excluded* from
   opponents'-percentage averaging (no real opponent that round). TopDeck.gg
   instead credits a bye as a win but *adds* synthetic opponent history (3
   opponents at a fixed 0.2 win rate) so the bye round still contributes to
   tiebreaker averaging rather than being skipped. These are two genuinely
   different, real, in-production conventions for the exact same MTR rule —
   worth flagging to the maintainer as a possible fifth `ScoringPolicy`
   axis (a `ByeTiebreakerHandling` enum: `ExcludeFromAverages` (MTR) vs.
   `SyntheticOpponent` (TopDeck.gg-style)) rather than hardcoding MTR's
   choice as the only option, mirroring how the win/draw/loss points
   themselves are already planned as TO-configurable rather than fixed
   constants. Not resolved here — a genuine open question for the
   maintainer, same as the original three below, not something this pass
   decides unilaterally.
2. **Round advancement**: organizer-gated only for v1, or build an
   auto-advance timer now? No new evidence found either way this session;
   still a product-scope call, not an architecture one — a timer is additive
   later regardless of which is chosen for v1.
3. **Confirm v1 stays lobby-only** (no auto-launched `GameSession` per
   pairing), with full-mode integration as an explicit fast-follow. Confirmed
   this matches #4612's original framing exactly, and nothing found this
   session suggests otherwise. Recommend confirming as stated.
4. ~~**Does v1 need Commander single-elimination bracket play, or only
   Commander Swiss?**~~ — **RESOLVED, maintainer review: excluded from v1.**
   This session's earlier recommendation (treat pod-based SE as a natural,
   undesigned-in-detail extension of the existing arity-gated SE path) was
   correctly rejected — "in-scope but undesigned" isn't a real resolution
   of a genuine open design question (bracket/advancement semantics for a
   multi-player pod bracket are not a mechanical extension of 1v1's
   adjacent-pair SE). Final: `BracketShape::SingleElimination` ships for
   `arity = HEAD_TO_HEAD` only in v1; `CreateTournament` rejects
   `SingleElimination` + `arity != HEAD_TO_HEAD` at construction time.
   Commander/multiplayer pods get Swiss only. See PLAN.md §2 and §5.

## Relationship to adjacent work — scope boundaries

- **Custom-format-engine (phase 58, branch `research/custom-format-engine`).**
  Read that phase's CONTEXT.md and PLAN.md in full for this cross-reference.
  Phase 58 already independently confirmed, from its own investigation, that
  **phase.rs has no match-level concept at all today** — "no best-of-N, no
  tournament round, no draw/tiebreak state machine; the engine models single
  games only" — and explicitly flagged its own adjacent open question
  (draw/tiebreak resolution mechanics) as **out of scope for the
  custom-format engine itself**, deferring it to "a separate, later
  tournament/match structure design" — i.e., exactly this phase. Phase 58's
  `LegacyRuleSet` (mana burn, damage-uses-stack, pre-M10 Wish templating,
  legend-rule scope) is a set of **in-game CR-rule-variation toggles** that
  live on `FormatConfig` in the `engine` crate and are read during game
  resolution. `ScoringPolicy` (win/draw/loss match points feeding
  match-point accumulation and tiebreaker math) is **tournament
  bookkeeping** that lives in `lobby-broker`, never enters a `GameState`, and
  is applied *after* a game/match ends, not during it. These are two
  different CR-adjacent categories (MTR administrative scoring vs. CR game
  rules) living in two different crates with no shared runtime state — the
  same "categorical boundary" reasoning this repo's CLAUDE.md applies to
  keep Life (CR 119) and Power/Toughness (CR 208/209) from being unified
  under one enum applies here at a coarser grain. **Recommendation: do not
  make `ScoringPolicy` a field of `CustomFormatDef`/`LegacyRuleSet`, or wait
  on phase 58 landing.** `TournamentManager`'s `ScoringPolicy` should be
  designed and shipped independently in `lobby-broker`. If a future format
  ever wants scoring defaults to travel with a format preset (e.g., "Old
  School events on this site default to draws=0"), that is a *product*
  wiring question for whichever UI creates a tournament to read a suggested
  default off the chosen format — not an architectural coupling between the
  two crates. No blocking dependency exists in either direction; phase 58 and
  this phase can land in either order.
- **Watch-game-mode (phase 60) and audio/video-chat (phase 59)** — no
  meaningful overlap found. Tournaments organize P2P tables via the existing
  `CreateGameWithSettings`/`JoinGameWithPassword` flow per #4612; whether a
  tournament's tables are individually spectatable is exactly phase 60's
  existing "is a P2P game spectatable" open question (currently: no, P2P
  games can't be spectated at all — see phase 60 Open Q2), not something this
  phase needs to solve. Flagged for awareness only.
- **Issue #4612 / PR #4615** — this phase supersedes neither; it is the
  second design pass on the same feature request, incorporating the first
  attempt's real review findings as day-one design constraints rather than
  post-hoc fixes. #4612 stays open until this design ships; #4615 stays
  closed (its code never merged and is not being resurrected — PR 1 of the
  rollout is a clean rewrite of `tournament.rs`, not a rebase of #4615's
  branch).
