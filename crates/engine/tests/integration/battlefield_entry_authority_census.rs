//! STRUCTURAL CENSUS over the FUSED battlefield-entry event pair and the anaphora slot behind it.
//! Three anchors, one instrument:
//!
//! * `GameEvent::ZoneChanged` — every production construction in `crates/engine/src` whose `from`
//!   field carries NO origin zone (spelled `from: None` or written in field-init shorthand) must
//!   live inside `zones::record_and_emit_entry_from_no_zone`, the single
//!   `from: None → Battlefield` record+emit authority, except three sites adjudicated BY NAME.
//! * `GameEvent::TokenCreated` — every production construction must live inside
//!   `token::push_committed_token_entry_events`, the single emitter, except one site adjudicated
//!   BY NAME. See the SECOND ANCHOR section below.
//! * SINGLE-ID PUBLISHES into either anaphora container (`state.last_created_token_ids` and
//!   `PendingCopyTokenResolution::created_ids`) — every production one must live inside
//!   `token::record_last_created_token` or `token::record_last_created_copy_batch_token`. No
//!   adjudicated survivors. See the THIRD ANCHOR section below.
//!
//! The first two anchors share [`literal_body`] and [`classify_anchor`]; all three share
//! `cfg_test_scoped_lines`, [`rs_files`] and the [`top_level_fn_headers`] / [`enclosing_fn`] scope
//! resolver. A second copy of that machinery would drift away from this one.
//!
//! CR 400.7 + CR 608.2i + CR 603.2c: `GameObject::snapshot_for_zone_change` leaves
//! `turn_zone_change_index` at a `0` placeholder for `restrictions::record_zone_change` to
//! overwrite. An entry that emits its `ZoneChanged` without reaching the recorder ships that
//! placeholder, so the CR 603.2c batched zone-change replay guard aliases it onto occurrence `0`
//! and `Ability::self_ref_own_departure_successor` subscripts a ledger row belonging to a DIFFERENT
//! object.
//!
//! COUNT, MEASURED on the pre-change tree (`b654513cb`), not recalled: SIX production writers had
//! the defect — `conjure.rs:213`, `counters.rs:535`, `counters.rs:839`, `gift_delivery.rs:168`,
//! `token_copy.rs:859`, `token_copy.rs:973`. Two more sites wrote the same
//! `from: None → Battlefield` shape but already routed through the recorder (`incubate.rs:128`,
//! `token.rs:1960`), for eight writers total. Fusing record and emit removes the call-site seam
//! the six were written through, and this census is the tripwire for a SEVENTH.
//!
//! WHAT THIS DOES NOT CLAIM. It is a SOURCE-TEXT instrument, so it is a tripwire against an
//! accidental clone, NOT a proof that one cannot be written. Three measured ceilings, all of them
//! real:
//!
//! * It cannot see a construction assembled through a helper that takes `from` as a parameter,
//!   nor one built by a macro. Neither exists today: the only non-`GameEvent::`-prefixed
//!   `ZoneChanged {` occurrences in `engine/src` are comments and the enum declaration in
//!   `types/events.rs`.
//! * A construction written ENTIRELY in field-init shorthand
//!   (`GameEvent::ZoneChanged { object_id, from, to, record }`) is textually identical to a
//!   destructuring pattern, so [`is_construction`] cannot separate them and the census skips it.
//!   This is the one FAIL-OPEN ceiling, and it is kept deliberately. The second anchor's
//!   [`is_token_construction`] closes the same ceiling by keying on FIELD COMPLETENESS, and
//!   backporting that predicate here was MEASURED rather than assumed: over `engine/src` it takes
//!   this anchor from the pinned 4 production / 10 test hits to 16 / 13, and every one of the 15
//!   additions is a destructuring CONSUMER (`let … else`, `if let`, match arms in
//!   `types/game_state.rs` x4, `game/triggers.rs` x2, `trigger_matchers.rs`, `trigger_index.rs`,
//!   `filter.rs`, `derived_views.rs`, `visibility.rs`, `merge_tests.rs`) — zero are constructions.
//!   The asymmetry is empirical, not structural: `TokenCreated`'s three fields are never all bound
//!   by a consumer (the widest elides one, `{ object_id, source_id, .. }`), while `ZoneChanged`'s
//!   four are routinely all bound because consumers need every one of them. So the mirror-image
//!   ceiling is fail-CLOSED in direction but not in magnitude — it would replace the pin with a
//!   12-file consumer list that churns on every unrelated edit, which is the exact outcome arm
//!   4(iii) exists to prevent. The same sweep found NO all-shorthand `ZoneChanged` construction in
//!   `engine/src`, so the ceiling is currently unexercised.
//! * Conversely, a PATTERN that renames a field (`record: rec`) reads as a construction and would
//!   be counted. That direction is fail-CLOSED — it can only ADD an unexpected hit and fail the
//!   exact multiset below. No such pattern exists in `engine/src` today (measured).
//!
//! STRUCTURAL COMPLETENESS, ENFORCED BY STRUCTURE — NEVER BY SUBSTRING. Round 3 of this census
//! keyed on the substring `from: None` inside a fixed six-line window from the anchor. Both halves
//! were evadable, and both evasions COMPILE (each is now a permanent arm of the anti-vacuity test
//! below, forms 6 and 7):
//!
//! 1. `let from = None;` followed by field-init shorthand `from,` — the value is still `None`, but
//!    the substring `from: None` never appears.
//! 2. Writing the `record:` field first as a multi-line `Box::new(…)` expression, which pushes
//!    `from: None` past the sixth line.
//!
//! Both are closed by scanning the literal's OWN extent (brace depth, [`literal_body`]) and by
//! classifying the `from` FIELD rather than matching a spelling of it.
//!
//! A THIRD THROUGH SIXTH evasion of the same instrument were found in rounds 8, 10 and 12, and
//! unlike the first two they are FAIL-OPEN — they REMOVE a hit rather than adding one:
//!
//! 3. An ordinary prose comment inside the literal carrying an unbalanced `}`. The brace scan
//!    counted it as the literal's closing brace and returned a TRUNCATED body, so every field
//!    written after the comment went unseen. This also defeated arm 4(ii), whose comment is
//!    brace-free and so never reached the scan.
//! 4. The same truncation through a `}` inside a string literal.
//! 5. The same truncation through a NESTED block comment. Rust block comments nest; the scan
//!    tracked them with a BOOL, so `/* outer /* inner */ } */` left comment state at the inner
//!    `*/` and the `}` truncated the body. This one survived the rewrite that closed 3 and 4, and
//!    the residual list that rewrite shipped did not name it.
//! 6. The same truncation through a RAW BYTE / RAW C string (`br#"…"#`, `cr#"…"#`). `b` and `c`
//!    are alphanumeric, so the guard that stops an identifier's trailing `r` opening a raw string
//!    rejected these too, and the `"` after it fell through to the `#`-blind escaped-string scan.
//!    The residual list that closed 5 did NOT name this one either, because that list said it was
//!    "derived from the branches the scanner actually has" — and a branch the scanner does not
//!    have is exactly what such a derivation cannot see. [`literal_body`]'s list is therefore now
//!    derived from an EXTERNAL basis: the Rust Reference's enumeration of tokens whose interior
//!    text is not code, each mapped to the branch that consumes it.
//!
//! All four are closed in [`literal_body`] — comment/string/char skipping for 3 and 4, a comment
//! DEPTH counter for 5, [`literal_prefix_start`] for 6 — and all four are permanent arms (1d, and
//! 4(iv) for the second anchor), each paired with the SAME literal carrying a balanced comment, a
//! non-nested comment, or an unprefixed raw string as its control, so each pair measures its
//! specific evasion rather than comments or raw strings in general.
//!
//! SEPARATELY, TWO MEASURED CEILINGS in the reused `cfg_test_scoped_lines` scope classifier (these
//! are about test/production SCOPING, not about finding the literal), both FAIL-CLOSED here:
//!
//! 1. Its rule is `opens_module || next.trim_end().ends_with('{')`, so a `#[cfg(test)]` item whose
//!    `fn` signature spans multiple lines (header ending in `(`) is never scoped. Live instance:
//!    `trigger_matchers.rs`'s `#[cfg(test)] pub(crate) fn test_trigger_source_context(`.
//! 2. It matches only the literal string `#[cfg(test)]`, so
//!    `#[cfg(any(test, feature = "test-support"))]` is never scoped either. Live instance:
//!    `ability.rs`'s `set_test_trigger_source_recursive`.
//!
//! Both are fail-CLOSED for THIS census because CONJUNCT 1 pins the production hit set as an EXACT
//! per-file multiset in which absent files must not appear: a mis-scoped test hit can only ADD an
//! unexpected file/count and FAIL, never let a real clone through. Fixing the classifier would move
//! `loop_shortcut_offer_writer_census`'s own pinned numbers, which is out of this change's scope.
//! Arm 1 of the anti-vacuity test MEASURES ceiling 1 rather than asserting it.
//!
//! ── SECOND ANCHOR: `GameEvent::TokenCreated` ─────────────────────────────────────────────────
//!
//! WHY IT EXISTS. The `ZoneChanged` anchor above pins only HALF of the fused pair. The other half
//! is the one this change moved: the `TokenCreated` emit was hoisted out of eight call sites into
//! `token::push_committed_token_entry_events` and gated on the recorder's own verdict. Until this
//! anchor existed, that half was protected by nothing but the fact that exactly one production
//! emit site happens to exist today — i.e. by ENUMERATION, the instrument the change argues
//! against. A new `events.push(GameEvent::TokenCreated { … })` written anywhere else tripped
//! nothing.
//!
//! COUNT, MEASURED on this tree, not recalled: 38 anchor occurrences in `engine/src`, of which
//! exactly TWO are production-scope constructions — the emitter (`effects/token.rs`, inside
//! `push_committed_token_entry_events`) and the adjudicated `stack.rs` probe. The other 10
//! constructions are `#[cfg(test)]`-scoped and the remaining 26 are consumer PATTERNS.
//!
//! THE CONSTRUCTION PREDICATE IS DIFFERENT HERE, AND THAT DIFFERENCE IS THE POINT. `ZoneChanged`
//! keys on an explicit `record:` initializer, so it cannot see a construction written entirely in
//! field-init shorthand — that is ceiling 2 above. The token emitter IS written entirely in
//! shorthand (`{ object_id, name, source_id }`), so reusing that predicate would have made this
//! anchor blind to the one site it exists to pin. [`is_token_construction`] therefore keys on
//! FIELD COMPLETENESS, which is Rust's own rule rather than a spelling: a struct-variant
//! EXPRESSION must initialise every field (enum variants admit no `..base` functional update),
//! while a PATTERN may elide any of them with `..`. Naming all three of
//! [`TOKEN_CREATED_FIELDS`] therefore counts every construction with NO false negatives.
//!
//! Its one ceiling is the mirror image and is fail-CLOSED: an EXHAUSTIVE pattern that binds all
//! three fields (`GameEvent::TokenCreated { object_id, name, source_id }`) is textually identical
//! to a construction and would be counted, ADDING an unexpected hit to the exact multiset below.
//! MEASURED: no such pattern exists in `engine/src` today — every consumer elides at least one
//! field, the widest being `effects/destroy.rs`'s `{ object_id, source_id, .. }`.
//!
//! The other ceilings carry over verbatim, for the same reasons and in the same direction:
//!
//! * A construction assembled through a helper taking the fields as parameters, or built by a
//!   macro, is invisible. Neither exists today: the only non-`GameEvent::`-prefixed
//!   `TokenCreated {` occurrence in `engine/src` is the enum declaration in `types/events.rs`.
//! * `cfg_test_scoped_lines`'s two scope ceilings are fail-CLOSED here for exactly the reason
//!   they are above — a mis-scoped test hit can only ADD an unexpected file to the per-file
//!   production multiset and FAIL, never let a real clone through.
//! * COMPLETENESS ACROSS CRATES, MEASURED: `GameEvent::TokenCreated` occurs in no crate outside
//!   `crates/engine/`. The four occurrences under `crates/engine/tests/**` are acceptance-row
//!   consumers of this surface, not members of it, and that tree is deliberately not walked.
//!
//! RESIDUAL, STATED RATHER THAN IMPLIED: the function-scope conjunct pins only the `token.rs`
//! hit. A second production construction written inside `stack.rs` that ALSO deleted the probe
//! would keep the multiset intact and pass. That is the same residual the `ZoneChanged` half
//! carries for each of its three non-authority files; closing it would mean a per-file enclosing-fn
//! pin on files whose hits are adjudicated survivors rather than authorities.
//!
//! ── THIRD ANCHOR: the SINGLE-ID ANAPHORA PUBLISH ─────────────────────────────────────────────
//!
//! WHY IT EXISTS. The two anchors above pin the two halves of the entry EVENT. The defect that
//! actually shipped in this change's round 9 and round 10 was neither: it was a single just-created
//! id published into the `TargetFilter::LastCreated` anaphora slot WITHOUT the object-existence
//! predicate — first into `state.last_created_token_ids` directly, then, after that was guarded,
//! into `PendingCopyTokenResolution::created_ids` one line BELOW the guard, which the copy-batch
//! drain assigns wholesale back onto ledger 3 and so republished the id the guard had withheld.
//! Four successive enumeration sweeps each declared that class closed. Its closure then rested on a
//! query written in a doc comment, i.e. on the same ENUMERATION this file's other two anchors exist
//! to replace. This anchor is that query made executable.
//!
//! WHAT IT KEYS ON, and why it inherits none of [`literal_body`]'s residuals: a method CALL, not a
//! struct-literal body. There is no brace scan, no comment/string/char skipping and no
//! construction-vs-pattern predicate — the anchor is the container's FIELD ACCESS and the verdict
//! is the next code token after it ([`call_tail`], then [`is_single_id_publish`] or
//! [`is_ambiguous_mutator`]). The FOUR fail-OPEN truncations that rounds 8, 10 and 12 found in
//! [`literal_body`] — evasions 3 through 6 — are therefore structurally absent here rather than
//! argued absent. (Four truncations, three rounds: round 12's byte-raw and raw-C prefixes are one
//! evasion with two spellings, and an earlier revision of this sentence counted the ROUNDS.)
//!
//! THE TAIL VOCABULARY, MEASURED on this tree rather than assumed — every distinct next-token form
//! following `.last_created_token_ids` or `.created_ids` in `engine/src`, over 157 field accesses:
//! `=` 39 (bulk assign), `[` 26 (indexed read), `.clone(` 25, `.len(` 19, `.is_empty(` 12,
//! `.iter(` 8, `;` 5, `{` 5 (a `for … in &state.last_created_token_ids {` header), `.extend(` 5,
//! `.contains(` 3, `.first(` 3, `)` 3, `.push(` 2, `.clear(` 1, `,` 1. `.push(` occurs exactly
//! TWICE, both inside the two authorities. NO tail begins with `/*`, which is what makes the
//! block-comment residual below latent rather than live.
//!
//! WHAT THE CLASSIFIER DOES WITH THAT VOCABULARY IS NOT DERIVED FROM IT — the derivation basis is
//! `std`'s method semantics, for the same reason [`literal_body`]'s residual list had to stop being
//! derived from the branches the scanner happened to have. `.push(` / `.insert(` always introduce
//! exactly one element; `.extend(` / `.append(` / `.resize(` / `.splice(` introduce one only for
//! arguments this scanner cannot read, so they are PINNED by conjunct 4 rather than judged;
//! `.clear(` / `.retain(` / `.truncate(` / `.remove(` / `.drain(` / `.pop(` cannot introduce an
//! element at all. `.insert(`, `.append(`, `.resize(` and `.splice(` occur zero times today and are
//! covered anyway — a classifier that only knew the spellings present in the tree is a classifier
//! that closes nothing. See [`is_single_id_publish`] and [`is_ambiguous_mutator`].
//!
//! THE RESIDUALS, all fail-OPEN unless noted, each measured:
//!
//! * A publish through an ALIAS (`let v = &mut state.last_created_token_ids; v.push(id);`) is
//!   invisible: the alias's own `.push` carries neither container name. The same query covers the
//!   UFCS form `Vec::push(&mut state.last_created_token_ids, id)`, whose tail is a bare `,`.
//!   MEASURED absent —
//!   `rg -n --pcre2 -U '&\s*mut\s+\w+\.(last_created_token_ids|created_ids)' crates/engine/src`
//!   exits 1 with no output, against a positive control of 89 match events for
//!   `&\s*mut\s+\w+\.objects` under the identical query shape. This is the direct analogue of the
//!   other two anchors' helper/macro ceiling.
//! * An INDEXED ASSIGNMENT (`state.last_created_token_ids[0] = id;`) introduces an id under a tail
//!   this anchor reads as `[`, which is also the 26 indexed READS. Deliberately NOT classified:
//!   separating them needs bracket matching plus a lookahead past the `]`, i.e. the fixed-window
//!   guess [`literal_body`]'s doc rejects, and counting `[` wholesale would move the conjunct-1 pin
//!   from `(2, 0)` to `(3, 25)`, its production multiset from `{effects/token.rs: 2}` to
//!   `{database/encore_tests.rs: 1, effects/token.rs: 2}`, and the files it touches from one to
//!   seven — 25 of the 26 `[` tails are `#[cfg(test)]` fixtures reading `…_ids[0]`, so the pin
//!   would churn on every new test that reads the ledger. MEASURED absent instead:
//!   `rg -n --pcre2 -U '\.(last_created_token_ids|created_ids)\s*\[[^\]]*\]\s*=[^=]'
//!   crates/engine/src` exits 1 with no output, against a positive control of 211 match events for
//!   `\w\s*\[[^\]]*\]\s*=[^=]` under the identical query shape.
//! * A block comment BETWEEN the field and its method call is a tail [`call_tail`] cannot read.
//!   Made fail-CLOSED by counting it — see [`is_single_id_publish`], arm 5(vi).
//! * PROSE that quotes the defect is skipped only in the three shapes [`code_span`] and the
//!   full-line rule handle: a full-line `//`, a leading `/* … */`, and a trailing `//` not preceded
//!   by a `"`. A `//` after a quote, a `/* … */` opened mid-line, a string literal containing the
//!   container-plus-verb text, and the interior lines of a multi-line block comment are still
//!   scanned. That residue is fail-CLOSED — it can only ADD a hit and fail the exact multiset,
//!   never hide a publish. MEASURED over every OCCURRENCE rather than every matching line: of 184
//!   container-name occurrences in `engine/src`, 27 sit in a full-line `//` and 157 are ordinary
//!   code — the same 157 the tail vocabulary above enumerates — and ZERO sit in any of the four
//!   residue shapes today.
//! * A publish into a container reached by a name this anchor does not know is invisible. The
//!   container set is closed by type rather than by search: `TargetFilter::LastCreated` reads
//!   `state.last_created_token_ids` (`game/filter.rs`, `game/targeting.rs`) and the only other
//!   writer of that field is the copy-batch drain's wholesale assign from
//!   `PendingCopyTokenResolution::created_ids`. Those are the two names.

