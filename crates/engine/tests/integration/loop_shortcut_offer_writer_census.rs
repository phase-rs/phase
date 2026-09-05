//! §6 R8 — THE OFFER-WRITER SURFACE TRIPWIRE, AS A TRACKED TEST RATHER THAN A
//! WRITTEN NUMBER.
//!
//! CR 732.2a: only the player with priority may suggest a shortcut, and the
//! engine's record of a live suggestion is a `WaitingFor::LoopShortcut` write.
//! The 5d period machinery adds certification paths, so the standing question
//! "did a new path learn to certify without declaring or driving?" needs an
//! instrument that re-measures on every `cargo test -p phase-engine` run. A
//! number written into a plan cannot fire; this can.
//!
//! WHAT IT PINS, and why it is an INVARIANCE claim rather than a re-measurement:
//! the offer-writer surface across `crates/engine/src` and `crates/phase-ai/src`.
//! NO NUMBER IS RESTATED HERE — not even to narrate the incident that caused this
//! rewrite. This header once carried a production/test pair that had drifted from
//! the assert below; quoting that pair *here* would reintroduce the defect,
//! because probe-pin validates only between its BEGIN/END markers and any number
//! above them is unanchored by construction. The incident, with both figures, is
//! recorded in `probe-pin/engine-census.toml`, whose anchors an adjudicator has to
//! edit anyway. The assert below is where the pair is authoritative. NO rustdoc in
//! this file states either half of it — the adjudication history names SUPERSEDED
//! test-half values only, each chain deferring to the assert rather than restating
//! it (the chains end at "the value the assert below pins", "the pinned value", and
//! "the assert below is the authority for the pair"; no single phrase is shared, so
//! none is quoted as if it were). Both
//! figures do recur inside the assert's own failure MESSAGE, beside the literal
//! they describe, where an adjudicator editing one has the other in view; P1's
//! anchors cover that literal and one sentence of that message. NOTHING covers
//! rustdoc, which is why the rustdoc now carries no figure OF THE PINNED PAIR.
//! ⚠ READ THAT SCOPE LITERALLY — it was written wider once and was false. This file's
//! SIBLING test (`the_cfg_scope_classifier_...`) still has its OWN asserted figures
//! restated in its rustdoc, and NOTHING pins them: that test builds its
//! input as an in-binary String, so no bind-mount can reach it and the manifest
//! declares it unpinnable. Those restatements can go stale exactly as the pinned
//! pair's did. They are disclosed rather than struck because striking them would edit
//! a test this commit does not pin and was not reviewed against. The
//! PROBE-PIN block below re-measures both halves on every `probe-pin check` — its
//! rows anchor the pinned tuple and the adjudication sentence inside the assert —
//! so moving either without regenerating the block turns the pin red. A failure
//! reads *"5d (or a successor) changed the offer-writer surface"*, not
//! *"someone re-measured"*.
//!
//! THE ANCHOR IS BARE — `WaitingFor::LoopShortcut {`, with no `= ` / `Ok(`
//! qualifier. A prefix-anchored regex cannot be completed by adding prefixes:
//! `Some(WaitingFor::LoopShortcut {`, `vec![WaitingFor::LoopShortcut {`, a bare
//! literal in argument position and `return WaitingFor::LoopShortcut {` are all
//! constructions, and a match-arm PATTERN is a *consumer* whose appearance is as
//! worth surfacing as a writer's. Dropping the qualifier pins the whole surface
//! and has no form gap by construction.
//!
//! Pattern copied from `no_top_level_test_binaries.rs` — the in-tree precedent
//! for a `#[test]` that reads the source tree through
//! `Path::new(env!("CARGO_MANIFEST_DIR"))` and asserts a structural invariant.

