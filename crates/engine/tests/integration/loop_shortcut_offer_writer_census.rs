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
//! 22 production + 14 test sites across `crates/engine/src` and
//! `crates/phase-ai/src`. A failure reads *"5d (or a successor) changed the
//! offer-writer surface"*, not *"someone re-measured"*.
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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
/// which a comment-blind bare anchor counts as a 23rd production site. A comment
/// is not a code surface — it writes no offer and consumes none — so counting it
/// would make the tripwire fire on prose and would force the pinned number to be
/// re-measured by the very commit that ships the row. Excluding comment lines
/// restores the plan's PRODUCTION count of 22 exactly, INCLUDING its per-file
/// production multiset. (It does not restore the plan's original test-half count
/// of 12: that half has since been adjudicated to 14, twice, and the assert below
/// is the authority for the pair. Prose that repeats a number is prose that can go
/// stale — this defers to the assert rather than restating it.)
fn classify(src: &str, needle: &str, file: &str) -> Vec<Hit> {
    let scoped = cfg_test_scoped_lines(src);
    src.lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle) && !line.trim_start().starts_with("//"))
        .map(|(n, _)| Hit {
            file: file.to_string(),
            line: n + 1,
            in_test: scoped[n],
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

/// R8 CONJUNCT 1 — the offer-writer surface, pinned BIDIRECTIONALLY (`== 22` /
/// `== 14`, so a REMOVED site fails too) and by per-file multiset.
///
/// ⚠ THE `#[cfg(test)]` HALF HAS MOVED TWICE, 12 ⇒ 13 ⇒ 14, AND EACH
/// ADJUDICATION IS RECORDED RATHER THAN THE ASSERT RELAXED.
/// * 12 ⇒ 13: §6 R27 (b)
///   (`analysis::resource::tests::r27_b_a_stored_may_auto_choice_survives_the_ring`)
///   destructures the offer the mint RETURNED to count its published CR 603.5
///   `MayChoice` points.
/// * 13 ⇒ 14 (5d U4): `game::engine::stage2_injector_tests::u4_park_on_offer`
///   parks a constructed board on a `LoopShortcut { proposer: P0 }` so §6 R28's
///   arm (b) can assert that the DECLARE firewall refuses a hostile
///   `template.owner` — i.e. that arm (b)'s drive-seam configuration is
///   production-unreachable.
///
/// Both are WRITES in a `#[cfg(test)]` scope, which is the benign case this
/// row's own failure message names: a test fixture cannot make the period
/// machinery certify. The PRODUCTION half is unchanged at 22 and so is the
/// per-file multiset below, which is the half §10 ruling condition (2) is about.
///
/// R8 CONJUNCT 2, same test — every production `validate_pins(` site is a
/// declare-time gate paired with `predictability_gate`.
///
/// ON FAILURE, the named consequence (§10 ruling condition (2)): a new
/// production site in a certification-path file, or a declare site without its
/// `validate_pins` pairing, means the period machinery may have created a path
/// that CERTIFIES WITHOUT DECLARING OR DRIVING. That converts
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
        (22, 16),
        "CR 732.2a OFFER-WRITER SURFACE CHANGED (not re-measured — this number is an \
         INVARIANCE pin over the whole 5d U-series).\n\
         The three CERTIFICATION-PATH writers are `reconcile_terminal_result` (object-growth \
         arm), `interactive_loop_bridge` (drain bridge arm) and \
         `try_offer_bounded_cycle_shortcut` (bounded arm), all in `engine/src/game/engine.rs`. \
         `game/visibility.rs`'s `filter_state_for_viewer` writer is EXCLUDED by name and not \
         silently: it re-emits an ALREADY-minted offer into a per-viewer projection and cannot \
         run unless `state.waiting_for` is already a `LoopShortcut`.\n\
         A new PRODUCTION site in a certification-path file means the period machinery may \
         certify without declaring or driving — §10 ruling condition (2), i.e. \
         answer-legality-at-certification becomes OWED WORK and the U-series stops. A new READ \
         site is the benign case; adjudicate, do not relax the assert.\n\
         THE TEST HALF HAS BEEN ADJUDICATED THREE TIMES (12 ⇒ 13, §6 R27 (b)'s schema read in \
         `engine/src/analysis/resource.rs`; 13 ⇒ 14, 5d U4's `u4_park_on_offer` fixture in \
         `engine/src/game/engine.rs`, which parks a constructed board on an offer so §6 R28 \
         arm (b) can assert the DECLARE firewall refuses a hostile `template.owner`; 14 ⇒ 16, \
         BOTH in `phase-ai/src/policies/loop_shortcut.rs`'s `#[cfg(test)]` module — \
         `bounded_offer_with_period`, a builder minting an offer whose certificate carries a \
         real `per_cycle` so the proposer-elimination arm can be driven, and `certificate_of`, \
         a read accessor for the same rows. PRODUCTION STAYED AT 22 across that change, which \
         is the half this pin exists to protect: the new policy arm READS the certificate and \
         writes no offer); if it moves again, name the new site here too rather than only \
         moving the number.\n\
         measured per-file production multiset: {multiset:?}\n\
         production: {production:?}\n\
         test: {in_test:?}"
    );
    assert_eq!(
        multiset,
        vec![
            ("engine/src/ai_support/candidates.rs".to_string(), 1),
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

    // ── CONJUNCT 2: every production `validate_pins(` site is a declare-time gate ──
    // UNQUALIFIED anchor, deliberately: the fully-qualified
    // `crate::analysis::decision_template::validate_pins(` form matches only the
    // `engine.rs` site and under-counts by one — this plan's own finding 5,
    // applied symmetrically.
    let pins = census("validate_pins(");
    let pins_production: Vec<&Hit> = pins.iter().filter(|h| !h.in_test).collect();
    assert_eq!(
        pins_production.len(),
        3,
        "expected 1 definition (`analysis/decision_template.rs`) + 2 declare-time call sites \
         (`game/engine.rs::handle_declare_shortcut`, \
         `game/interaction.rs::materialize_loop_shortcut_response`); got {pins_production:?}"
    );
    let definition = pins_production
        .iter()
        .filter(|h| h.file == "engine/src/analysis/decision_template.rs")
        .count();
    assert_eq!(definition, 1, "exactly one definition: {pins_production:?}");

    // Each CALL SITE is paired with `predictability_gate` — the coverage half of
    // the same declare-time gate. Pairing is asserted WITHIN the enclosing
    // statement, i.e. a `predictability_gate` hit within two lines of the call.
    let gates = census("predictability_gate(");
    for site in pins_production
        .iter()
        .filter(|h| h.file != "engine/src/analysis/decision_template.rs")
    {
        let paired = gates
            .iter()
            .any(|g| g.file == site.file && g.line.abs_diff(site.line) <= 2);
        assert!(
            paired,
            "CR 732.2a: a declare site that validates pin VALUES without also running \
             `predictability_gate`'s COVERAGE check can accept a proposal that leaves a \
             published choice unpinned — the certifies-without-declaring shape §10 condition \
             (2) names. Unpaired site: {site:?}; gates: {gates:?}"
        );
    }
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
/// measured keyed on the real tree: it returns BOTH 22 production AND 12 test
/// above, so it is not constant in either direction.
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