use std::path::Path;

use super::loop_shortcut_offer_writer_census::{cfg_test_scoped_lines, rs_files};
// The comment rule is NOT this file's any more: it moved to `src/source_census.rs`, which the
// crate's own unit-test censuses share through a plain `mod` and this binary reaches through a
// `#[path]` declaration in `main.rs`. One implementation, both venues.
use super::source_census::{code, code_span};

/// The bare anchor, ASSEMBLED AT RUNTIME so this file can never count its own text — the doctrine
/// `loop_shortcut_offer_writer_census` files against its own superseded round-2 anchor.
///
/// Deliberately BARE: no `to:` qualifier. A qualifier-anchored detector is evadable by spelling
/// (`to: dest`), which is exactly the evasion the round-2 anchor lost to. Destination specificity
/// is CONJUNCT 3's job, as a secondary.
fn anchor() -> String {
    format!("{}::{} {{", "GameEvent", "ZoneChanged")
}

/// The second bare anchor, assembled at runtime for the same self-counting reason.
///
/// Deliberately BARE for the same reason as [`anchor`]: any field qualifier is evadable by
/// spelling, and the emitter writes all three of its fields in shorthand anyway.
fn token_anchor() -> String {
    format!("{}::{} {{", "GameEvent", "TokenCreated")
}

/// The literal's OWN extent: everything between the anchor's opening `{` and its matching `}`.
///
/// This replaces a fixed-size line window. A window is a guess about how long a literal is, and a
/// literal is exactly as long as its braces say — writing the `record:` field first as a multi-line
/// `Box::new(…)` expression pushes `from: None` off the end of any fixed window while leaving the
/// construction intact (evasion 2, form 7 below).
///
/// Nested braces are tracked, so `record: Box::new(ZoneChangeRecord { … })` does not terminate the
/// scan early. Braces that are NOT code are SKIPPED rather than counted: `//` and `/* … */`
/// comments (block comments by DEPTH, because Rust nests them), and string / raw-string / char
/// literals. Counting them was a FAIL-OPEN defect — a `}` in an ordinary prose comment closed the
/// literal early and TRUNCATED the captured body, so every field written after it went unseen and
/// the construction scored zero. Arms 1d and 4(iv) are that evasion, each paired with the same
/// literal carrying a BALANCED comment as its control.
///
/// WHERE THIS LIST COMES FROM, because its provenance is the thing that kept failing. Revision 1
/// claimed the only failure mode "can only ADD hits and fail the exact multiset" — the fail-CLOSED
/// direction — and asserting it as the ONLY one is what let the fail-OPEN truncation above hide.
/// Revision 2 listed four residuals and silently omitted the NESTED block comment, inside the very
/// rewrite that closed the others. Revision 3 said it was "derived from the branches the scanner
/// actually has", which cannot work: a branch the scanner does NOT have is invisible to that
/// derivation, and round 12 duly found a fourth truncation (`br#"…"#`) sitting in the gap.
///
/// So the derivation basis is now EXTERNAL to this scanner: the Rust Reference's list of tokens
/// whose interior text is not code, enumerated in full and each mapped to the branch that consumes
/// it. What follows is that enumeration, not a list of holes someone happened to find.
///
/// | Rust token kind        | example    | consumed by                                          |
/// |------------------------|------------|------------------------------------------------------|
/// | line comment (`//`, `///`, `//!`) | `// }` | `pair_at(_, '/', '/')` → break to end of line |
/// | block comment (NESTS; `/** */`, `/*! */`) | `/* /* } */ */` | `block_depth`      |
/// | char literal           | `'}'`      | [`skip_literal`]'s two `'\''` arms                    |
/// | byte literal           | `b'}'`     | same arms; the `b` is emitted as ordinary body text   |
/// | string                 | `"}"`      | [`skip_escaped`]                                      |
/// | byte string            | `b"}"`     | [`skip_escaped`]; the `b` is ordinary body text       |
/// | C string               | `c"}"`     | [`skip_escaped`]; the `c` is ordinary body text       |
/// | raw string             | `r#"}"#`   | [`skip_raw`], any `#` count                           |
/// | raw byte string        | `br#"}"#`  | [`skip_raw`] via [`literal_prefix_start`] (evasion 6) |
/// | raw C string           | `cr#"}"#`  | [`skip_raw`] via [`literal_prefix_start`] (evasion 6) |
/// | lifetime / loop label  | `&'a str`  | DELIBERATELY not a literal — see the residual below   |
///
/// That table is the completeness claim, and it is checkable by reading it against the Reference
/// rather than by trusting this file. `br` / `cr` are MEASURED absent from `engine/src` today
/// (`rg --pcre2 -U --json '(?<![A-Za-z0-9_])[bc]r#*"' crates/engine/src` → 0 match events, against
/// non-empty positive controls of 141 and 31 match EVENTS for `(?<![A-Za-z0-9_])r#*"` and
/// `(?<![A-Za-z0-9_])b"` under the identical query shape — 156 and 35 individual matches under
/// `--count-matches`. The two gaps have DIFFERENT causes, measured with ripgrep 15.2.0 rather than
/// assumed, because an earlier revision gave both the same wrong one: `b"` genuinely hits twice on
/// some lines (35 matches on 31 lines), so per-LINE event grouping explains 31 vs 35; `r#*"` does
/// NOT (156 matches on 156 distinct lines, no line carrying two), and its 141 comes from `-U`
/// MULTILINE mode merging adjacent matches into one event — `rg --pcre2 --json` without `-U`
/// reports 156 events for that same query. The control's job is to be non-empty, which every
/// figure satisfies), so evasion 6 was latent — which is the point: it was closed from the
/// grammar, not from a sighting.
///
/// THE RESIDUALS THAT REMAIN, each MEASURED rather than argued:
///
/// * An unterminated struct literal (only reachable on syntactically invalid source, which would
///   not compile) yields the tail of the file. Fail-CLOSED: extra hits fail the exact multiset.
/// * A string with no closing delimiter on its own line — i.e. ANY multi-line string — has its
///   CONTINUATION LINES scanned as CODE. [`skip_escaped`] and [`skip_raw`] both return
///   `chars.len()` when they reach the end of the line without finding one, which ends the inner
///   `while at < chars.len()` loop; the outer loop then advances `row` and resumes brace counting
///   on line 2 of the string. So the hazard is a `}` on the SECOND or later line of a string inside
///   an anchor literal, closing the literal early and truncating the captured body. It is NOT, as
///   an earlier revision of this residual said, the remainder of the FIRST line: that part is
///   inside the string and is correctly skipped, exactly as a terminated string's interior is.
///   Fail-OPEN. RAW strings behave identically, through [`skip_raw`]'s own `chars.len()` return,
///   and they are the COMMON multi-line form in Rust — MEASURED: of 156 raw-string openers in
///   `engine/src` (`(?<![A-Za-z0-9_])(b|c)?r#*"`, counted per occurrence), 28 have no closing
///   delimiter on the same line, several of them brace-dense JSON fixtures in
///   `database/card_db.rs` and `database/mtgjson.rs`. The earlier revision named neither the raw
///   form nor the right line.
///   What keeps this LATENT is the population this scanner actually walks — the anchor LINES, not
///   the subset it keeps: 326 `ZoneChanged` and 38 `TokenCreated` anchor lines in `engine/src`, of
///   which 20 and 10 respectively DO carry a string inside the literal's extent (e.g.
///   `types/events.rs`'s `name: "Test".to_string()`), and ZERO carry one that spans a line break.
///   An earlier revision's "no anchor literal contains a string at all" was false, and false in
///   the direction that stops a reader looking for the multi-line form.
/// * `'` opens a char literal only in the unambiguous `'c'` / `'\c'` forms, because a lifetime
///   (`&'a str`) is not a literal and swallowing to the next `'` would be the same truncation in a
///   new place. MEASURED: `engine/src` carries 51 `'{'`/`'}'` char literals on 46 lines (the count
///   is per OCCURRENCE; an earlier revision shipped the line count under the literal noun), which
///   is why the
///   branch exists at all rather than being dismissed as hypothetical. Longer escapes
///   (`'\u{7d}'`) are NOT skipped by those arms — they are 8 chars, not 4 — but their braces
///   always BALANCE, so brace counting is unaffected.
/// * NESTED block comments are handled by DEPTH (`block_depth`), not by a boolean. Under a boolean
///   the inner `*/` of `/* outer /* inner */ } */` ends comment state and the `}` truncates the
///   body. MEASURED absent from `engine/src` with a query that can actually EXPRESS the multi-line
///   form: `rg --pcre2 -U --multiline-dotall --json '/\*(?:(?!\*/).)*?/\*' crates/engine/src` → 0
///   match events. Its positive control is synthetic, because the tree has no live instance:
///   `printf 'fn a() {\n/* outer\n   /* inner */ }\n*/\n}\n' > /tmp/pc/nested.rs` then the same
///   query over `/tmp/pc` → 1. The LINE-ORIENTED query an earlier revision shipped
///   (`rg -n --pcre2 '/\*.*/\*'`) returns 0 on that same file, i.e. it was blind to the very
///   hazard it claimed to have measured absent. Arm 1d / evasion 5 pins the fix, with a NON-nested
///   block comment carrying the same stray `}` as its control.
/// * [`classify_anchor`] scans each line for the FIRST occurrence of ITS needle only, so two
///   constructions of the SAME anchor on ONE source line score 1. Fail-OPEN. MEASURED absent:
///   `rg --pcre2 -U --multiline-dotall --json 'GameEvent::ZoneChanged \{[^\n]*GameEvent::ZoneChanged
///   \{|GameEvent::TokenCreated \{[^\n]*GameEvent::TokenCreated \{' crates/engine/src` → 0 match
///   events, against a positive control proving the query CAN hit — the mixed-anchor form returns 1
///   (`effects/token.rs`'s `ZoneChanged { .. } | TokenCreated { .. }` match arm; no line number,
///   because a cited line number churns on every edit above it), which is
///   harmless because the two anchors are scanned in separate passes. rustfmt splitting long
///   literals is why the same-anchor case stays empty rather than being argued impossible.
fn literal_body(lines: &[&str], n: usize, needle: &str) -> String {
    let mut body = String::new();
    let start = lines[n]
        .find(needle)
        .expect("the caller located this needle on this line");
    let mut chars: Vec<char> = lines[n][start + needle.len()..].chars().collect();
    let mut depth = 1usize;
    let mut row = n;
    // A DEPTH, not a bool: Rust block comments NEST, so `/* outer /* inner */ } */` leaves comment
    // state at the INNER `*/` under a boolean and then counts the `}` as the literal's own closing
    // brace — the same fail-OPEN truncation the comment/string skipping above closes, surviving in
    // the rewrite that closed the others. Arm 1d / evasion 5 pins it.
    let mut block_depth = 0usize;
    loop {
        let mut at = 0usize;
        while at < chars.len() {
            if block_depth > 0 {
                if pair_at(&chars, at, '*', '/') {
                    block_depth -= 1;
                    at += 2;
                } else if pair_at(&chars, at, '/', '*') {
                    block_depth += 1;
                    at += 2;
                } else {
                    at += 1;
                }
                continue;
            }
            if pair_at(&chars, at, '/', '/') {
                break;
            }
            if pair_at(&chars, at, '/', '*') {
                block_depth += 1;
                at += 2;
                continue;
            }
            if let Some(past) = skip_literal(&chars, at) {
                at = past;
                continue;
            }
            match chars[at] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return body;
                    }
                }
                _ => {}
            }
            body.push(chars[at]);
            at += 1;
        }
        row += 1;
        match lines.get(row) {
            Some(next) => {
                body.push('\n');
                chars = next.chars().collect();
            }
            None => return body,
        }
    }
}