// ── PROBE-PIN ────────────────────────────────────────────────────────────────
// The claims below are MEASURED, not asserted in prose: each row is a mutation of
// the walked tree plus the verdict it produced. Regenerate with
//   cargo probe-pin run --write probe-pin/engine-census.toml
// A row whose anchor stops matching is a number that moved without an adjudication.
//
// DISCLOSURE — the SHAPE named as the anchor lint's residual now occurs in a MANIFEST for the
// first time. The predicate is stated because a bare "first time" is false: `docs/probe-pin.md`
// already prints an anchor of this shape as a worked example, so the shape's first appearance
// in-tree is the doc's, not this file's. What is new is the first one AUTHORED IN A SHIPPING
// MANIFEST. `docs/probe-pin.md` rejects anchors embedding a line number, and names one
// shape it cannot reject: a positional integer in a `("<path>", <int>)` slot. The block
// below carries anchors of that shape. In each, the integer is a COUNT (a per-file multiset
// entry), which is the legitimate form the lint deliberately admits — never a line. Said
// here because no instrument distinguishes the two. The doc's REVISIT CONDITION is narrower
// than the shape and is NOT met by this commit: it asks for a revisit only "if an anchor of
// the `("<path>", <int>)` shape is ever authored with a *line* in the integer slot". No
// revisit is owed here; what is owed is this disclosure.
// ⚠ THAT QUOTATION IS UNANCHORED, and saying so is the point: no probe in the manifest matches
// it, and `docs/probe-pin.md` is not one of this resource's deps, so editing the doc leaves this
// quotation silently stale. It is quoted rather than paraphrased because a paraphrase was wrong
// here once; it is disclosed rather than pinned because pinning another file's prose from this
// file would make an unrelated doc edit fail the census pin.
//
// VENUE LIMIT — the block below is checked by ONE venue: the Tiltfile's `probe-pin-census`
// resource, locally, on engine-source edits. It is NOT checked in GitHub CI, and CI
// enrollment is policy-blocked (`.agents/pr-review-policy.toml` `[hard_stops]` lists
// `.github/workflows/**`). A green block in a merged commit is not a CI-verified block.
// PROBE-PIN:BEGIN manifest=probe-pin/engine-census.toml digest=sha256:d4d228a11ec1379c
// instrument rustc = rustc 1.97.0-nightly (0febdbab2 2026-04-18)
// | probe | mutation | expect | verdict | firing assertion (anchor) | provenance |
// |---|---|---|---|---|---|
// | P0_control | (none) | pass | pass | (control; no mounts) | — |
// | P1_production_site_removed | scenario.rs ×1 | fail | fail | left: (22, 21) / right: (23, 21) / THE TEST HALF HAS BEEN ADJUDICATED FIVE TIMES | crates/engine/tests/integration/loop_shortcut_offer_writer_census.rs |
// | P2_test_site_removed | projection.rs ×1 | fail | fail | left: (23, 20) / right: (23, 21) | crates/engine/tests/integration/loop_shortcut_offer_writer_census.rs |
// | P3_walk_reaches_phase_ai_and_skips_comments | lib.rs ×1 | fail | fail | left: (24, 21) / right: (23, 21) | crates/engine/tests/integration/loop_shortcut_offer_writer_census.rs |
// | P4_counting_is_per_line | lib.rs ×1 | fail | fail | left: (25, 21) / right: (23, 21) | crates/engine/tests/integration/loop_shortcut_offer_writer_census.rs |
// | P5_relocation_preserves_the_count | scenario.rs ×1, interaction.rs ×1 | fail | fail | the COUNT can be preserved by a move that relocates a writer / ("engine/src/game/interaction.rs", 6) | crates/engine/tests/integration/loop_shortcut_offer_writer_census.rs |
// | P6_second_validate_pins_consumer | scenario.rs ×1 | fail | fail | expected `validate_pins(` to appear in production exactly twice | crates/engine/tests/integration/loop_shortcut_offer_writer_census.rs |
// | P7_coverage_half_unpaired | decision_template.rs ×1 | fail | fail | validating pin VALUES without also running | crates/engine/tests/integration/loop_shortcut_offer_writer_census.rs |
// | P8_authority_call_site_removed | engine.rs ×1 | fail | fail | expected 1 definition + 3 call sites / ("engine/src/game/engine.rs", 1) | crates/engine/tests/integration/loop_shortcut_offer_writer_census.rs |
// probe-pin validates only the lines between BEGIN and END. Prose outside is never checked.
// PROBE-PIN:END

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::source_census::code;

/// The bare anchor, ASSEMBLED AT RUNTIME.
///
/// This file lives under `crates/engine/tests/`, which the census does not walk,
/// so a literal could not be self-counted today. Assembling it anyway keeps that
/// true across a future move: an instrument that can count its own needle
/// reports its own text as a finding.
fn anchor() -> String {
    format!("{}::{} {{", "WaitingFor", "LoopShortcut")
}

