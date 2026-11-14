# Swiss-Pairing Tournament Organizer — Design Proposal

**Status:** research and design only — no engine or frontend code in this PR.

This is a design proposal for a general Swiss/single-elimination tournament
organizer built on top of the existing casual lobby, motivated by the lack
of any organized multi-round event support in `lobby-broker` today. Running
a Swiss tournament for a playgroup currently means tracking pairings and
standings by hand outside the app.

Originating issue [phase-rs/phase#4612](https://github.com/phase-rs/phase/issues/4612),
a first implementation attempt [phase-rs/phase#4615](https://github.com/phase-rs/phase/pull/4615)
(closed unmerged after review found real bugs — not because the architecture
was wrong), and this second design pass via discussion
[phase-rs/phase#5314](https://github.com/phase-rs/phase/discussions/5314),
which grounds scoring/tiebreaker/pairing rules in the actual current Magic
Tournament Rules (and, for Commander/multiplayer pods, the official
Multiplayer Addendum to the MTR) instead of inventing convention from
memory.

- **CONTEXT.md** — why this matters, independently re-verified architecture
  claims against current `main`, the real (not paraphrased) review findings
  from #4615, the case for keeping tournament `ScoringPolicy` independent
  from the sibling custom-format-engine work (#7703), open questions for the
  maintainer, and the Commander/multiplayer scope this design now covers.
- **RESEARCH.md** — the detailed evidence trail: `LobbyManager`/
  `Broker::handle`/`ConnState` traced end-to-end, `draft_session.rs`'s
  token-reconnect precedent, `draft-core`'s existing non-backtracking Swiss
  pairing, real-world tournament-platform scoring levers (TopDeck.gg,
  Melee.gg), and the Multiplayer Addendum to the MTR's scoring/tiebreak/
  pairing rules for Commander pods.
- **PLAN.md** — the proposed schema (`TournamentManager`, `MatchArity`,
  `ScoringPolicy`, arity-selected `TiebreakOrder`), the token-based
  organizer/player authority model (fixing #4615's socket-identity bug), and
  a four-PR rollout sequencing (pure core → protocol/native server →
  Cloudflare Worker shell → frontend), each independently reviewable.

This is intended as a discussion artifact for maintainer review, not a
ready-to-merge implementation plan.