/// Do `chars[at]` and its successor spell the two-character token `first``second`?
fn pair_at(chars: &[char], at: usize, first: char, second: char) -> bool {
    chars[at] == first && chars.get(at + 1) == Some(&second)
}

/// The index just past the string / raw-string / char literal starting at `at`, or `None` when no
/// literal starts there.
///
/// The `b` / `c` PREFIXES need no arm of their own: `b"…"`, `c"…"` and `b'…'` reach the `"` / `'`
/// arms below with the prefix already emitted as ordinary body text, which is harmless because a
/// prefix carries no brace. The RAW forms are the exception and are handled by
/// [`literal_prefix_start`] — see evasion 6.
fn skip_literal(chars: &[char], at: usize) -> Option<usize> {
    match chars[at] {
        '"' => Some(skip_escaped(chars, at + 1)),
        // `r"…"` / `r#"…"#` and the `br` / `cr` raw byte / raw C forms, but not the trailing `r`
        // of an identifier. A raw IDENTIFIER (`r#foo`) falls through: its `#` run is not followed
        // by a `"`, so the `.then(…)` below yields `None`.
        'r' if !literal_prefix_start(chars, at)
            .checked_sub(1)
            .is_some_and(|before| chars[before].is_alphanumeric() || chars[before] == '_') =>
        {
            let hashes = chars[at + 1..].iter().take_while(|ch| **ch == '#').count();
            (chars.get(at + 1 + hashes) == Some(&'"'))
                .then(|| skip_raw(chars, at + 2 + hashes, hashes))
        }
        // A lifetime is not a literal, so `'` counts only in the unambiguous one-character forms.
        '\'' if chars.get(at + 1) == Some(&'\\') && chars.get(at + 3) == Some(&'\'') => {
            Some(at + 4)
        }
        '\'' if chars.get(at + 1) != Some(&'\\') && chars.get(at + 2) == Some(&'\'') => {
            Some(at + 3)
        }
        _ => None,
    }
}

/// Where a raw string's PREFIX starts: `at` itself, or the `b` / `c` immediately before it
/// (`br"…"`, `cr"…"`, and their `#`-delimited forms).
///
/// EVASION 6, and the reason this is a function rather than an inline condition. `b` and `c` are
/// alphanumeric, so [`skip_literal`]'s identifier guard — which exists to stop an identifier's
/// trailing `r` opening a raw string — rejected every raw BYTE and raw C string. The following `"`
/// then fell through to [`skip_escaped`], which honours no `#` delimiter and stops at the first
/// embedded `"`, so the rest of the raw string was scanned as CODE and its `}` closed the literal
/// early. Fail-OPEN, the same truncation class as evasions 3, 4 and 5: `br#"" }"#` scored (0, 0)
/// while the byte-for-byte identical `r#"" }"#` scored (1, 1). Arm 1d / evasion 6 pins the pair.
fn literal_prefix_start(chars: &[char], at: usize) -> usize {
    match at.checked_sub(1) {
        Some(before) if matches!(chars[before], 'b' | 'c') => before,
        _ => at,
    }
}

/// Index just past the closing `"`, honouring backslash escapes. An unterminated string consumes
/// the rest of the line (residual 2 in [`literal_body`]'s doc).
fn skip_escaped(chars: &[char], from: usize) -> usize {
    let mut at = from;
    while at < chars.len() {
        match chars[at] {
            '\\' => at += 2,
            '"' => return at + 1,
            _ => at += 1,
        }
    }
    chars.len()
}

/// Index just past a raw string's `"` plus its `hashes` closing `#`s. Raw strings have no escapes,
/// which is exactly why they need their own scan: `r#"}"#` is one `}` that is not code.
fn skip_raw(chars: &[char], from: usize, hashes: usize) -> usize {
    let mut at = from;
    while at < chars.len() {
        if chars[at] == '"' && chars[at + 1..].iter().take_while(|ch| **ch == '#').count() >= hashes
        {
            return at + 1 + hashes;
        }
        at += 1;
    }
    chars.len()
}

/// Does `body` name `from` in FIELD-INIT SHORTHAND rather than with an explicit value?
///
/// Splitting on the field separators (`,`) and on brace/newline boundaries yields one entry per
/// field; a shorthand field is the bare token. `from: None` yields `from: None`, which is not the
/// bare token, so the two spellings are classified independently and both are counted.
fn has_from_shorthand(body: &str) -> bool {
    body.split([',', '{', '}', '\n'])
        .any(|field| field.trim() == "from")
}

/// Is `body` a struct EXPRESSION (a writer) rather than a destructuring PATTERN (a consumer)?
///
/// MEASURED discriminator: an explicit `record:` initializer. Every construction in `engine/src`
/// writes `record: Box::new(…)`, and every pattern binds `record` (or `object_id`, or `to`) as a
/// bare name. Only needed for the shorthand branch — `from: None` is already unambiguous enough to
/// be pinned by name, and `log.rs`'s adjudicated PATTERN survivor is counted through it.
///
/// The two directions are not symmetric, on purpose. A renaming pattern (`record: rec`) would be
/// misread as a construction and ADD a hit, which the exact multiset rejects — fail-closed. An
/// all-shorthand construction is invisible; that ceiling is stated in the module header.
fn is_construction(body: &str) -> bool {
    body.contains("record:")
}

/// The three fields `GameEvent::TokenCreated` declares (`types/events.rs`).
const TOKEN_CREATED_FIELDS: [&str; 3] = ["object_id", "name", "source_id"];

/// Is `body` a `TokenCreated` struct EXPRESSION (a writer) rather than a destructuring PATTERN?
///
/// Keys on FIELD COMPLETENESS, not on a spelling, because the emitter writes every field in
/// shorthand and [`is_construction`]'s `record:` discriminator would score it zero. Rust requires
/// an enum struct-variant expression to initialise every field and admits no `..base` functional
/// update, so "names all three" has no false negatives; the mirror-image ceiling (an exhaustive
/// pattern binding all three) is fail-CLOSED and is stated in the module header.
///
/// Field names are taken as the text before the first `:` of each `,`/brace/newline-delimited
/// segment, which reads `object_id: PROBE_ID` and bare `object_id` identically.
fn is_token_construction(body: &str) -> bool {
    let named: Vec<&str> = body
        .split([',', '{', '}', '\n'])
        .map(|field| field.split(':').next().unwrap_or_default().trim())
        .collect();
    TOKEN_CREATED_FIELDS
        .iter()
        .all(|field| named.contains(field))
}

/// One classified hit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hit {
    file: String,
    line: usize,
    in_test: bool,
    /// CONJUNCT 3's secondary: does the same window name the battlefield literally? Meaningful
    /// only for the `ZoneChanged` anchor — a `TokenCreated` literal has no `to:` field, so this is
    /// uniformly `false` there and no token conjunct reads it.
    to_battlefield: bool,
}

/// Locate every non-comment `needle` line in `src`, take the literal's OWN extent, and keep the
/// hits whose body satisfies `keep`.
///
/// The shared walker for BOTH anchors. Keeping one copy is what guarantees the two pins agree on
/// the comment rule, on the brace scan, and on the `cfg_test_scoped_lines` scope resolver.
///
/// The needle is required in the line's CODE half ([`code_span`]), not merely in the line: a
/// whole-line-only exclusion counts a needle written after a trailing `//` as a construction.
/// Strictly more specific — `code_span` only ever removes comment text, and leaves a `//` that
/// follows a `"` in the code half.
fn classify_anchor(src: &str, file: &str, needle: &str, keep: impl Fn(&str) -> bool) -> Vec<Hit> {
    let scoped = cfg_test_scoped_lines(src);
    let lines: Vec<&str> = src.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| code(line).contains(needle))
        .filter_map(|(n, _)| {
            let body = literal_body(&lines, n, needle);
            keep(&body).then(|| Hit {
                file: file.to_string(),
                line: n + 1,
                in_test: scoped[n],
                to_battlefield: body.contains("to: Zone::Battlefield"),
            })
        })
        .collect()
}

/// Classify every no-origin-zone anchor hit in `src`.
///
/// A hit is an anchor whose literal body either writes `from: None` outright, or names `from` in
/// field-init shorthand inside something [`is_construction`] recognises as a writer. The second
/// branch is what a spelling-based detector misses: `let from = None;` + `from,` constructs exactly
/// the same event.
///
/// The comment exclusion is shared with `loop_shortcut_offer_writer_census::classify` and
/// `loop_shortcut_seat_pin_census::sites_in_source` — all three now route it through
/// [`code_span`], so the rule is whole-line AND trailing: a comment writes no event, and a
/// comment-blind anchor makes the tripwire fire on prose.
fn classify(src: &str, file: &str) -> Vec<Hit> {
    classify_anchor(src, file, &anchor(), |body| {
        body.contains("from: None") || (is_construction(body) && has_from_shorthand(body))
    })
}

/// Classify every `GameEvent::TokenCreated` CONSTRUCTION in `src`.
///
/// Unlike [`classify`] there is no value predicate to apply: `TokenCreated` carries no origin-zone
/// field, so every construction of it is in scope and the only question is construction-vs-pattern.
fn classify_tokens(src: &str, file: &str) -> Vec<Hit> {
    classify_anchor(src, file, &token_anchor(), is_token_construction)
}