/// The ROUND-2 anchor this row replaces — a CONSTRUCTION-shaped detector. Kept
/// only so the foreign-form plant below can measure that it scores `(0, 0)` on
/// input the bare anchor scores `(4, 4)` on; that measurement is the statement
/// that the old tripwire was evadable.
fn construction_anchors() -> [String; 2] {
    let bare = anchor();
    [format!("= {bare}"), format!("Ok({bare}")]
}

/// One classified hit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hit {
    file: String,
    line: usize,
    in_test: bool,
    /// The trimmed source line, so a hit can be selected by ROLE (definition vs. consumer)
    /// instead of by line ORDER. Mirrors the sibling seat-pin census's `Site::text`.
    text: String,
}

/// CR-neutral source classification: which lines of `src` sit inside a
/// `#[cfg(test)]` scope?
///
/// THE CORRECTED RULE (the shipped `.combofb-cfgscope.sh` gets this wrong with a
/// bare `/^mod /`, which reports every hit inside a `#[cfg(test)] pub mod tests
/// {` as PRODUCTION):
///
/// * `#[cfg(test)]` immediately followed by an OPTIONAL VISIBILITY PREFIX and
///   then `mod ` (`mod` / `pub mod` / `pub(crate) mod` / `pub(super) mod`) opens
///   a module spanning to that `mod` line's own closing brace, at the `mod`'s
///   indentation.
/// * `#[cfg(test)]` followed by anything else scopes ONLY its own item.
///
/// The naive "nearest preceding attribute" rule is measured wrong and yields
/// false TEST verdicts, so it is deliberately not used.
pub(super) fn cfg_test_scoped_lines(src: &str) -> Vec<bool> {
    let lines: Vec<&str> = src.lines().collect();
    let mut scoped = vec![false; lines.len()];
    let mut i = 0usize;
    while i < lines.len() {
        if lines[i].trim() != "#[cfg(test)]" || i + 1 >= lines.len() {
            i += 1;
            continue;
        }
        let next = lines[i + 1];
        let indent = next.len() - next.trim_start().len();
        let closing = format!("{}}}", " ".repeat(indent));
        let body = next.trim_start();
        let after_vis = body
            .strip_prefix("pub(crate) ")
            .or_else(|| body.strip_prefix("pub(super) "))
            .or_else(|| body.strip_prefix("pub "))
            .unwrap_or(body);
        let opens_module = after_vis.starts_with("mod ");
        // A `#[cfg(test)]` item that opens a brace spans to its own closing
        // brace; one that does not (a `use`, a `const`) is a single line.
        if opens_module || next.trim_end().ends_with('{') {
            let mut j = i + 2;
            while j < lines.len() && lines[j].trim_end() != closing {
                j += 1;
            }
            for s in scoped.iter_mut().take((j + 1).min(lines.len())).skip(i) {
                *s = true;
            }
            i = j + 1;
            continue;
        }
        scoped[i + 1] = true;
        i += 1;
    }
    scoped
}

/// Classify every `needle` hit in `src`, skipping COMMENT lines.
///
/// ⚠ THE COMMENT EXCLUSION IS A MEASURED DEVIATION FROM THE PLAN, DISCLOSED
/// HERE RATHER THAN ABSORBED. §6 R8's ROUND-7 pre-change-tree check asserts that
/// U1–U6 introduce no `WaitingFor::LoopShortcut {` token. Measured on this tree:
/// 5d U2's declare-time owner firewall added the DOC LINE
/// `// copied from `WaitingFor::LoopShortcut { proposer }`.` to `game/engine.rs`,
/// which a comment-blind bare anchor counts as one MORE production site. A comment
/// is not a code surface — it writes no offer and consumes none — so counting it
/// would make the tripwire fire on prose and would force the pinned number to be
/// re-measured by the very commit that ships the row. Excluding comment lines
/// restores the plan's PRODUCTION count exactly, INCLUDING its per-file
/// production multiset. (It does not restore the plan's original test-half count
/// of 12: that half has since been adjudicated repeatedly, and the assert
/// below is the authority for the pair. Prose that repeats a number is prose that
/// can go stale — this defers to the assert rather than restating it.)
/// THE COMMENT RULE IS THE CODE HALF OF THE LINE, not the whole line. Rejecting only lines that
/// OPEN with `//` left a needle sitting AFTER a trailing `//` counted as a writer, which breaks
/// the exclusion in both directions: a pure prose edit moves the pinned number with no code
/// change, and deleting a real writer while naming the same spelling in a trailing comment on a
/// surviving line HOLDS the number — the substitution class this census exists to catch.
/// [`super::source_census::code`] is the one home of that rule, shared by every census in
/// fail-CLOSED (a `//` preceded by a `"` on the same line is left in the code half, so a URL in
/// a string literal cannot hide a real writer behind it).
fn classify(src: &str, needle: &str, file: &str) -> Vec<Hit> {
    let scoped = cfg_test_scoped_lines(src);
    src.lines()
        .enumerate()
        .filter(|(_, line)| code(line).contains(needle))
        .map(|(n, line)| Hit {
            file: file.to_string(),
            line: n + 1,
            in_test: scoped[n],
            text: line.trim().to_string(),
        })
        .collect()
}

pub(super) fn rs_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}")) {
            let path = entry.expect("read dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The two crate roots R8 walks. `crates/engine/tests/**` is deliberately NOT
/// walked: the acceptance rows that name the variant live there, and they are
/// consumers of the surface rather than members of it.
fn census(needle: &str) -> Vec<Hit> {
    let engine_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let ai_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("phase-ai")
        .join("src");
    let mut hits = Vec::new();
    for (root, prefix) in [(engine_src, "engine/src"), (ai_src, "phase-ai/src")] {
        for path in rs_files(&root) {
            let src =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
            // Stable, checkout-independent label: `<crate>/src/...`. Built from the
            // walk root rather than from the absolute path, because the phase-ai
            // root is reached through `../` and would otherwise label as
            // `engine/../phase-ai/src/...`.
            let rel = path
                .strip_prefix(&root)
                .expect("walked path is under its root")
                .to_string_lossy()
                .replace('\\', "/");
            hits.extend(classify(&src, needle, &format!("{prefix}/{rel}")));
        }
    }
    hits
}

/// R8 CONJUNCT 1 — the offer-writer surface, pinned BIDIRECTIONALLY (an EQUALITY
/// on each half, so a REMOVED site fails too) and by per-file multiset.
///
/// ⚠ THE `#[cfg(test)]` HALF HAS MOVED REPEATEDLY — 12 ⇒ 13 ⇒ 14 ⇒ 16 ⇒ 17 ⇒ the
/// value the assert below pins — AND EACH ADJUDICATION IS RECORDED RATHER THAN
/// THE ASSERT RELAXED.
/// * 12 ⇒ 13: §6 R27 (b)
///   (`analysis::resource::tests::r27_b_a_stored_may_auto_choice_survives_the_ring`)
///   destructures the offer the mint RETURNED to count its published CR 603.5
///   `MayChoice` points.
/// * 13 ⇒ 14 (5d U4): `game::engine::stage2_injector_tests::u4_park_on_offer`
///   parks a constructed board on a `LoopShortcut { proposer: P0 }` so §6 R28's
///   arm (b) can assert that the DECLARE firewall refuses a hostile
///   `template.owner` — i.e. that arm (b)'s drive-seam configuration is
///   production-unreachable.
/// * 14 ⇒ 16: both in `phase-ai/src/policies/loop_shortcut.rs`'s `#[cfg(test)]`
///   module — `bounded_offer_with_period`, a builder minting an offer whose
///   certificate carries a real `per_cycle` so the proposer-elimination arm can
///   be driven, and `certificate_of`, a READ accessor for the same rows.
/// * 16 ⇒ 17: the cap-round row
///   `the_bounded_offer_charges_a_forced_victim_it_publishes_no_point_for` in
///   `engine/src/analysis/resource.rs` — A READ, NOT A WRITER: it destructures
///   the offer it minted to assert an EMPTY `schema.points` beside a
///   `victim_slot` that still names the forced victim.
/// * 17 ⇒ the pinned value: the CR 732.2a declaration change's two in-crate
///   rows spell the anchor FOUR
///   times between them — `game/visibility.rs` row D5-h's mint and its projection
///   read, and `ai_support/candidates.rs` row D6-n's mint and its reach-guard read.
///
/// All five are in a `#[cfg(test)]` scope — mints and reads both — which is the
/// benign case this row's own failure message names: a test fixture cannot make
/// the period machinery certify. The PRODUCTION half is UNCHANGED across all five
/// — and so is the per-file multiset below, which is the half §10 ruling condition
/// (2) is about. Its VALUE is the assert's, not this comment's.
///
/// ⚠ THE PRODUCTION HALF HAS BEEN ADJUDICATED TOO — `game/derived_views.rs`'s
/// `derive_views` arm, on the ground the failure message already excludes
/// `filter_state_for_viewer` on: a match PATTERN on `state.waiting_for` inside a
/// projection taking `&GameState`, minting no offer, unreachable unless a window
/// is already open, in a file holding no declare-time authority call. A benign
/// READ, so the assert is adjudicated and not relaxed. The assert below is where
/// the pair is authoritative.
///
/// R8 CONJUNCT 2, same test — pin VALUE-legality has exactly ONE production
/// consumer (`analysis::decision_template::declaration_conforms`), that consumer
/// also runs `predictability_gate`'s COVERAGE half, and every production site
/// that publishes or accepts a declaration routes through it. Superseded shape,
/// recorded at the assert: a per-call-site `validate_pins`/`predictability_gate`
/// pairing rule, which could not see a site that ran neither.
///
/// ON FAILURE, the named consequence (§10 ruling condition (2)): a new
/// production site in a certification-path file, or a declare site that does not
/// route through the shared authority, means the period machinery may have
/// created a path that CERTIFIES WITHOUT DECLARING OR DRIVING. That converts
/// answer-legality-at-certification from a doc note into owed work, and the
/// U-series stops until it is carried. Adjudication is a human step; this is not
/// a test to relax. A new *read* site is the benign case and the message says so.
#[test]
fn the_loop_shortcut_offer_writer_surface_is_pinned_and_every_declare_site_validates_pins() {
    let hits = census(&anchor());
    let production: Vec<&Hit> = hits.iter().filter(|h| !h.in_test).collect();
    let in_test: Vec<&Hit> = hits.iter().filter(|h| h.in_test).collect();

    let mut per_file: BTreeMap<&str, usize> = BTreeMap::new();
    for h in &production {
        *per_file.entry(h.file.as_str()).or_default() += 1;
    }
    let multiset: Vec<(String, usize)> = per_file
        .iter()
        .map(|(f, n)| ((*f).to_string(), *n))
        .collect();

    assert_eq!(
        (production.len(), in_test.len()),
        (23, 21),
        "CR 732.2a OFFER-WRITER SURFACE CHANGED (not re-measured — this number is an \
         INVARIANCE pin over the whole 5d U-series).\n\
         The three CERTIFICATION-PATH writers are `reconcile_terminal_result` (object-growth \
         arm), `interactive_loop_bridge` (drain bridge arm) and \
         `try_offer_bounded_cycle_shortcut` (bounded arm), all in `engine/src/game/engine.rs`. \
         `game/visibility.rs`'s `filter_state_for_viewer` writer is EXCLUDED by name and not \
         silently: it re-emits an ALREADY-minted offer into a per-viewer projection and cannot \
         run unless `state.waiting_for` is already a `LoopShortcut`. `game/derived_views.rs`'s \
         `derive_views` arm is excluded on the same ground, and is what moved the production \
         half: it is a match PATTERN on `state.waiting_for` inside a projection taking \
         `&GameState`, so it mints no offer and cannot run unless a `LoopShortcut` is already \
         open. That file holds no declare-time authority call.\n\
         A new PRODUCTION site in a certification-path file means the period machinery may \
         certify without declaring or driving — §10 ruling condition (2), i.e. \
         answer-legality-at-certification becomes OWED WORK and the U-series stops. A new READ \
         site is the benign case; adjudicate, do not relax the assert.\n\
         THE TEST HALF HAS BEEN ADJUDICATED FIVE TIMES (12 ⇒ 13, §6 R27 (b)'s schema read in \
         `engine/src/analysis/resource.rs`; 13 ⇒ 14, 5d U4's `u4_park_on_offer` fixture in \
         `engine/src/game/engine.rs`, which parks a constructed board on an offer so §6 R28 \
         arm (b) can assert the DECLARE firewall refuses a hostile `template.owner`; 14 ⇒ 16, \
         BOTH in `phase-ai/src/policies/loop_shortcut.rs`'s `#[cfg(test)]` module — \
         `bounded_offer_with_period`, a builder minting an offer whose certificate carries a \
         real `per_cycle` so the proposer-elimination arm can be driven, and `certificate_of`, \
         a read accessor for the same rows. PRODUCTION STAYED AT 22 across that change, which \
         is the half this pin exists to protect: the new policy arm READS the certificate and \
         writes no offer). FOURTH ADJUDICATION, 16 => 17: the cap-round row \
         `the_bounded_offer_charges_a_forced_victim_it_publishes_no_point_for` in \
         `engine/src/analysis/resource.rs`, whose `WaitingFor::LoopShortcut` DESTRUCTURE reads the \
         offer it minted to assert the combination that decoupling CR 732.2a publication from CR \
         704.5a charging makes reachable: `schema.points` EMPTY while `victim_slot` still names the \
         forced victim. A READ, not a writer. PRODUCTION STAYED AT 22 with an IDENTICAL per-file \
         multiset, and that conjunct is what makes this the benign case rather than a surface \
         change; if it moves again, name the new site here too rather than only moving the \
         number.\n\
         FIFTH ADJUDICATION, 17 => 21: publishing the bounded offer's own CR 732.2a \
         `declaration` on `WaitingFor::LoopShortcut` added two in-crate rows that spell the anchor \
         FOUR times between them. Named individually, because this census counts LINES (its \
         `classify()` is `line.contains(needle)`, deliberately replacing a construction-shaped \
         detector), so a mint and a read of the same fixture are two counted sites: (1) \
         `engine/src/game/visibility.rs` row D5-h's MINT, `d5h_offer` — one local helper called \
         twice, staging a declaration whose pins are all-seat vs. one naming a hidden object; \
         (2) `engine/src/game/visibility.rs` row D5-h's READ, `d5h_projected_declaration` — one \
         local helper destructuring `filter_state_for_viewer(..).waiting_for`, called three \
         times (hidden/viewer, hidden/proposer, public); (3) \
         `engine/src/ai_support/candidates.rs` row D6-n's MINT, `d6n_offer` — one local helper \
         called twice, `declaration: None` vs `Some(..)` and nothing else; (4) \
         `engine/src/ai_support/candidates.rs` row D6-n's READ — the reach-guard destructure \
         that asserts the staged offer really `is_bounded()` and really publishes a point \
         BEFORE the negative claim, without which that row would pass on the wrong conjunct. \
         Site (4) was NOT predicted (the plan budgeted 20 on the reading that D6-n's assertions \
         touch only `legal_actions`); the measurement wins and the site is named rather than the \
         row contorted to hit the budget. ALL FOUR ARE `#[cfg(test)]` FIXTURES: none writes an \
         offer the period machinery can certify. PRODUCTION STAYED AT 22 with an IDENTICAL \
         per-file multiset — C2b adds the field INSIDE existing literals and patterns and \
         introduces no new production anchor line.\n\
         measured per-file production multiset: {multiset:?}\n\
         production: {production:?}\n\
         test: {in_test:?}"
    );
    assert_eq!(
        multiset,
        vec![
            ("engine/src/ai_support/candidates.rs".to_string(), 1),
            ("engine/src/game/derived_views.rs".to_string(), 1),
            ("engine/src/game/engine.rs".to_string(), 5),
            ("engine/src/game/interaction.rs".to_string(), 5),
            ("engine/src/game/scenario.rs".to_string(), 1),
            ("engine/src/game/visibility.rs".to_string(), 2),
            ("engine/src/types/game_state.rs".to_string(), 4),
            ("phase-ai/src/decision_kind.rs".to_string(), 1),
            ("phase-ai/src/policies/loop_shortcut.rs".to_string(), 1),
            ("phase-ai/src/projection.rs".to_string(), 1),
            ("phase-ai/src/search.rs".to_string(), 1),
        ],
        "the COUNT can be preserved by a move that relocates a writer into a \
         certification-path file, so the per-file multiset is pinned too"
    );

    // ── CONJUNCT 2: pin VALUE-legality has exactly ONE production consumer, and it
    //    is the one that also runs the COVERAGE half ──
    //
    // ⚠ THE SHAPE OF THIS CONJUNCT CHANGED, AND THE OLD ONE IS RECORDED RATHER
    // THAN OVERWRITTEN. It used to pin `validate_pins(` at 3 production sites (1
    // definition + 2 declare-time call sites) and assert each CALL SITE had a
    // `predictability_gate(` hit within two lines. That pairing rule was a
    // per-call-site *convention*: it could only catch a site that forgot the
    // coverage half, never a site that ran BOTH gates against a differently-derived
    // `required` list — and it could not see `build_bounded_declaration`, which
    // PUBLISHED a declaration while running NEITHER gate (the C2b review's finding
    // F1). The two gates are now composed once, in
    // `analysis::decision_template::declaration_conforms`, and the invariant this
    // conjunct pins is the stronger one: production validates pin values in exactly
    // ONE place, that place runs both halves, and every declare-time site routes
    // through it. A relapse — a second `validate_pins(` consumer, or a declare site
    // that stops routing through the authority — fails the counts below.
    //
    // UNQUALIFIED anchors, deliberately: the fully-qualified
    // `crate::analysis::decision_template::validate_pins(` form matches only the
    // `engine.rs` site and under-counts by one — this plan's own finding 5,
    // applied symmetrically.
    let pins = census("validate_pins(");
    let pins_production: Vec<&Hit> = pins.iter().filter(|h| !h.in_test).collect();
    let pins_files: Vec<&str> = pins_production.iter().map(|h| h.file.as_str()).collect();
    assert_eq!(
        pins_files,
        vec![
            "engine/src/analysis/decision_template.rs",
            "engine/src/analysis/decision_template.rs"
        ],
        "expected `validate_pins(` to appear in production exactly twice, BOTH in \
         `analysis/decision_template.rs`: its own definition and its single consumer, \
         `declaration_conforms`. A hit in any other file is a declare-time site that \
         re-derives the pin firewall instead of routing through the shared authority — the \
         divergence C2b's F1 closed, where a PUBLISHER emitted a declaration under a weaker \
         predicate than the HANDLER accepts under. got {pins_production:?}"
    );

    // The one consumer runs the COVERAGE half too, asserted the way the old
    // per-call-site rule did: a `predictability_gate` hit within two lines.
    //
    // THE CONSUMER IS SELECTED BY ROLE, NOT BY LINE ORDER. `max_by_key(|h| h.line)` picked the
    // consumer only while the definition happened to sit ABOVE it; moving `pub fn validate_pins`
    // below `declaration_conforms` — a legal refactor this census has no business objecting to —
    // silently made the coverage assertion below check the DEFINITION line instead, i.e. measure
    // the wrong thing and report an unrelated failure. The definition is the hit whose text
    // declares the fn; the consumer is the other one, and both counts are asserted so a shape
    // this rule cannot classify fails loudly instead of defaulting.
    let gates = census("predictability_gate(");
    let (definitions, consumers): (Vec<&&Hit>, Vec<&&Hit>) = pins_production
        .iter()
        .partition(|h| h.text.contains("fn validate_pins("));
    assert_eq!(
        (definitions.len(), consumers.len()),
        (1, 1),
        "the two production `validate_pins(` hits must split by ROLE into exactly one \
         DEFINITION (`fn validate_pins(`) and exactly one CONSUMER. Anything else means the \
         role rule stopped classifying this surface and the coverage assertion below would be \
         measuring an unknown hit. definitions: {definitions:?}; consumers: {consumers:?}"
    );
    let consumer = consumers[0];
    assert!(
        gates
            .iter()
            .any(|g| g.file == consumer.file && g.line.abs_diff(consumer.line) <= 2),
        "CR 732.2a: validating pin VALUES without also running `predictability_gate`'s \
         COVERAGE check can accept a proposal that leaves a published choice unpinned — the \
         certifies-without-declaring shape §10 condition (2) names. Unpaired: {consumer:?}; \
         gates: {gates:?}"
    );

    // Every production site that asks "is this declaration legal?" — the declare
    // handler, the human ingress, and the bounded PUBLISHER — routes through the
    // authority. The publisher is the site F1 added: it is what makes
    // `declaration.is_some()`, the predicate `ai_support::candidates` gates its
    // `DeclareShortcut` candidate on, mean "the handler will accept this".
    let authority = census("declaration_conforms(");
    let authority_production: Vec<&Hit> = authority.iter().filter(|h| !h.in_test).collect();
    let mut authority_per_file: BTreeMap<&str, usize> = BTreeMap::new();
    for h in &authority_production {
        *authority_per_file.entry(h.file.as_str()).or_default() += 1;
    }
    assert_eq!(
        authority_per_file.into_iter().collect::<Vec<_>>(),
        vec![
            ("engine/src/analysis/decision_template.rs", 1),
            ("engine/src/game/engine.rs", 2),
            ("engine/src/game/interaction.rs", 1),
        ],
        "expected 1 definition + 3 call sites: \
         `game/engine.rs::handle_declare_shortcut` (accept), \
         `game/engine.rs::build_bounded_declaration` (publish), \
         `game/interaction.rs::materialize_loop_shortcut_response` (human emit). A MISSING \
         call site is a path that publishes or accepts a declaration under its own predicate; \
         a NEW one is benign but must be named here rather than absorbed. \
         got {authority_production:?}"
    );
}

/// R8 ANTI-VACUITY ARM 2 — THE FOREIGN-FORM PLANT.
///
/// Feeds the classifier a synthetic source carrying the anchor in FOUR forms the
/// round-2 construction anchor could not match — the bare literal in expression
/// position (the `types/game_state.rs` shape, the one genuinely-missed site),
/// `Some(..)`, a match-arm PATTERN, and `return ..` — plus one `cfg(test)`
/// mod-scoped copy of each. `(production, test) == (4, 4)`.
///
/// THE PLANT IS DELIBERATELY NOT IN THE PLAN'S OWN ANCHOR FORM. A tripwire that
/// only detects its own shape is the defect this row files against the
/// superseded instrument, and planting in that shape would repeat it.
///
/// KEYED, not trusted: the round-2 construction anchors are run over the SAME
/// input and must score `(0, 0)` — the measured statement that the old tripwire
/// was evadable — while the bare anchor scores `(4, 4)`. One instrument
/// resolving two different values on one input is what makes this a measurement
/// rather than a constant.
///
/// REVERT-PROBE (arm 3, the ONLY remaining non-trivial conjunct under a bare
/// anchor): remove the cfg-scope filter — i.e. make `cfg_test_scoped_lines`
/// return all-`false` — and the four mod-scoped plants count as production, so
/// `(4, 4)` becomes `(8, 0)` and this test FLIPS TO FAIL. The classifier is also
/// measured keyed on the real tree: the census assert above resolves a production
/// half and a test half that differ from each other and from this test's `(4, 4)`,
/// so it is not constant in either direction. THE PAIR IS NOT RESTATED HERE — this
/// sentence used to restate it, the test half moved under adjudication, and nothing
/// went red. The assert above is where that pair is authoritative, and the PROBE-PIN
/// block below re-measures it on every `probe-pin check`.
#[test]
fn the_cfg_scope_classifier_sees_four_foreign_forms_the_construction_anchor_misses() {
    let bare = anchor();
    let forms = [
        // 1. bare literal in ARGUMENT position — `types/game_state.rs`'s shape,
        //    the one site the construction anchor genuinely missed.
        format!(
            "    cases.push((\"answered by DeclareShortcut\", {bare} proposer: PlayerId(0) }}));"
        ),
        // 2. `Some(..)`.
        format!("    let offer = Some({bare} proposer, schema }});"),
        // 3. a match-arm PATTERN — a CONSUMER of the surface.
        format!("        {bare} proposer, .. }} => *proposer,"),
        // 4. `return ..`.
        format!("    return {bare} proposer, schema, certificate, predicted_winner }};"),
    ];
    let mut src = String::from("fn production_side() {\n");
    for f in &forms {
        src.push_str(f);
        src.push('\n');
    }
    src.push_str("}\n\n#[cfg(test)]\npub(crate) mod tests {\n    fn test_side() {\n");
    for f in &forms {
        // Same four forms, one indent deeper, inside a `pub(crate) mod` — the
        // visibility prefix the superseded shell classifier's `/^mod /` misses.
        src.push_str("    ");
        src.push_str(f);
        src.push('\n');
    }
    src.push_str("    }\n}\n");

    let hits = classify(&src, &bare, "synthetic");
    let production = hits.iter().filter(|h| !h.in_test).count();
    let in_test = hits.iter().filter(|h| h.in_test).count();
    assert_eq!(
        (production, in_test),
        (4, 4),
        "the bare anchor must see all four foreign forms on BOTH sides of the cfg scope, and \
         the cfg-scope classifier must put the `pub(crate) mod tests` copies in the TEST \
         column. Removing the cfg-scope filter makes this (8, 0). hits: {hits:?}\nsrc:\n{src}"
    );

    for old in construction_anchors() {
        let old_hits = classify(&src, &old, "synthetic");
        assert_eq!(
            old_hits.len(),
            0,
            "keying control: the ROUND-2 construction anchor `{old}` scores 0 on input the \
             bare anchor scores 8 on — that is the measured statement that the superseded \
             tripwire was evadable, and it is what makes the (4, 4) above a measurement \
             rather than a constant. hits: {old_hits:?}"
        );
    }
}