/// The scan root, shared by both anchors: `crates/engine/src`, recursively, `.rs` only.
///
/// COMPLETENESS MEASURED, not assumed: NEITHER `GameEvent::ZoneChanged` nor
/// `GameEvent::TokenCreated` occurs in any crate outside `crates/engine/`.
/// `crates/engine/tests/**` is deliberately not walked — the acceptance rows are consumers of this
/// surface, not members of it (and this very file's runtime-assembled anchors would otherwise be a
/// finding).
fn census_with(classifier: impl Fn(&str, &str) -> Vec<Hit>) -> Vec<Hit> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    for path in rs_files(&root) {
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        let rel = path
            .strip_prefix(&root)
            .expect("walked path is under its root")
            .to_string_lossy()
            .replace('\\', "/");
        hits.extend(classifier(&src, &format!("engine/src/{rel}")));
    }
    hits
}

fn census() -> Vec<Hit> {
    census_with(classify)
}

/// The `GameEvent::TokenCreated` half of the pair, over the same root and with the same exclusions.
fn token_census() -> Vec<Hit> {
    census_with(classify_tokens)
}

/// The two containers the CR 111.1 anaphora slot (`TargetFilter::LastCreated`) is read out of:
/// `GameState::last_created_token_ids` (ledger 3) and `PendingCopyTokenResolution::created_ids`
/// (the copy-batch buffer, which `token_copy.rs`'s drain assigns WHOLESALE back onto ledger 3).
///
/// Anchored on the FIELD ACCESS (leading `.`), not the bare name, so a local `created_ids` vector
/// being built up for a later bulk assignment — the deliberately-out-of-scope republish class — is
/// not confused with a write to the buffer field.
const ANAPHORA_CONTAINERS: [&str; 2] = [".last_created_token_ids", ".created_ids"];

/// The next code token after a container field access, skipping whitespace, line breaks and `//`
/// comments.
///
/// THIS IS NOT A LINE SCAN, and that is the whole point. `token_copy.rs:321-323` and `:327-329`
/// write `pending` / `.created_ids` / `.extend(…)` across THREE lines, so a line-oriented rule
/// reports 9 buffer writers where there are 11 — the exact query-shape error that made round 8
/// find 4 of 5 ledger-3 sites and left round 9's two buffer siblings unguarded. Arm 5(ii) is that
/// multi-line shape made executable.
fn call_tail(lines: &[&str], row: usize, from: usize) -> String {
    let mut row = row;
    let mut rest: &str = &lines[row][from..];
    loop {
        // DELIBERATELY NOT ROUTED THROUGH `source_census::code`, measured rather than overlooked.
        // Routing it lets `call_tail` read THROUGH a `/* … */` sitting between the field and its
        // call, which turns arm 5(vi)'s UNREADABLE tail into a readable `.extend(` and drops that
        // arm from `(1, 1)` to `(0, 0)` — RUN, not reasoned. Arm 5(vi)'s contract is "a tail this
        // function cannot read is COUNTED", and it is not this change's to redefine. The shared
        // rule governs which text is a NEEDLE SITE; this function reads a TAIL, and the two
        // questions come apart exactly here.
        let trimmed = rest.trim_start();
        if !trimmed.is_empty() && !trimmed.starts_with("//") {
            return trimmed.chars().take(24).collect();
        }
        row += 1;
        match lines.get(row) {
            Some(next) => rest = next,
            None => return String::new(),
        }
    }
}

/// Does `tail` publish a SINGLE id into the container it follows?
///
/// The twelve `Vec` mutators this change's ledger-3 and buffer queries enumerate split three ways
/// by ARGUMENT-FREE semantics — which is the point, because that basis is checkable against the
/// `std` docs rather than against this tree's current contents:
///
/// * ALWAYS introduces exactly one element, whatever its argument: `.push(`, `.insert(`. Counted
///   here, and these two only.
/// * MAY introduce one element, depending on an argument this scanner cannot read: `.extend(`,
///   `.append(`, `.resize(`, `.splice(`. `state.last_created_token_ids.extend(iter::once(id))`
///   publishes exactly one id and is indistinguishable BY TEXT from `.extend(other_vec)`. NOT
///   counted here — and NOT thereby declared bulk. [`is_ambiguous_mutator`] carries them, and
///   conjunct 4 pins their whole population, so a new one cannot land unread.
/// * CANNOT introduce an element at all: `.clear(`, `.retain(`, `.truncate(`, `.remove(`,
///   `.drain(`, `.pop(`. Excluded because removal-only is a property of the method, not because
///   they happen to be absent here.
///
/// So the two classifiers together are the executable form of that twelve-verb query, rather than
/// a two-verb proxy for it: 2 counted, 4 pinned, 6 excluded by semantics.
///
/// A `/* … */` tail is counted TOO, and deliberately. [`call_tail`] skips `//` comments but not
/// block comments, so a block comment written between the field and its method call would hide the
/// call from this classifier — fail-OPEN. Counting the unreadable tail instead makes it ADD a hit
/// and FAIL the exact multiset, which is the same fail-CLOSED discipline
/// [`FN_PREFIX_ALLOW_SET`]'s abort applies to an unrecognised `fn` prefix. Measured: zero of the
/// container field accesses in `engine/src` carry such a tail today.
fn is_single_id_publish(tail: &str) -> bool {
    tail.starts_with(".push(") || tail.starts_with(".insert(") || tail.starts_with("/*")
}

/// The mutators whose ARGUMENT decides whether they introduce an id, bounded by PINNING them.
///
/// Not classified, PINNED. Conjunct 4 fixes their exact production multiset, so writing
/// `v.extend(std::iter::once(id))` anywhere in `engine/src` turns the census red and a human reads
/// the argument. That is the same trade the `/* … */` tail gets in [`is_single_id_publish`] — an
/// unreadable thing is COUNTED rather than skipped — applied to an unreadable ARGUMENT instead of
/// an unreadable tail.
///
/// SCOPE, STATED HONESTLY BECAUSE AN EARLIER REVISION OVERSTATED IT: this is a VOCABULARY, not a
/// proof of exhaustiveness over `std`. It bounds the eight verbs listed below and nothing else. An
/// earlier revision called this "the one fail-OPEN this anchor would otherwise carry" and claimed a
/// new ambiguous publish "cannot arrive without a human reading its argument"; both were false
/// universals — `extend_from_slice`, `clone_from`, `resize_with` and `extend_from_within` each
/// compile, each publish exactly one id, and each scored `(0, 0)` under the four-verb set. They are
/// listed now. Any `std` mutator NOT listed is a named fail-OPEN of this anchor, recorded in the
/// residual list rather than denied here.
///
/// Several of these occur zero times on either container today. They are listed because the CLASS
/// is what is being closed: "absent from the tree" is a liveness statement, not a coverage
/// statement — the lesson the `br` / `cr` raw-string evasion already paid for.
fn is_ambiguous_mutator(tail: &str) -> bool {
    [
        ".extend(",
        ".extend_from_slice(",
        ".extend_from_within(",
        ".append(",
        ".resize(",
        ".resize_with(",
        ".splice(",
        ".clone_from(",
    ]
    .iter()
    .any(|verb| tail.starts_with(verb))
}

/// Classify every access to either anaphora container in `src` whose TAIL satisfies `keep`.
///
/// The third anchor's walker, parameterised by the tail predicate so that
/// [`is_single_id_publish`] and [`is_ambiguous_mutator`] walk the SAME surface and cannot drift
/// apart — the two populations partition the mutator vocabulary and a second copy of this loop
/// would be the place that stopped being true.
///
/// It shares `cfg_test_scoped_lines`, [`rs_files`] and the [`top_level_fn_headers`] /
/// [`enclosing_fn`] resolver with the other two anchors, but deliberately NOT [`literal_body`]:
/// this anchor keys on a method CALL rather than on a struct-literal body, so it inherits none of
/// that scanner's brace/string/raw-string residuals. [`code_span`] is the one comment rule it does
/// carry, and it is subtractive-only by construction.
fn classify_container_tails(src: &str, file: &str, keep: fn(&str) -> bool) -> Vec<Hit> {
    let scoped = cfg_test_scoped_lines(src);
    let lines: Vec<&str> = src.lines().collect();
    let mut hits = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        // ONE comment rule, the shared one: a whole-line comment has an EMPTY code span, so the
        // `starts_with("//")` test this used to carry beside it was a second policy saying the
        // same thing — and a second place to get it wrong.
        let (lo, hi) = code_span(line);
        let code_part = &line[lo..hi];
        for needle in ANAPHORA_CONTAINERS {
            let mut from = 0usize;
            while let Some(at) = code_part[from..].find(needle) {
                from += at + needle.len();
                if keep(&call_tail(&lines, n, lo + from)) {
                    hits.push(Hit {
                        file: file.to_string(),
                        line: n + 1,
                        in_test: scoped[n],
                        // No `to:` field on a method call; no conjunct of this anchor reads it.
                        to_battlefield: false,
                    });
                }
            }
        }
    }
    hits
}

/// Classify every SINGLE-ID publish into either anaphora container in `src` — conjuncts 1–3.
fn classify_publishes(src: &str, file: &str) -> Vec<Hit> {
    classify_container_tails(src, file, is_single_id_publish)
}

/// Classify every ARGUMENT-AMBIGUOUS mutator call on either container in `src` — conjunct 4.
fn classify_ambiguous_mutators(src: &str, file: &str) -> Vec<Hit> {
    classify_container_tails(src, file, is_ambiguous_mutator)
}

/// Read `crates/engine/src/<rel>` — the source both anchors resolve enclosing functions against.
fn engine_src(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

/// The per-file production multiset of `hits`, sorted — CONJUNCT 1's shape for both anchors.
fn production_multiset(hits: &[&Hit]) -> Vec<(String, usize)> {
    let mut multiset: Vec<(String, usize)> = Vec::new();
    for hit in hits {
        match multiset.iter_mut().find(|(file, _)| *file == hit.file) {
            Some((_, count)) => *count += 1,
            None => multiset.push((hit.file.clone(), 1)),
        }
    }
    multiset.sort();
    multiset
}

/// Every column-0 `fn` header prefix that exists in `crates/engine/src`.
///
/// PINNED AS AN ALLOW-SET so an UNRECOGNISED prefix (a future `const fn`, `async fn`, `unsafe fn`)
/// ABORTS the census instead of being silently skipped. A hard-coded include-list fails OPEN — the
/// unknown header is skipped and an EARLIER header wins the resolution, which is the paid-for
/// lesson recorded in
/// `replacement.rs::every_applying_path_reaches_the_recorder_because_the_hook_is_in_pipeline_loop`.
/// This fails CLOSED.
///
/// MEASURED over all of `engine/src`, not over the two files this resolver happens to read. AT
/// THIS COMMIT'S TREE, by the command below (which implements exactly the rule
/// [`top_level_fn_headers`] applies — column 0, not a `//` line, some whitespace-delimited token is
/// a bare `fn`, prefix = the tokens before it):
///
/// ```text
/// find crates/engine/src -name '*.rs' -exec awk '
///   /^[ \t]/{next} /^[ \t]*\/\//{next}
///   {for(i=1;i<=NF;i++) if($i=="fn"){p="";for(j=1;j<i;j++)p=p $j" ";c[p]++;break}}
///   END{for(k in c) print c[k], "["k"]"}' {} + | sort -rn
/// ```
///
/// → 14021 bare, 1813 `pub(crate) `, 1492 `pub `, 734 `pub(super) `, 24 `pub(in crate::game) `.
///
/// THOSE FOUR ABSOLUTE COUNTS ARE POSITIVE CONTROLS, NOT INVARIANTS — they move whenever anyone
/// adds a top-level `fn` anywhere in the crate, and a previous revision of this file shipped the
/// BASE-tip `1808 pub(crate) ` beside the HEAD-tip `14008 bare`, a pair that existed at no commit.
/// The two facts the arms below actually depend on are re-derivable from that same command and do
/// NOT churn: the command emits exactly FIVE distinct prefixes (so the allow-set is complete), and
/// `const ` is not one of them (so arm 1b(ii)'s fixture is genuinely unrecognised).
/// `pub(in crate::game) ` is listed because the abort's own remedy
/// says "EXTEND `FN_PREFIX_ALLOW_SET`" while arm 1b(ii) used to hard-code that exact prefix as its
/// "unrecognised" fixture — following the error message turned one red test into another. The
/// prefix has zero column-0 instances in `zones.rs` and `effects/token.rs`, so the abort could not
/// actually fire; the contradiction was the defect, not the liveness.
const FN_PREFIX_ALLOW_SET: [&str; 5] = [
    "",
    "pub ",
    "pub(crate) ",
    "pub(super) ",
    "pub(in crate::game) ",
];

/// Collect `(line_number_1_based, fn_name)` for every column-0 `fn` header in `src`.
///
/// A header is a column-0, non-comment line one of whose whitespace-delimited tokens is the bare
/// token `fn`. In a rustfmt'd file every top-level item header starts at column 0, and a multi-line
/// signature's continuation (`) -> Option<…> {`) carries no bare `fn` token, so it is not collected.
///
/// `Err` carries the extend-the-allow-set message for an unrecognised prefix.
///
/// TOKENS COME FROM THE CODE HALF ([`code_span`]), as everywhere else in this binary: a trailing
/// comment naming `fn` on a column-0 code line would otherwise contribute a bare `fn` token and
/// resolve to an unrecognised prefix — loud rather than silent, but still the wrong answer. The
/// COLUMN-0 test deliberately stays on the ORIGINAL line: `code_span` skips a leading
/// `/* … */`, and testing the trimmed remainder for column 0 would drop a real header that
/// happens to carry one.
fn top_level_fn_headers(src: &str) -> Result<Vec<(usize, String)>, String> {
    let mut out = Vec::new();
    for (n, line) in src.lines().enumerate() {
        if line.starts_with([' ', '\t']) {
            continue;
        }
        // A whole-line comment yields an empty code half and therefore no `fn` token, so the
        // shared rule subsumes the comment test that used to sit in the condition above.
        let tokens: Vec<&str> = code(line).split_whitespace().collect();
        let Some(at) = tokens.iter().position(|token| *token == "fn") else {
            continue;
        };
        let prefix = tokens[..at]
            .iter()
            .map(|token| format!("{token} "))
            .collect::<String>();
        if !FN_PREFIX_ALLOW_SET.contains(&prefix.as_str()) {
            return Err(format!(
                "line {}: unrecognised top-level `fn` prefix {prefix:?} — the census resolves a \
                 hit's enclosing function by scanning back to the latest column-0 header, and an \
                 unknown prefix would silently resolve to an EARLIER function. EXTEND \
                 `FN_PREFIX_ALLOW_SET` (currently {FN_PREFIX_ALLOW_SET:?}) and re-check that the \
                 resolution below is still correct.",
                n + 1
            ));
        }
        // A column-0 header whose LAST token is the bare `fn` puts the name on the next line,
        // breaking the one-line-header assumption documented above. Route it through `Err` — a
        // raw index here would abort the census with an out-of-bounds panic, i.e. the wrong
        // failure mode for what is really an unhandled header shape.
        let Some(name_token) = tokens.get(at + 1) else {
            return Err(format!(
                "line {}: column-0 header ends at the bare `fn` token, so the function name is \
                 not on this line. The census resolves a hit's enclosing function by that name, \
                 and cannot do so here. Re-check the single-line-header assumption above.",
                n + 1
            ));
        };
        let name = name_token
            .split(['(', '<'])
            .next()
            .unwrap_or_default()
            .to_string();
        out.push((n + 1, name));
    }
    Ok(out)
}

/// A column-0 header ending at the bare `fn` token is reported, not panicked on.
///
/// NON-VACUITY: both fixtures carry a prefix that IS in `FN_PREFIX_ALLOW_SET` (`""` and `"pub "`),
/// so they clear the unrecognised-prefix `Err` above and actually reach the name-token read. A
/// fixture with an unknown prefix would return `Err` from the earlier arm and assert nothing here.
/// DISCRIMINATION is two-sided: dropping the `tokens.get(at + 1)` guard makes arm 1 panic with an
/// index-out-of-bounds instead of returning `Err`, and trivialising the function to always return
/// `Err` fails arm 2. No current `crates/engine/src` line has this shape (measured: zero), so this
/// pins the failure MODE of an unhandled header shape rather than a live occurrence.
#[test]
fn bare_trailing_fn_header_is_an_error_not_a_panic() {
    // Arm 1 — the shape CodeRabbit flagged: name is not on the header line.
    for src in ["fn", "pub fn", "fn\npub fn real_one() {}"] {
        let err = top_level_fn_headers(src)
            .expect_err("a header ending at the bare `fn` token must be reported as `Err`");
        assert!(
            err.contains("bare `fn` token"),
            "error must name the unhandled header shape, got: {err}"
        );
    }

    // Arm 2 — the happy path still resolves names, so arm 1 cannot be satisfied by a
    // blanket `Err`.
    let headers =
        top_level_fn_headers("pub fn alpha(x: u8) {}\nfn beta<T>() {}\n    fn nested() {}")
            .expect("well-formed single-line headers must parse");
    assert_eq!(
        headers,
        vec![(1, "alpha".to_string()), (2, "beta".to_string())],
        "indented `fn` is not a column-0 header and must not be collected"
    );
}

/// The enclosing top-level `fn` of `line`: the LATEST collected header at or before it.
fn enclosing_fn(headers: &[(usize, String)], line: usize) -> Option<&str> {
    headers
        .iter()
        .rev()
        .find(|(at, _)| *at <= line)
        .map(|(_, name)| name.as_str())
}

/// The two production `from: None` constructions that are NOT the authority, each adjudicated by
/// name with its verdict. A failure message naming them reads as "a NEW construction appeared",
/// not as "someone re-measured".
const ADJUDICATED_SURVIVORS: &str = "\n  \
     engine/src/game/log.rs      — a match PATTERN (`GameEvent::ZoneChanged { .., from: None, to, .. } => …`), a CONSUMER, not a writer;\n  \
     engine/src/game/merge.rs    — already routed through `record_zone_change`, and its `to: dest` is a VARIABLE, which the \
     `Battlefield`-hardcoding authority cannot serve.\n";

#[test]
fn every_from_none_battlefield_entry_construction_lives_in_the_authority() {
    let hits = census();
    let production: Vec<&Hit> = hits.iter().filter(|hit| !hit.in_test).collect();
    let test_scoped: Vec<&Hit> = hits.iter().filter(|hit| hit.in_test).collect();

    // ── CONJUNCT 1 — the surface, pinned BIDIRECTIONALLY and by per-file multiset. ───────────
    let multiset = production_multiset(&production);

    assert_eq!(
        (production.len(), test_scoped.len()),
        (3, 12),
        "the no-origin-zone battlefield-entry construction surface moved. Expected 3 production \
         constructions — the authority plus two adjudicated survivors:{ADJUDICATED_SURVIVORS}\
         A NEW production hit means a SEVENTH clone of the record/emit split was written: route it \
         through `zones::record_and_emit_entry_from_no_zone` instead. A REMOVED hit means the \
         authority or a survivor moved. The 12 test-scoped hits include the `stack.rs` observer \
         probe, the Saga entry-boundary regression, 7 other constructions spelled `from: None`, \
         and 3 written in field-init shorthand \
         (`analysis/sim.rs`, `trigger_matchers.rs`, `targeting.rs`), which the shorthand branch of \
         `classify` is what sees. \
         Production hits = {production:#?}"
    );

    assert_eq!(
        multiset,
        vec![
            ("engine/src/game/log.rs".to_string(), 1),
            ("engine/src/game/merge.rs".to_string(), 1),
            ("engine/src/game/zones.rs".to_string(), 1),
        ],
        "per-file production multiset moved. Absent files must NOT appear — that exactness is what \
         makes the classifier's two known ceilings fail-CLOSED (a mis-scoped test hit can only add \
         an unexpected file and fail).{ADJUDICATED_SURVIVORS}"
    );

    // ── CONJUNCT 2 — the FUNCTION-SCOPE anchor, fail-closed by construction. ─────────────────
    //
    // Without this, a seventh clone written INSIDE `zones.rs` that also removed the authority's own
    // hit would keep the multiset at `("zones.rs", 1)` and pass.
    let zones = engine_src("game/zones.rs");
    // No assertion on `headers.len()`: the list is recomputed every run and the enclosing fn is
    // resolved BY NAME below, so a count pin protects nothing and would fail on any unrelated
    // top-level `fn` added to or removed from this hot, multi-agent file.
    let headers = top_level_fn_headers(&zones).unwrap_or_else(|message| panic!("{message}"));

    let zones_hit = production
        .iter()
        .find(|hit| hit.file == "engine/src/game/zones.rs")
        .expect("zones.rs carries the authority's construction");
    assert_eq!(
        enclosing_fn(&headers, zones_hit.line),
        Some("record_and_emit_entry_from_no_zone"),
        "zones.rs's `from: None` construction at line {} is NOT inside the authority. Every such \
         construction in this file must live in `record_and_emit_entry_from_no_zone`; a second one \
         elsewhere in the file is the clone this census exists to catch.",
        zones_hit.line
    );

    // ── CONJUNCT 3 — `→ Battlefield` specificity, as a SECONDARY. ────────────────────────────
    //
    // Secondary because on its own it is evadable by spelling; necessary because the multiset
    // alone would not fire if `merge.rs` later spelled its `dest` as a literal `Zone::Battlefield`.
    let mut battlefield_files: Vec<&str> = production
        .iter()
        .filter(|hit| hit.to_battlefield)
        .map(|hit| hit.file.as_str())
        .collect();
    battlefield_files.sort_unstable();
    assert_eq!(
        battlefield_files,
        vec!["engine/src/game/zones.rs"],
        "exactly one production window may name `to: Zone::Battlefield` literally — the \
         authority. The `stack.rs` observer probe is test-scoped, `log.rs` binds a bare `to,` in a \
         pattern, and `merge.rs` uses the variable `dest`; either production survivor gaining the \
         literal is a new battlefield-entry writer.{ADJUDICATED_SURVIVORS}"
    );
}

#[test]
fn every_token_created_construction_lives_in_the_single_emitter() {
    let hits = token_census();
    let production: Vec<&Hit> = hits.iter().filter(|hit| !hit.in_test).collect();
    let test_scoped: Vec<&Hit> = hits.iter().filter(|hit| hit.in_test).collect();

    // ── CONJUNCT 1 — the surface, pinned BIDIRECTIONALLY and by per-file multiset. ───────────
    let multiset = production_multiset(&production);

    assert_eq!(
        (production.len(), test_scoped.len()),
        (1, 11),
        "the `TokenCreated` construction surface moved. Expected one production construction — \
         `token::push_committed_token_entry_events`, the SINGLE emitter every one of its eight \
         callers inherits its `record.is_some()` gate from. \
         A NEW production hit means a SECOND emit site was written: a `TokenCreated` that does not \
         go through the emitter is a creation event with no `created_tokens_this_turn` row behind \
         it, which makes `trigger_matchers::match_token_created` skip its CR 111.2 controller \
         filter and fire for a controller it should have rejected. Route it through \
         `token::push_committed_token_entry_events` instead. A REMOVED hit means the emitter moved. \
         The 11 test-scoped constructions include the `stack.rs` observer probe, `analysis/sim.rs`, \
         `game/log.rs`, `game/triggers.rs` x4, and `game/effects/destroy.rs` x4. \
         Production hits = {production:#?}"
    );

    assert_eq!(
        multiset,
        vec![("engine/src/game/effects/token.rs".to_string(), 1)],
        "per-file production `TokenCreated` multiset moved. Absent files must NOT appear — that \
         exactness is what makes `cfg_test_scoped_lines`'s two known scope ceilings, and \
         `is_token_construction`'s exhaustive-pattern ceiling, fail-CLOSED: each can only add an \
         unexpected file and fail."
    );

    // ── CONJUNCT 2 — the FUNCTION-SCOPE anchor, fail-closed by construction. ─────────────────
    //
    // Without this, a second emit written INSIDE `token.rs` that also removed the emitter's own
    // construction would keep the multiset at `("effects/token.rs", 1)` and pass. `token.rs` is
    // exactly where such a clone would be written, which is why this conjunct is not optional.
    let token_src = engine_src("game/effects/token.rs");
    let headers = top_level_fn_headers(&token_src).unwrap_or_else(|message| panic!("{message}"));

    let token_hit = production
        .iter()
        .find(|hit| hit.file == "engine/src/game/effects/token.rs")
        .expect("effects/token.rs carries the emitter's construction");
    assert_eq!(
        enclosing_fn(&headers, token_hit.line),
        Some("push_committed_token_entry_events"),
        "token.rs's `GameEvent::TokenCreated` construction at line {} is NOT inside the emitter. \
         Every such construction in this file must live in `push_committed_token_entry_events`, \
         where the `record.is_some()` gate ties the event to the CR 400.7 ledger row; a second one \
         elsewhere in the file is the clone this census exists to catch.",
        token_hit.line
    );
}

/// The two authorities every single-id anaphora publish must live inside, named with what each
/// one owns, so a failure reads as "a third publisher appeared" rather than "someone re-measured".
const ANAPHORA_AUTHORITIES: &str = "\n  \
     token::record_last_created_token            — the object-existence predicate, and the ONLY \
     production `.push` into ledger 3;\n  \
     token::record_last_created_copy_batch_token — consumes that same verdict and mirrors the id \
     into the in-flight copy batch, so one evaluation owns BOTH destinations.\n";

#[test]
fn every_single_id_anaphora_publish_lives_in_an_authority() {
    let hits = census_with(classify_publishes);
    let production: Vec<&Hit> = hits.iter().filter(|hit| !hit.in_test).collect();
    let test_scoped: Vec<&Hit> = hits.iter().filter(|hit| hit.in_test).collect();

    // ── CONJUNCT 1 — the surface, pinned BIDIRECTIONALLY and by per-file multiset. ───────────
    let multiset = production_multiset(&production);

    assert_eq!(
        (production.len(), test_scoped.len()),
        (2, 0),
        "the single-id anaphora-publish surface moved. Expected exactly 2 production publishes and \
         0 test-scoped ones — the two authorities:{ANAPHORA_AUTHORITIES}\
         A NEW production hit means a single just-created id can reach \
         `TargetFilter::LastCreated` WITHOUT the object-existence predicate, which is the defect \
         this change's round 10 review found after four enumeration sweeps had each declared the \
         class closed: a token that vanished during a replacement pause stays in the anaphora slot \
         and \"the token you created\" resolves to an object that never finished entering. Route it \
         through one of the two authorities instead. A REMOVED hit means an authority moved. \
         Production hits = {production:#?}"
    );

    assert_eq!(
        multiset,
        vec![("engine/src/game/effects/token.rs".to_string(), 2)],
        "per-file single-id anaphora-publish multiset moved. Absent files must NOT appear: \
         `counters.rs` and `token_copy.rs` are exactly where the round 9 and round 10 defects were \
         written, and their absence here is the claim this conjunct exists to keep \
         true.{ANAPHORA_AUTHORITIES}"
    );

    // ── CONJUNCT 2 — the FUNCTION-SCOPE anchor, fail-closed by construction. ─────────────────
    //
    // Without this, a third publish written INSIDE `token.rs` that also deleted one authority's own
    // `push` would keep the multiset at `("effects/token.rs", 2)` and pass. `token.rs` is where
    // both authorities live, so it is exactly where such a clone would be written.
    let token_src = engine_src("game/effects/token.rs");
    let headers = top_level_fn_headers(&token_src).unwrap_or_else(|message| panic!("{message}"));

    let mut enclosing: Vec<&str> = production
        .iter()
        .filter(|hit| hit.file == "engine/src/game/effects/token.rs")
        .map(|hit| {
            enclosing_fn(&headers, hit.line).unwrap_or_else(|| {
                panic!(
                    "token.rs publish at line {} resolves to no top-level fn",
                    hit.line
                )
            })
        })
        .collect();
    enclosing.sort_unstable();
    assert_eq!(
        enclosing,
        vec![
            "record_last_created_copy_batch_token",
            "record_last_created_token",
        ],
        "token.rs's single-id anaphora publishes are not the two authorities' own. Every `.push` \
         into either container in this file must live in one of them; a third one elsewhere in the \
         file — or one of these two moving out — is the clone this anchor exists to \
         catch.{ANAPHORA_AUTHORITIES}"
    );

    // ── CONJUNCT 4 — the ARGUMENT-AMBIGUOUS mutators, which is what makes conjuncts 1-3 a claim
    //    about the CLASS rather than about two spellings of it. ───────────────────────────────
    //
    // `state.last_created_token_ids.extend(std::iter::once(id))` publishes exactly one id and
    // reads, BY TEXT, exactly like `.extend(other_vec)`. No text scanner can tell them apart, so
    // this conjunct does not try: it pins the whole `.extend(` / `.append(` / `.resize(` /
    // `.splice(` population instead, and a new one anywhere in `engine/src` turns this red so a
    // human reads the argument. Kept as a separate pin rather than folded into conjunct 1 for a
    // measured reason: folding would take the pin to `(7, 0)` and put `counters.rs` and
    // `token_copy.rs` — whose ABSENCE is conjunct 1's whole claim — back into the multiset, and
    // would add a non-authority function to conjunct 2's list.
    let ambiguous = census_with(classify_ambiguous_mutators);
    let ambiguous_production: Vec<&Hit> = ambiguous.iter().filter(|hit| !hit.in_test).collect();

    assert_eq!(
        (
            ambiguous_production.len(),
            ambiguous.len() - ambiguous_production.len()
        ),
        (5, 0),
        "the argument-ambiguous mutator surface on the two anaphora containers moved. These are \
         the calls whose SINGLE-ID-ness this census cannot read, so they are pinned instead of \
         classified. If the new call's argument is a bulk source (a `Vec`, a \
         `CopyTokenApplyStatus`'s `created_ids`, a clone of ledger 3), update this pin in the same \
         commit. If it is a single id, it belongs in an authority instead:{ANAPHORA_AUTHORITIES}\
         Ambiguous hits = {ambiguous_production:#?}"
    );

    // Issue #5904 moved TWO of these from `counters.rs` to `token_copy.rs` without changing
    // the total, which is exactly the shape this pin's own instructions ask for. The
    // `ContinueCopyTokenCreation` resume arm's `if let Some(pending) = active_copy_token_mut()
    // { pending.created_ids.extend(..) } else { last_created_token_ids.extend(..) }` pair was
    // about to be COPIED verbatim into the new `ContinueCopyTokenEntryAfterAuraHost` resume,
    // so it was lifted into `token_copy::extend_copy_batch_created_ids` instead and both arms
    // now call it. Same five calls, same bulk arguments (`status.created_ids`, a `Vec`), one
    // fewer place to write the destination-selection rule wrong. `counters.rs` therefore drops
    // out of this multiset entirely; its absence is now a claim, not an omission.
    assert_eq!(
        production_multiset(&ambiguous_production),
        vec![
            ("engine/src/game/effects/token.rs".to_string(), 1),
            ("engine/src/game/effects/token_copy.rs".to_string(), 4),
        ],
        "per-file argument-ambiguous mutator multiset moved. All five of these are bulk \
         republishes today — `.extend(status.created_ids)` and `.extend(state.\
         last_created_token_ids.clone())` — and the pin exists so that a SIXTH cannot arrive \
         without someone reading its argument"
    );
}

// ── ANTI-VACUITY ─────────────────────────────────────────────────────────────────────────────
//
// A census that cannot fail is worse than none. Arms 1, 1b, 1c and 1d live here permanently — 1c
// and 1d carry the four measured evasions as synthetic sources, which is where they belong: a
// permanent test cannot mutate production source. Arms 2 (the cfg-scope revert probe) and 3
// (planting the same two evasions plus a canonical clone in `gift_delivery.rs`, end to end) are
// executor-run temporary mutations recorded in the change's probe ledger. Arm 4 is the
// `TokenCreated` anchor's own set, kept as a SIBLING test rather than as extra arms of the
// `from`-field resolver test so that the `(4, 10)` pin's own test stays untouched by token work.
//
// Arms 1d and 4(iv) pin the SAME shared function ([`literal_body`]) from both anchors on purpose:
// the truncation defect they cover was found through the token anchor and reached the `from`-field
// one identically, and a shared instrument with a one-sided control is how that stayed invisible.

/// Build a synthetic source carrying `body` once at production scope and once inside a
/// `#[cfg(test)] pub(crate) mod tests {`.
fn at_both_scopes(body: &str) -> String {
    let indented: String = body
        .lines()
        .map(|line| format!("    {line}\n"))
        .collect::<String>();
    format!("fn production() {{\n{indented}}}\n\n#[cfg(test)]\npub(crate) mod tests {{\n    fn scoped() {{\n{indented}    }}\n}}\n")
}

/// The four single-line-header forms, parameterised by the origin zone so the same generator
/// produces both the `from: None` population and its `from: Some(Zone::Hand)` negative control.
fn forms_1_to_4(from: &str) -> String {
    let needle = anchor();
    [
        format!("events.push({needle}\n    object_id: id,\n    {from}\n    to: Zone::Battlefield,\n    record: Box::new(rec),\n}});"),
        format!("let zc = {needle}\n    object_id: id,\n    {from}\n    to: Zone::Battlefield,\n    record: Box::new(rec),\n}};"),
        format!("match event {{ {needle} object_id, {from} to, .. }} => handle(object_id, to), _ => {{}} }}"),
        format!("return {needle}\n    object_id: id,\n    {from}\n    to: Zone::Battlefield,\n    record: Box::new(rec),\n}};"),
    ]
    .join("\n")
}

/// Form 5: the needle inside a `#[cfg(test)]` item whose `fn` signature spans MULTIPLE lines.
/// This is ceiling 1 made executable — the shipped classifier cannot scope it.
fn form_5(from: &str) -> String {
    let needle = anchor();
    format!(
        "fn production() {{\n    events.push({needle} object_id: id, {from} to: Zone::Battlefield, record: r }});\n}}\n\n#[cfg(test)]\npub(crate) fn helper(\n    state: &GameState,\n) -> T {{\n    events.push({needle} object_id: id, {from} to: Zone::Battlefield, record: r }});\n}}\n"
    )
}

/// EVASION 1, verbatim: bind the origin zone OUTSIDE the literal and write the field in shorthand.
/// The constructed event is byte-for-byte the same one the authority emits, and the substring
/// `from: None` never appears. Round 3's detector scored this `(0, 0)`.
fn form_6_shorthand_evasion() -> String {
    let needle = anchor();
    format!(
        "let from = None;\nevents.push({needle}\n    object_id: id,\n    from,\n    to: Zone::Battlefield,\n    record: Box::new(rec),\n}});"
    )
}

/// EVASION 2, verbatim: write `record:` FIRST as a multi-line expression, so `from: None` lands on
/// the literal's ninth line — outside any fixed-size window, and past a NESTED brace pair that a
/// naive scan would stop on. Round 3's detector (`WINDOW = 6`) scored this `(0, 0)`.
fn form_7_window_evasion() -> String {
    let needle = anchor();
    format!(
        "events.push({needle}\n    object_id: id,\n    record: Box::new(ZoneChangeRecord {{\n        name: name.clone(),\n        controller,\n        power,\n        toughness,\n        core_types,\n    }}),\n    from: None,\n    to: Zone::Battlefield,\n}});"
    )
}

/// EVASION 3, parameterised by the COMMENT the literal carries so the mutant and its control are
/// the SAME construction differing only in that one line.
///
/// A `//` comment inside the literal whose prose carries an unbalanced `}` — ordinary in this
/// codebase's comment style — decremented the raw brace scan to zero and TRUNCATED the captured
/// body, so every field written after the comment went unseen. Fail-OPEN, and in the direction
/// [`literal_body`]'s own doc used to argue was impossible.
fn form_8_comment_in_literal(comment: &str) -> String {
    let needle = anchor();
    format!(
        "events.push({needle}\n    object_id: id,\n    {comment}\n    record: Box::new(rec),\n    from: None,\n    to: Zone::Battlefield,\n}});"
    )
}

/// EVASION 4: the same truncation reached through a STRING literal rather than a comment. The
/// `}` is inside `"…"`, so it is not the literal's closing brace, but a scan that does not know
/// about string literals cannot tell.
fn form_9_string_brace_evasion() -> String {
    let needle = anchor();
    format!(
        "events.push({needle}\n    object_id: id,\n    record: Box::new(rec.named(\"a }} b\")),\n    from: None,\n    to: Zone::Battlefield,\n}});"
    )
}

/// EVASION 6, parameterised by the literal's PREFIX so the mutant and its control are the SAME raw
/// string differing only in the leading `b` / `c`.
///
/// The raw string's content carries a `"` BEFORE its `}`, which is what separates the two scans: a
/// scan that honours `#` delimiters skips the whole literal, while one that falls through to
/// [`skip_escaped`] closes at that embedded `"` and then reads the following `}` as the struct
/// literal's own closing brace. All three prefixes lex as valid Rust
/// (`rustc --edition 2021 --crate-type=lib`, rc=0).
fn form_10_raw_string_brace_evasion(prefix: &str) -> String {
    let needle = anchor();
    format!(
        "events.push({needle}\n    object_id: id,\n    record: Box::new(rec.tagged({prefix}#\"a \" }} b\"#)),\n    from: None,\n    to: Zone::Battlefield,\n}});"
    )
}

/// The NEGATIVE control for the shorthand branch: a destructuring PATTERN that binds `from` bare is
/// a CONSUMER and must not be counted, or the census would swallow ~20 `engine/src` match sites and
/// stop being a pin on writers.
fn pattern_binding_from() -> String {
    let needle = anchor();
    format!("match event {{ {needle} object_id, from, to, record }} => handle(from), _ => {{}} }}")
}

fn score(src: &str) -> (usize, usize) {
    let hits = classify(src, "synthetic");
    (
        hits.iter().filter(|hit| !hit.in_test).count(),
        hits.iter().filter(|hit| hit.in_test).count(),
    )
}

/// The emitter's own shape, VERBATIM: a construction written entirely in field-init shorthand.
///
/// This is the arm that justifies a second predicate at all. `is_construction`'s `record:`
/// discriminator — the one the `ZoneChanged` anchor uses — scores this `(0, 0)`, so reusing it
/// would have made the token anchor blind to the single site it exists to pin.
fn token_form_shorthand_construction() -> String {
    let needle = token_anchor();
    format!("events.push({needle}\n    object_id,\n    name,\n    source_id,\n}});")
}

/// The `stack.rs` probe's shape: the same three fields with explicit values, and a COMMENT inside
/// the literal (comments there must not knock a field off the completeness check).
fn token_form_explicit_construction() -> String {
    let needle = token_anchor();
    format!(
        "let tc = {needle}\n    object_id: PROBE_ID,\n    name: spec.display_name.clone(),\n    // the creating source is irrelevant here\n    source_id: PROBE_ID,\n}};"
    )
}

/// The emitter's shorthand shape carrying an ordinary prose comment whose brace is UNBALANCED.
///
/// This is arm 4(ii)'s claim ("an interleaved comment line must not hide a field") pushed to the
/// case that actually broke it: arm 4(ii)'s comment has no braces at all, so it never exercised
/// the brace scan.
fn token_form_comment_brace_construction() -> String {
    let needle = token_anchor();
    format!(
        "events.push({needle}\n    object_id,\n    // set by the copy tail, see the match arm ending in }}\n    name,\n    source_id,\n}});"
    )
}

/// The NEGATIVE control: the four consumer PATTERN shapes that actually occur in `engine/src`.
///
/// `trigger_matchers.rs`, `log.rs`, `destroy.rs`, `engine_debug.rs` and a dozen others are full of
/// these — 26 of the anchor's 38 occurrences are patterns. Counting them would turn the pinned
/// multiset into churn that moves on every unrelated consumer edit.
fn token_patterns() -> String {
    let needle = token_anchor();
    [
        format!("match event {{ {needle} .. }} => LogCategory::Token, _ => other }}"),
        format!("match event {{ {needle} object_id, .. }} => Some(*object_id), _ => None }}"),
        format!("let {needle} object_id, source_id, .. }} = event else {{ return None }};"),
        format!("events.iter().any(|e| matches!(e, {needle} name, .. }} if name == \"Soldier\"))"),
    ]
    .join("\n")
}

fn token_score(src: &str) -> (usize, usize) {
    let hits = classify_tokens(src, "synthetic");
    (
        hits.iter().filter(|hit| !hit.in_test).count(),
        hits.iter().filter(|hit| hit.in_test).count(),
    )
}

/// ARM 4 — the `TokenCreated` anchor's anti-vacuity set.
///
/// The `(2, 10)` pin above is only a measurement if the same instrument resolves differently on
/// different inputs. These three arms prove it can score non-zero, that it separates production
/// from `#[cfg(test)]` scope, and that it refuses consumers.
#[test]
fn the_token_created_resolver_keys_on_cfg_scope_and_on_field_completeness() {
    // (i) POSITIVE CONTROL + SCOPE DISCRIMINATION, on the emitter's real shorthand shape.
    assert_eq!(
        token_score(&at_both_scopes(&token_form_shorthand_construction())),
        (1, 1),
        "arm 4(i): the emitter writes all three fields in SHORTHAND. It must be seen once at \
         production scope and once inside a `#[cfg(test)] pub(crate) mod tests {{`. The \
         `ZoneChanged` anchor's `record:`-keyed `is_construction` scores this (0, 0), which is \
         exactly why `is_token_construction` keys on field completeness instead"
    );

    // (ii) The explicit-value shape, with a comment inside the literal.
    assert_eq!(
        token_score(&at_both_scopes(&token_form_explicit_construction())),
        (1, 1),
        "arm 4(ii): an explicit-value construction (the `stack.rs` probe's shape) must score the \
         same as the shorthand one, and an interleaved comment line must not hide a field from the \
         completeness check"
    );

    // (iii) NEGATIVE CONTROL: the consumer patterns. This is the arm that keeps the pin from
    //       swallowing the 26 match sites and becoming churn.
    assert_eq!(
        token_score(&at_both_scopes(&token_patterns())),
        (0, 0),
        "arm 4(iii): a destructuring PATTERN elides at least one field with `..`, so \
         `is_token_construction` rejects it. Counting these would add 26 consumer sites across 15 \
         files to the multiset"
    );

    // (iv) The same shape with an UNBALANCED brace in that comment. This is (ii)'s claim taken to
    //      the case that broke it: (ii)'s comment is brace-free, so it never reached the brace
    //      scan. Both anchors share `literal_body`, so this pins it from the token side while
    //      arm 1d pins it from the `ZoneChanged` side.
    assert_eq!(
        token_score(&at_both_scopes(&token_form_comment_brace_construction())),
        (1, 1),
        "arm 4(iv): a `}}` inside a `//` comment is prose, not the literal's closing brace. A raw \
         brace scan truncates the body there, so `name` and `source_id` are never named and \
         `is_token_construction` returns false — the emitter itself would go uncounted, which is \
         fail-OPEN"
    );
}

/// The exact shape of the round 10 defect: a single-id publish written NEXT TO the guarded one.
fn publish_form(receiver_and_field: &str) -> String {
    format!("{receiver_and_field}.push(object_id);")
}

/// The MULTI-LINE field access rustfmt actually produces (`token_copy.rs:321-323`), which is what
/// makes a line-oriented rule under-count this surface.
fn publish_form_multiline() -> String {
    "pending\n    .created_ids\n    .push(object_id);".to_string()
}

/// The non-publish tails that really occur in `engine/src`, one of each shape from the measured
/// tail vocabulary: bulk assign, bulk extend, clear, clone, len, index, contains.
fn non_publish_forms() -> String {
    [
        "state.last_created_token_ids = created_ids;",
        "state.last_created_token_ids.extend(status.created_ids);",
        "state.last_created_token_ids.clear();",
        "let v = state.last_created_token_ids.clone();",
        "let n = state.last_created_token_ids.len();",
        "let first = state.last_created_token_ids[0];",
        "let has = state.last_created_token_ids.contains(&object_id);",
        "pending.created_ids = created_ids;",
        "pending\n    .created_ids\n    .extend(state.last_created_token_ids.clone());",
    ]
    .join("\n")
}

fn publish_score(src: &str) -> (usize, usize) {
    let hits = classify_publishes(src, "synthetic");
    (
        hits.iter().filter(|hit| !hit.in_test).count(),
        hits.iter().filter(|hit| hit.in_test).count(),
    )
}

/// Conjunct 4's scorer. Same walker, other half of the mutator partition.
fn ambiguous_score(src: &str) -> (usize, usize) {
    let hits = classify_ambiguous_mutators(src, "synthetic");
    (
        hits.iter().filter(|hit| !hit.in_test).count(),
        hits.iter().filter(|hit| hit.in_test).count(),
    )
}

/// ARM 5 — the third anchor's anti-vacuity set.
///
/// The `(2, 0)` and `(5, 0)` pins are only measurements if the same instrument resolves
/// differently on different inputs. These arms prove it scores non-zero on the defect's own shape,
/// separates production from `#[cfg(test)]` scope, sees the multi-line field access a line-oriented
/// rule cannot, refuses every bulk/read form, refuses prose in all three positions it is skipped
/// in, fails CLOSED on a tail it cannot read, and PARTITIONS the mutator vocabulary between the two
/// classifiers — including the assert that `.extend(std::iter::once(id))` is invisible to the
/// single-id one, which is the limit this set exists to state rather than to hide.
#[test]
fn the_anaphora_publish_resolver_keys_on_cfg_scope_and_on_the_call_tail() {
    // (i) POSITIVE CONTROL + SCOPE DISCRIMINATION, on ledger 3 — the round 9 defect's own shape.
    assert_eq!(
        publish_score(&at_both_scopes(&publish_form("state.last_created_token_ids"))),
        (1, 1),
        "arm 5(i): a bare `state.last_created_token_ids.push(id)` is the UNGUARDED ledger-3 publish \
         that shipped at four sites before round 9. It must be seen once at production scope and \
         once inside a `#[cfg(test)] pub(crate) mod tests {{`"
    );

    // (ii) The BUFFER, in the multi-line shape rustfmt writes. This is the `-U` lesson made
    //      executable: a line-oriented rule reports 9 buffer writers where there are 11.
    assert_eq!(
        publish_score(&at_both_scopes(&publish_form_multiline())),
        (1, 1),
        "arm 5(ii): `pending` / `.created_ids` / `.push(…)` split across three lines is the round \
         10 defect's own shape AND the shape a line-oriented query cannot express. If this scores \
         (0, 0) the anchor has silently become line-oriented and the class it pins is unpinned"
    );

    // (iii) NEGATIVE CONTROL for the SINGLE-ID classifier, and note carefully what it does NOT
    //       claim. A bulk assign, `.clear`, `.clone`, `.len`, indexing and `.contains` cannot
    //       introduce an id — that is a property of the operation. `.extend` CAN
    //       (`.extend(std::iter::once(id))`); it is absent from this score only because its
    //       argument is unreadable, and it is accounted for by conjunct 4 / arm 5(vii), not
    //       dismissed here. Counting the read forms would make the pin churn on the ~150 reads
    //       and bulk republishes that are deliberately out of scope.
    assert_eq!(
        publish_score(&at_both_scopes(&non_publish_forms())),
        (0, 0),
        "arm 5(iii): the single-id classifier must not fire on a bulk assign, a `.clear`, or a \
         read (`.clone`, `.len`, indexing, `.contains`) — none of those can introduce an id, and \
         counting them would put every consumer file in the multiset. The `.extend` lines in this \
         fixture carry BULK arguments; they are uncounted here because this classifier cannot read \
         arguments at all, which is exactly what conjunct 4 and arm 5(vii) exist to bound"
    );

    // (iv) NEGATIVE CONTROL: prose, in the FULL-LINE `//` shape. The doc comments this very change
    //      added mention `pending.created_ids.push(id)` verbatim; a comment-blind anchor fires on
    //      them. Arm 5(viii) covers the other two shapes the same prose can take.
    assert_eq!(
        publish_score(&at_both_scopes(
            "// the guard shipped with an UNGUARDED `pending.created_ids.push(id)` below it"
        )),
        (0, 0),
        "arm 5(iv): a full-line `//` comment publishes nothing. This change's own `counters.rs` \
         and `token.rs` docs quote the defect verbatim, so a comment-blind anchor would pin its \
         own prose"
    );

    // (v) `.insert(` is the other single-element `Vec` write, and a fix that only closed `.push`
    //     would leave it open with no arm saying so.
    assert_eq!(
        publish_score(&at_both_scopes(
            "state.last_created_token_ids.insert(0, object_id);"
        )),
        (1, 1),
        "arm 5(v): `.insert(0, id)` publishes exactly one id, same as `.push(id)`"
    );

    // (vi) FAIL-CLOSED on an unreadable tail. `call_tail` skips `//` comments but not block
    //      comments, so a `/* … */` between the field and its call would hide the call. It is
    //      counted instead, which ADDS a hit and fails the exact multiset.
    assert_eq!(
        publish_score(&at_both_scopes(
            "state.last_created_token_ids /* see below */ .extend(v);"
        )),
        (1, 1),
        "arm 5(vi): a block comment between the field and its method call is a tail `call_tail` \
         cannot read. It is counted, so it ADDS an unexpected hit and FAILS the exact multiset — \
         the same fail-CLOSED choice `FN_PREFIX_ALLOW_SET`'s abort makes for an unknown `fn` prefix"
    );

    // (vii) THE ARGUMENT-AMBIGUOUS CLASS, both halves. `.extend(std::iter::once(id))` publishes
    //       exactly ONE id — this is the measured limit of the single-id classifier, asserted
    //       here rather than left as a claim in prose — and conjunct 4's classifier is what keeps
    //       it accounted for. The third assert is the one that matters: the two classifiers must
    //       PARTITION the vocabulary, so a form counted by one is never counted by the other and
    //       neither pin can absorb the other's churn.
    let single_id_extend = "state.last_created_token_ids.extend(std::iter::once(object_id));";
    assert_eq!(
        publish_score(&at_both_scopes(single_id_extend)),
        (0, 0),
        "arm 5(vii): `.extend(std::iter::once(id))` publishes one id and is INVISIBLE to the \
         single-id classifier, because no text scanner can separate it from `.extend(other_vec)`. \
         If this ever scores non-zero the classifier has started guessing at arguments, and the \
         guess is what will be wrong"
    );
    assert_eq!(
        ambiguous_score(&at_both_scopes(single_id_extend)),
        (1, 1),
        "arm 5(vii): the same call MUST be seen by the ambiguous-mutator classifier, once per \
         scope. If this scores (0, 0) the fail-OPEN of arm 5(vii)'s first assert is unbounded \
         again and the class this anchor exists for is only half pinned"
    );
    assert_eq!(
        ambiguous_score(&at_both_scopes(&publish_form(
            "state.last_created_token_ids"
        ))),
        (0, 0),
        "arm 5(vii): `.push(id)` belongs to conjunct 1, not conjunct 4. The two classifiers \
         partition the twelve-verb mutator vocabulary; an overlap would double-count a publish and \
         make each pin churn on the other's edits"
    );

    // (viii) COMMENT POSITION, the two shapes arm 5(iv)'s full-line rule does not reach — and,
    //        just as importantly, the two controls proving `code_span` cannot eat CODE. The
    //        removals are the only place this anchor deletes text before scanning, so a fail-OPEN
    //        introduced there would be invisible to every other arm.
    assert_eq!(
        publish_score(&at_both_scopes(
            "let n = 1; // shipped with state.last_created_token_ids.push(id) below"
        )),
        (0, 0),
        "arm 5(viii): a TRAILING `//` comment publishes nothing. Re-wrapping one of this change's \
         own doc lines into this position is the most likely way to fail the census with no defect \
         present"
    );
    assert_eq!(
        publish_score(&at_both_scopes(
            "/* shipped with pending.created_ids.push(id) below */"
        )),
        (0, 0),
        "arm 5(viii): a full-line BLOCK comment publishes nothing either"
    );
    assert_eq!(
        publish_score(&at_both_scopes(
            "/*count=*/ state.last_created_token_ids.push(object_id);"
        )),
        (1, 1),
        "arm 5(viii): CONTROL — the leading-`/*` removal must stop at the first `*/`. \
         `engine.rs` writes `/*tapped=*/` argument labels in exactly this shape, and a removal \
         that ran to end-of-line would silently unsee any publish written after one"
    );
    assert_eq!(
        publish_score(&at_both_scopes(
            "let u = \"http://x\"; state.last_created_token_ids.push(object_id);"
        )),
        (1, 1),
        "arm 5(viii): CONTROL — the trailing-`//` removal is suppressed when a `\"` precedes the \
         slashes, because that `//` is inside a string. Without this guard a URL on the same line \
         would hide a real publish, which is fail-OPEN"
    );
}

#[test]
fn the_census_resolver_keys_on_cfg_scope_and_on_the_literal_from_field() {
    // ── ARM 1 — POSITIVE CONTROL / keying. ──────────────────────────────────────────────────
    //
    // SOURCE A: forms 1–4, each once at production scope and once inside a `#[cfg(test)]` module.
    let source_a = at_both_scopes(&forms_1_to_4("from: None,"));
    assert_eq!(
        score(&source_a),
        (4, 4),
        "arm 1 / source A: four single-line-header construction forms — a bare `events.push`, a \
         `let` binding, a match PATTERN, and a `return` — must each be seen once at production \
         scope and once inside a `#[cfg(test)] pub(crate) mod tests {{`. A bare anchor sees all \
         four shapes; the qualifier-anchored detector this replaced saw only two."
    );

    // SOURCE B: form 5 alone. BOTH copies score PRODUCTION — that is ceiling 1, MEASURED here
    // rather than asserted in prose. The cfg-attributed copy is not scoped because its `fn` header
    // line ends in `(`, which satisfies neither `opens_module` nor `ends_with('{')`.
    let source_b = form_5("from: None,");
    assert_eq!(
        score(&source_b),
        (2, 0),
        "arm 1 / source B: a `#[cfg(test)]` item with a MULTI-LINE `fn` signature is a known, \
         measured ceiling of `cfg_test_scoped_lines` — BOTH copies (the correctly-scoped \
         production one and the cfg-attributed one the classifier cannot see) score production. \
         This is asserted because it is measured, not because it is desirable; it is fail-CLOSED \
         for this census because the per-file multiset above rejects unexpected files."
    );

    // NEGATIVE CONTROL on both sources: the window keys on `from: None`, not on the anchor alone.
    // One instrument resolving (4,4), (2,0) and (0,0) on different inputs is a measurement rather
    // than a constant.
    assert_eq!(
        score(&at_both_scopes(&forms_1_to_4("from: Some(Zone::Hand),"))),
        (0, 0),
        "arm 1 negative control (source A): a `Some(from)` construction is the `move_to_zone` \
         sibling and must NOT be counted"
    );
    assert_eq!(
        score(&form_5("from: Some(Zone::Hand),")),
        (0, 0),
        "arm 1 negative control (source B): same, for the multi-line-header form"
    );

    // ── ARM 1c — THE TWO MEASURED EVASIONS, permanently pinned. ─────────────────────────────
    //
    // Both COMPILE and both construct the identical event. Round 3's window+substring detector
    // scored each of these `(0, 0)`; the whole point of the brace scan and the field
    // classification is that they now score like any other construction.
    assert_eq!(
        score(&at_both_scopes(&form_6_shorthand_evasion())),
        (1, 1),
        "arm 1c / evasion 1 (field-init shorthand): `let from = None;` + `from,` is the same \
         construction spelled differently. A detector that matches the string `from: None` scores \
         it (0, 0) and lets the clone through"
    );
    assert_eq!(
        score(&at_both_scopes(&form_7_window_evasion())),
        (1, 1),
        "arm 1c / evasion 2 (record-first): a multi-line `record:` initializer pushes `from: None` \
         onto the literal's ninth line, past a NESTED brace pair. A fixed six-line window scores \
         it (0, 0); scanning to the literal's own closing brace does not"
    );

    // ── ARM 1d — THE TRUNCATION EVASIONS, two-sided on the SAME literal. ────────────────────
    //
    // Round 8's scan counted every `}` as code, so a `}` inside a comment or a string closed the
    // literal EARLY and the fields written after it were never captured. That is the fail-OPEN
    // direction — it REMOVES a hit — which is why it needs a permanent arm rather than the
    // "can only ADD a hit" reasoning the other ceilings rest on.
    //
    // The pair below is the discriminating control: both sources are the same construction, and
    // they differ only in whether the comment's braces are balanced. Under the raw scan the
    // balanced one scored (1, 1) and the stray-brace one scored (0, 0).
    assert_eq!(
        score(&at_both_scopes(&form_8_comment_in_literal(
            "// the Some(rec) arm above already closed with }"
        ))),
        (1, 1),
        "arm 1d / evasion 3 (stray `}}` in a comment): a comment writes no field and its braces \
         are not code. A raw brace scan treats that `}}` as the literal's own closing brace, \
         returns a TRUNCATED body, and never sees the `from: None` written after it — scoring \
         (0, 0) on a construction that compiles and emits the event"
    );
    assert_eq!(
        score(&at_both_scopes(&form_8_comment_in_literal(
            "// the Some(rec) arm above already closed with { }"
        ))),
        (1, 1),
        "arm 1d control: the SAME construction whose comment braces BALANCE. It scores (1, 1) \
         under the raw scan too, which is what makes the arm above a measurement of the stray \
         brace rather than of comments in general"
    );
    assert_eq!(
        score(&at_both_scopes(&form_9_string_brace_evasion())),
        (1, 1),
        "arm 1d / evasion 4 (`}}` inside a string literal): same truncation, reached through a \
         string rather than a comment. AT THIS TIP `engine/src` carries 51 `'{{'`/`'}}'` char \
         literals on 46 distinct lines (`grep -raoP \"'[{{}}]'\" crates/engine/src | wc -l`) and \
         156 raw-string openers (`grep -raoP '(?<![A-Za-z0-9_])(b|c)?r#*\"' crates/engine/src | \
         wc -l`), so brace-bearing literals are not hypothetical. Counted PER OCCURRENCE with \
         `-o`: an earlier revision reported these as `--json` MATCH EVENTS, which ripgrep emits \
         one per LINE and merges further under `-U`, so it undercounted every population whose \
         matches share a line -- and it is why the raw-string figure disagreed with the 156 in \
         [`literal_body`]'s residual for the SAME population. Both are positive controls \
         re-derivable from those commands, not invariants"
    );

    // EVASION 5 and its own control, on the SAME literal. Rust block comments NEST, so tracking
    // them with a bool exits comment state at the INNER `*/` and the `}` after it truncates the
    // body — the identical fail-OPEN class as 3 and 4, and it SURVIVED the rewrite that closed
    // them because that rewrite's residual list was copied from its predecessor rather than read
    // off the scanner. The control below is a NON-nested block comment carrying the same stray
    // `}`: it scores (1, 1) under a bool scan too, which is what makes the pair a measurement of
    // NESTING rather than of block comments in general.
    // CONTROL FIRST, deliberately: asserted BEFORE the evasion so that a single run against a
    // reverted (boolean) scan shows the control PASSING and only the evasion failing. With the
    // evasion first the run aborts before the control is ever evaluated, and "the control passes
    // under both scans" would be a claim rather than a line in the failure log.
    assert_eq!(
        score(&at_both_scopes(&form_8_comment_in_literal(
            "/* the Some(rec) arm already closed with } */"
        ))),
        (1, 1),
        "arm 1d control for evasion 5: the SAME construction whose block comment does NOT nest. It \
         scores (1, 1) under the bool scan too, so the arm below measures the NESTING and not \
         block comments as such"
    );
    assert_eq!(
        score(&at_both_scopes(&form_8_comment_in_literal(
            "/* the Some(rec) arm /* see below */ already closed with } */"
        ))),
        (1, 1),
        "arm 1d / evasion 5 (NESTED block comment): a bool `in_block_comment` leaves comment state \
         at the INNER `*/`, so the `}}` after it reads as the literal's closing brace, the body is \
         TRUNCATED, and `from: None` written after it is never seen — scoring (0, 0) on a \
         construction that compiles and emits the event. A DEPTH counter is the whole fix"
    );

    // EVASION 6 and its own control, on the SAME raw string. `b` and `c` are alphanumeric, so the
    // identifier guard that stops an identifier's trailing `r` opening a raw string also rejected
    // every raw BYTE / raw C string; the `"` after it fell through to `skip_escaped`, which honours
    // no `#` delimiter. CONTROL FIRST for the same reason as evasion 5: run against a scanner
    // without `literal_prefix_start`, the control PASSES and only the two evasion rows fail, so
    // "the control passes under both scans" is a line in the log rather than a claim.
    //
    // MEASURED against a verbatim extraction of this scanner, before and after
    // `literal_prefix_start`: control (1, 1) → (1, 1); `br` (0, 0) → (1, 1); `cr` (0, 0) → (1, 1).
    assert_eq!(
        score(&at_both_scopes(&form_10_raw_string_brace_evasion("r"))),
        (1, 1),
        "arm 1d control for evasion 6: a PLAIN raw string carrying `\" }}` scores (1, 1) under both \
         scans — the identifier guard never rejected an unprefixed `r`. This is what makes the two \
         arms below a measurement of the `b` / `c` PREFIX rather than of raw strings in general"
    );
    assert_eq!(
        score(&at_both_scopes(&form_10_raw_string_brace_evasion("br"))),
        (1, 1),
        "arm 1d / evasion 6 (raw BYTE string): `b` is alphanumeric, so `skip_literal`'s \
         identifier guard rejected the `r` of `br#\"…\"#`. The `\"` then reached `skip_escaped`, \
         which stops at the first embedded `\"` and honours no `#` delimiter, so the rest of the \
         raw string was scanned as CODE and its `}}` truncated the body — (0, 0) on a construction \
         that compiles and emits the event. `literal_prefix_start` is the whole fix"
    );
    assert_eq!(
        score(&at_both_scopes(&form_10_raw_string_brace_evasion("cr"))),
        (1, 1),
        "arm 1d / evasion 6b (raw C string): the identical hole through `c`, asserted separately \
         because a fix that special-cased only `b` would leave this one open and no arm would say so"
    );

    // NEGATIVE control for the shorthand branch — this is the arm that keeps the branch from
    // swallowing every consumer in `engine/src`.
    assert_eq!(
        score(&at_both_scopes(&pattern_binding_from())),
        (0, 0),
        "arm 1c negative control: a destructuring PATTERN binding `from` bare has no `record:` \
         initializer, so `is_construction` rejects it. Counting it would add ~20 consumer sites \
         across 15 files and turn the pinned multiset into churn"
    );

    // ── ARM 1b — CONJUNCT 2's own control. ──────────────────────────────────────────────────
    //
    // (i) The resolver must pick the LATEST header at or before the hit, including a `pub(super)`
    //     one — the exact prefix a hard-coded three-element list omitted, which is the failure mode
    //     model (2) paid for.
    let nested = "pub(crate) fn authority() {\n    let _ = 1;\n}\n\npub(super) fn decoy() {\n    let _ = 2;\n}\n";
    let headers = top_level_fn_headers(nested).expect("both prefixes are in the allow-set");
    assert_eq!(
        enclosing_fn(&headers, 6),
        Some("decoy"),
        "arm 1b(i): a hit inside `pub(super) fn decoy` must resolve to `decoy`, never to the \
         earlier `pub(crate) fn authority` — resolving to an earlier header is precisely how a \
         missing prefix form makes the census pass on a clone"
    );

    // (ii) An UNRECOGNISED prefix must ABORT with the extend-the-allow-set message, not resolve to
    //      an earlier header. The fixture is `const `, MEASURED at ZERO column-0 instances in all
    //      of `engine/src` — `rg -n --pcre2 '^const\s+fn\s' crates/engine/src` exits 1 with no
    //      output. The positive controls that the same instrument CAN return non-zero are the four
    //      prefix counts in `FN_PREFIX_ALLOW_SET`'s doc, measured at this commit's tree by the
    //      command printed there; they are controls, not invariants, and re-deriving them from that
    //      command is the only way to quote them. It used to be `pub(in crate::game) `, which the
    //      abort's own message tells the reader to ADD — so obeying the error turned this arm red
    //      instead of green.
    let foreign =
        "pub(crate) fn authority() {\n    let _ = 1;\n}\n\nconst fn intruder() -> u8 {\n    2\n}\n";
    let message = top_level_fn_headers(foreign)
        .expect_err("arm 1b(ii): an unrecognised `fn` prefix must fail the census, not be skipped");
    assert!(
        message.contains("EXTEND") && message.contains("const "),
        "arm 1b(ii): the abort must name the offending prefix and tell the reader to extend the \
         allow-set; got {message:?}"
    );

    // (iii) …and the prefix the remedy names must actually BE in the allow-set, or the remedy is a
    //       contradiction. This is the assertion that fails if `pub(in crate::game) ` is removed.
    let local = "pub(crate) fn authority() {\n    let _ = 1;\n}\n\npub(in crate::game) fn helper() {\n    let _ = 2;\n}\n";
    let headers = top_level_fn_headers(local).expect(
        "arm 1b(iii): `pub(in crate::game) ` is a real engine idiom and must be recognised",
    );
    assert_eq!(
        enclosing_fn(&headers, 6),
        Some("helper"),
        "arm 1b(iii): `pub(in crate::game) fn` is the one column-0 prefix in `engine/src` outside \
         the original four (24 instances). Aborting on it would make an unrelated edit red, and \
         the abort's advised remedy — extend the allow-set — is only followable if the set can \
         actually hold it"
    );

    // Positive control on the same instrument: a zero/abort result is only meaningful if the
    // instrument can also succeed on the real file.
    let zones = engine_src("game/zones.rs");
    assert!(
        top_level_fn_headers(&zones).is_ok(),
        "arm 1b positive control: the real zones.rs must parse cleanly under the same allow-set"
    );
}
