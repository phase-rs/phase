//! **Row R2-b — the PROVENANCE census.** CR 601.2c vs CR 115.10a: a seat pin's SPELLING is its
//! provenance, so no TARGET-class producer may construct a `TargetPin::Player`.
//!
//! # Why a census and not a validator narrowing
//!
//! The alternative — teaching `validate_pins` / `declaration_conforms` to REJECT a
//! `TargetPin::Player` on a player-valued `Targets` slot — is refused for a measured reason.
//! `game::engine`'s `a_shrouded_player_pin_is_still_published_by_the_offer_builder` carries the
//! revert-probe *"route `resolve_target`'s `TargetPin::Player` arm through
//! `targeting::player_is_legal_target` ⇒ both assertions FAIL"*: that narrowing restores the
//! CR 115.10a over-veto, where a merely CHOSEN player (a CR 701.34a proliferate choice) is
//! refused for a targeting-only exclusion it is not subject to. It would also put a rejection
//! rule on a WIRE-VISIBLE type in order to stop a FUTURE producer from picking the wrong
//! spelling. A source census stops the same thing at no wire-compatibility cost, which is what
//! this file is.
//!
//! # What it asserts, in three conjuncts
//!
//! 1. **Neither TARGET-class producer constructs a `TargetPin::Player`.** The two producers are
//!    named individually — `game::engine::record_trigger_target_answer` (the engine's own
//!    CR 601.2c announcement journal) and
//!    `game::interaction::materialize_loop_shortcut_response` (the human ingress of the same
//!    point kind). Naming them individually is the point: a census asserting only a TOTAL
//!    production count would stay green with EITHER producer reverted, since a revert removes
//!    a ranked construction and adds a `TargetPin::Player` one, leaving the total unchanged.
//! 2. **The CHOICE-class and pass-through sites are still PRESENT and classified.** A census
//!    that counts zero of everything is vacuous, so every surviving production site is
//!    enumerated with its disposition and the list is pinned exactly.
//! 3. **Both producers do construct the ranked spelling**, keyed to the same instrument — one
//!    census returning a non-zero answer for one needle and a zero for another, on the same
//!    files, is what makes conjunct 1's zero a measurement rather than a broken grep.
//!
//! # Anti-vacuity: the instrument is validated before it is trusted
//!
//! [`the_seat_pin_census_instrument_reports_both_answers_on_planted_input`] feeds the classifier
//! a synthetic source carrying BOTH spellings inside and outside a `#[cfg(test)]` scope and
//! requires it to separate them. Without that arm, a needle that silently matched nothing would
//! report conjunct 1 as a pass.
//!
//! The `#[cfg(test)]` scope classifier and the directory walk are REUSED from
//! [`super::loop_shortcut_offer_writer_census`] rather than re-derived: that file records a
//! measured defect in the naive "nearest preceding attribute" rule, and a second copy of the
//! rule is a second place for it to be got wrong.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::loop_shortcut_offer_writer_census::{cfg_test_scoped_lines, rs_files};
use super::source_census::code;

/// The CHOICE-class needle, ASSEMBLED AT RUNTIME for the same reason the sibling census
/// assembles its own: this file lives under `crates/engine/tests/`, which the walk does not
/// visit, but an instrument that would report its own text as a finding after a future move is
/// an instrument that lies about the surface it measures.
fn choice_needle() -> String {
    format!("{}::{}(", "TargetPin", "Player")
}

/// The TARGET-class needle — the subject kind that only ever reaches the resolver through a
/// `Ranking`, i.e. the spelling that IS the CR 601.2c provenance.
fn target_needle() -> String {
    format!("{}::{}(", "AnnouncementSubject", "Seat")
}

/// One classified construction/match site.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Site {
    file: String,
    line: usize,
    text: String,
}

/// Non-comment, production-scope OCCURRENCES of `needle` in `src`, one [`Site`] each.
///
/// THE SINGLE HOME OF THIS CENSUS'S MATCHING RULE. Source-parameterized, so the anti-vacuity
/// and matched-pair rows below exercise THE SHIPPED RULE rather than a copy of it — a copy
/// validates a rule the file no longer ships. Mirrors
/// [`super::loop_shortcut_offer_writer_census`]'s `classify(src, needle, file)` shape, and
/// computes its own `#[cfg(test)]` scope map for the same reason that one does: a
/// caller-supplied map is a second thing a call site can get wrong.
///
/// OCCURRENCE granularity, not line granularity: `line.matches(needle).count()` (the `std`
/// building block for non-overlapping matches) emits one `Site` per construction, so two
/// constructions co-located on one line are two counted sites. The boolean rule this replaced
/// counted them once — measured on this tree, 146 non-comment lines already carry two or more
/// occurrences of the same `Enum::Variant(` spelling, including the
/// `TargetRef::Object(id) => Some(TargetRef::Object(*id))` match-arm-plus-construction shape a
/// producer revert would take.
///
/// COMMENT TEXT IS EXCLUDED — whole-line AND trailing, which is the half this rule got wrong.
/// Prose writes no pin and reads none, so a doc mentioning a spelling is not a construction site;
/// the doc surface is swept separately (the commit's per-property bucket table). Rejecting only
/// lines that OPEN with `//` still counted every occurrence after a trailing `//`, and that fails
/// in BOTH directions: a pure prose edit moves the pinned multiset with no code change, and
/// deleting one construction while naming the same spelling in a trailing comment on a surviving
/// construction line HOLDS the count — the substitution class conjunct 1 exists to catch.
/// Occurrences are therefore counted in the line's CODE half only, via
/// [`super::source_census::code`] (the one home of the rule, shared
/// with both sibling censuses). It is fail-CLOSED: a `//` preceded by a `"` on the same line —
/// `let u = "http://x"; ..` — stays in the code half, so a URL in a string literal cannot hide a
/// real construction behind it, which a naive `split("//")` would.
///
/// MEASURED at the time of the fix: no line in any walked root carries a needle after a trailing
/// `//`, so this repaired NO pinned number. The repo-scanning rows are therefore blind to it and
/// [`the_seat_pin_census_ignores_a_trailing_comment_mention`] — synthetic input, both directions
/// — is the only thing that measures the rule.
fn sites_in_source(src: &str, needle: &str, file: &str) -> Vec<Site> {
    let scoped = cfg_test_scoped_lines(src);
    let mut out = Vec::new();
    for (n, line) in src.lines().enumerate() {
        if scoped[n] {
            continue;
        }
        for _ in 0..code(line).matches(needle).count() {
            out.push(Site {
                file: file.to_string(),
                line: n + 1,
                text: line.trim().to_string(),
            });
        }
    }
    out.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    out
}

/// The LINE count the pre-S279 boolean rule produced, derived from the SAME `Vec<Site>`.
///
/// NOT a second copy of the rule: [`sites_in_source`] emits one `Site` per OCCURRENCE, so the
/// distinct `(file, line)` keys are exactly the lines `line.contains(needle)` would have
/// counted — provably equal to the old rule without shipping the old rule. The key is
/// `(file, line)` and not `line`, so two files sharing a line number cannot collapse.
fn distinct_lines(sites: &[Site]) -> usize {
    sites
        .iter()
        .map(|s| (s.file.as_str(), s.line))
        .collect::<BTreeSet<_>>()
        .len()
}

/// Production-scope OCCURRENCES of `needle` across the three walked crate roots — the walk,
/// with the matching rule delegated to [`sites_in_source`].
fn production_sites(needle: &str) -> Vec<Site> {
    let engine_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let server_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("server-core")
        .join("src");
    let ai_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("phase-ai")
        .join("src");
    let mut out = Vec::new();
    for (root, prefix) in [
        (engine_src, "engine/src"),
        (server_src, "server-core/src"),
        (ai_src, "phase-ai/src"),
    ] {
        for path in rs_files(&root) {
            let src =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
            let rel = path
                .strip_prefix(&root)
                .expect("walked path is under its root")
                .to_string_lossy()
                .replace('\\', "/");
            out.extend(sites_in_source(&src, needle, &format!("{prefix}/{rel}")));
        }
    }
    out.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    out
}

/// CONJUNCT 1 + 2 — neither TARGET-class producer constructs a `TargetPin::Player`, and every
/// surviving production site is CHOICE class or a pass-through arm.
///
/// # Discrimination — TWO independent revert-probes, one per producer
///
/// * restore `game::engine::record_trigger_target_answer`'s `TargetRef::Player(pl)` arm to
///   `Some(TargetPin::Player(*pl))` ⇒ `engine/src/game/engine.rs` gains an unclassified
///   `TargetPin::Player` construction ⇒ the pinned site list below FAILS with its file:line;
/// * restore `game::interaction::materialize_loop_shortcut_response`'s
///   `Target(TargetRef::Player(player))` arm to `Ok(TargetPin::Player(*player))` ⇒
///   `engine/src/game/interaction.rs` re-appears in the list ⇒ FAILS.
///
/// A census asserting only a TOTAL would catch NEITHER: each revert swaps one ranked
/// construction for one `TargetPin::Player` construction and leaves the sum where it was.
///
/// # Paired positive — the CHOICE class must still be THERE
///
/// The surviving sites are asserted PRESENT, not merely bounded: the CR 701.34a proliferate arm
/// in `engine.rs`, `resolve_target`'s CHOICE authority arm and the drive-period arm, the
/// redaction arm in `visibility.rs`, and the wire-bound arm in `server-core`. A census that
/// counted zero of everything — a broken needle, a walk that visited nothing — would satisfy
/// conjunct 1 alone.
#[test]
fn no_target_class_producer_constructs_a_choice_class_player_pin() {
    let sites = production_sites(&choice_needle());
    let files: Vec<&str> = sites.iter().map(|s| s.file.as_str()).collect();

    // CONJUNCT 1: what is compared is the FILE MULTISET — file identity AND multiplicity, in
    // sorted order. Line numbers are not compared against any pinned value; they order the sites
    // (`production_sites` sorts by `(file, line)`) and appear in the failure message. That is
    // deliberate: a line pin fails on any insertion above a site, which is drift burden without
    // discrimination — measured: a 200-line insertion above every site leaves this census green.
    // Multiplicity carries the load instead — a new construction anywhere THE WALK VISITS in
    // production scope changes the compared value, and multiplicity is counted per OCCURRENCE,
    // not per line, so a construction co-located with an existing one on the same line still
    // moves the compared value. That includes a THIRD `engine.rs` hit from
    // reverting `record_trigger_target_answer`, and `interaction.rs` is pinned by ABSENCE,
    // which no rearrangement inside that file can satisfy. The walk visits three roots only
    // (`engine/src`, `server-core/src`, `phase-ai/src`), so a construction added in a crate
    // outside them — `engine-wasm`, `seat-reducer` and `phase-server` all depend on the engine —
    // is invisible here. That gap is latent, not live: today `TargetPin` appears in no crate
    // other than `engine` and `server-core`, and both of their `src` roots are walked.
    //
    // The doubled `engine.rs` entry is guarded by MULTIPLICITY, not by the text pins below: a
    // third construction in that file fails THIS assertion, before the text pins are reached —
    // including one written onto a line that already carries a construction, since the count is
    // per OCCURRENCE.
    // Do NOT relax `files` to a de-duplicated set on the theory that the text pins cover the
    // doubling — they are a different layer, and layer 3 below exists because a change can pass
    // both of them. Three measured layers, in the order they fire:
    //   1. this multiset — a THIRD `engine.rs` construction, and, via ABSENCE, either producer
    //      reverted;
    //   2. the text pins below — a SUBSTITUTION at either arm that holds the count at 2, and a
    //      change in the two arms' relative ORDER (relocating arm 0 below arm 1 fails the first
    //      text pin with the count and both texts otherwise unchanged);
    //   3. conjunct 3's per-file `AnnouncementSubject::Seat` count — a substitution that ALSO
    //      preserves both texts (producer reverted AND the positive control dropped) passes both
    //      text pins and is caught only there.
    //
    // LIMITATION: the three singly-listed files (`decision_template.rs`, `visibility.rs`,
    // `server-core/...`) are pinned by file identity and count alone. Swapping one of their
    // constructions for a DIFFERENT one in the same file leaves this census green. Those are
    // not the producers this row is about, and none of them is text-pinned.
    assert_eq!(
        files,
        vec![
            "engine/src/analysis/decision_template.rs",
            "engine/src/game/engine.rs",
            "engine/src/game/engine.rs",
            "engine/src/game/visibility.rs",
            "server-core/src/game_action_payload_guard.rs",
        ],
        "CR 601.2c / CR 115.10a PROVENANCE SPLIT VIOLATED, or a new unclassified \
         `TargetPin::Player` production site appeared.\n\
         The FIVE surviving production sites and their dispositions:\n\
         1. `analysis/decision_template.rs` — `resolve_target`'s CHOICE authority arm \
            (CR 115.10a existence-only). This IS the class's authority; do not narrow it, \
            `a_shrouded_player_pin_is_still_published_by_the_offer_builder` is the shipped \
            guard against exactly that.\n\
         2. `game/engine.rs` — `shortcut_drive_period`'s period arm; the migrated pin lands on \
            the `Scheduled(Constant(_))` arm of the SAME match, which also returns 1.\n\
         3. `game/engine.rs` — `apply_action`'s CR 701.34a proliferate arm feeding \
            `record_loop_pin`. A proliferate choice is NOT a target, so this construction is \
            correct and is this census's positive control.\n\
         4. `game/visibility.rs` — `pins_name_hidden_source`'s redaction arm (`false`: seat \
            identity is public in this engine), kept for wire-sourced pins.\n\
         5. `server-core/src/game_action_payload_guard.rs` — the wire pass-through arm.\n\
         A hit in `game/interaction.rs` means the HUMAN ingress reverted to the choice-class \
         spelling; a THIRD hit in `game/engine.rs` means the announcement journal did. Either \
         re-creates two authorities selected by WHO SUBMITTED an answer rather than by WHAT IT \
         IS. got {sites:?}"
    );

    // The `engine.rs` pair is pinned to its two ARMS by text: a SUBSTITUTION at either arm that
    // holds the file count at 2 fails here, as does relocating the arms relative to each other.
    // An ADDED construction in this file does not reach here — the multiset above fails first,
    // and it fails per OCCURRENCE, so co-locating the addition on an existing hit line does not
    // slip past it either.
    let engine_texts: Vec<&str> = sites
        .iter()
        .filter(|s| s.file == "engine/src/game/engine.rs")
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(
        engine_texts.len(),
        2,
        "the file-level list above is pinned at two `engine.rs` sites; if that changes the \
         text pin below is measuring the wrong things. got {engine_texts:?}"
    );
    assert!(
        engine_texts[0].starts_with("| TargetPin::Player(_) =>"),
        "site 2 must remain `shortcut_drive_period`'s MATCH arm — a match arm reads a pin, it \
         cannot produce one. got {:?}",
        engine_texts[0]
    );
    assert!(
        engine_texts[1].contains("TargetPin::Player(*pl)"),
        "site 3 must remain the CR 701.34a proliferate CONSTRUCTION — this census's positive \
         control, and the one production construction of the choice class. got {:?}",
        engine_texts[1]
    );

    // CONJUNCT 3, keyed to the SAME instrument: both TARGET-class producers construct the
    // ranked spelling. One needle returning five sites and the other returning the two
    // producers, over the same walk, is what makes conjunct 1's absence a measurement.
    let ranked = production_sites(&target_needle());
    let mut ranked_per_file: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &ranked {
        *ranked_per_file.entry(s.file.as_str()).or_default() += 1;
    }
    let ranked_files: Vec<(&str, usize)> = ranked_per_file.into_iter().collect();
    assert_eq!(
        ranked_files,
        vec![
            ("engine/src/analysis/decision_template.rs", 1),
            ("engine/src/game/engine.rs", 1),
            ("engine/src/game/interaction.rs", 1),
            ("engine/src/game/visibility.rs", 1),
        ],
        "the TARGET-class spelling must be CONSTRUCTED by BOTH producers — `game/engine.rs` \
         (`record_trigger_target_answer`) and `game/interaction.rs` \
         (`materialize_loop_shortcut_response`) — beside its two READ sites: \
         `evaluate_schedule`'s CR 601.2c resolver arm in `analysis/decision_template.rs` and \
         the wildcard-free redaction arm in `game/visibility.rs`. A MISSING producer is the \
         revert this row exists to catch; an EXTRA file is a new producer that must be \
         classified rather than absorbed. got {ranked:?}"
    );
    assert!(
        !sites.is_empty(),
        "keying control: the choice-class needle must return a NON-EMPTY set, or its \
         per-producer absence above would be the answer a dead instrument gives"
    );
}

/// ANTI-VACUITY ARM — the instrument returns BOTH answers on planted input.
///
/// The classifier is fed one synthetic source carrying the choice-class spelling and the
/// target-class spelling, each in production scope, inside a `#[cfg(test)] pub(crate) mod`, and
/// on a comment line. It must report exactly the production, non-comment hits.
///
/// # Discrimination
///
/// * delete the `!scoped[n]` conjunct ⇒ the mod-scoped plants count ⇒ `(1, 1)` becomes
///   `(2, 2)` ⇒ FAILS;
/// * delete the comment filter ⇒ the prose plants count ⇒ `(1, 1)` becomes `(2, 2)` ⇒ FAILS.
///
/// It is measured on the real tree too, in the row above: the same classifier returns five
/// sites for one needle and four files for the other, so it is not constant in either
/// direction.
#[test]
fn the_seat_pin_census_instrument_reports_both_answers_on_planted_input() {
    let choice = choice_needle();
    let target = target_needle();
    let src = format!(
        "fn production_side() {{\n\
         \x20   let a = {choice}PlayerId(0));\n\
         \x20   let b = Ranking::one({target}PlayerId(1)));\n\
         \x20   // prose about {choice}..) and {target}..) that constructs nothing\n\
         }}\n\
         \n\
         #[cfg(test)]\n\
         pub(crate) mod tests {{\n\
         \x20   fn test_side() {{\n\
         \x20       let a = {choice}PlayerId(0));\n\
         \x20       let b = Ranking::one({target}PlayerId(1)));\n\
         \x20   }}\n\
         }}\n"
    );

    // Routed through THE SHIPPED RULE, never a local re-implementation of it: an inline copy
    // here would validate a rule this file no longer ships, which is the drift the census
    // itself exists to catch.
    let count = |needle: &str| sites_in_source(&src, needle, "planted.rs").len();

    assert_eq!(
        (count(&choice), count(&target)),
        (1, 1),
        "the classifier must see exactly the ONE production, non-comment construction of each \
         spelling: the `#[cfg(test)] pub(crate) mod` copies belong in the test column and the \
         prose line is not a construction site. Dropping either filter makes this (2, 2).\n\
         src:\n{src}"
    );
}

/// TRAILING-COMMENT ARM — a needle mention after `//` on a REAL CODE LINE neither inflates the
/// count nor masks a deleted construction.
///
/// SYNTHETIC INPUT BY NECESSITY, not by preference. Measured when this rule was repaired: no line
/// in any of the three walked roots carries a needle after a trailing `//`, so every row that
/// scans the real tree passes byte-identically with the rule and without it. A repo-scanning
/// assertion for this property is vacuous BY CONSTRUCTION; only planted input can separate the
/// two rules. [`the_seat_pin_census_instrument_reports_both_answers_on_planted_input`] plants its
/// prose on its OWN line, which the whole-line filter already rejected, so it never reached this
/// path.
///
/// # The three arms, and what each one would be under the old rule
///
/// 1. SUBSTITUTION, the arm that matters and therefore the one asserted FIRST — the construction
///    is DELETED and the trailing-comment mention survives: 0, not 1. Under the old rule the
///    census counted a removed construction as still present, which is exactly the substitution
///    class conjunct 1 exists to catch. MEASURED by revert-probe: restoring
///    `line.matches(needle)` makes this arm fail `left: 1 / right: 0`.
/// 2. INFLATION — one real construction plus a mention of the same spelling in a trailing
///    comment on that same line: 1, not 2. Under the old rule a pure PROSE edit moved the pinned
///    multiset with no code change (revert-probe: `left: 2 / right: 1`).
/// 3. FAIL-CLOSED — a `//` inside a STRING LITERAL preceding a real construction: 1, not 0. A
///    naive `split("//")` truncates there and UNDER-counts, silently dropping a real site;
///    [`code`] leaves a `//` that follows a `"` in the code half, so the miss cannot happen.
#[test]
fn the_seat_pin_census_ignores_a_trailing_comment_mention() {
    let choice = choice_needle();
    let count = |src: &str| sites_in_source(src, &choice, "planted.rs").len();

    // FIRST, because it is the arm that matters: a revert-probe must show THIS one failing, not
    // merely the cheaper inflation arm that would short-circuit ahead of it.
    let substitution = format!("fn f() {{\n    let a = 0; // was {choice}PlayerId(0))\n}}\n");
    assert_eq!(
        count(&substitution),
        0,
        "THE SUBSTITUTION ARM: the construction is gone and only a trailing-comment mention \
         survives, so the count must fall to 0. Counting the whole line makes this 1 — a \
         DELETED producer reported as still present, which is the failure conjunct 1 exists to \
         catch.\nsrc:\n{substitution}"
    );

    let inflation =
        format!("fn f() {{\n    let a = {choice}PlayerId(0)); // also {choice}PlayerId(9))\n}}\n");
    assert_eq!(
        count(&inflation),
        1,
        "a needle in a TRAILING comment is prose: the code half of this line holds exactly ONE \
         construction. Counting the whole line makes this 2, and a pure prose edit then moves \
         the pinned multiset with no code change.\nsrc:\n{inflation}"
    );

    let in_string =
        format!("fn f() {{\n    let u = \"http://x\";\n    let a = {choice}PlayerId(0));\n}}\n");
    let in_string_one_line =
        format!("fn f() {{\n    let u = \"http://x\"; let a = {choice}PlayerId(0));\n}}\n");
    assert_eq!(
        (count(&in_string), count(&in_string_one_line)),
        (1, 1),
        "FAIL-CLOSED: a `//` inside a string literal is not a comment opener. A naive \
         `split(\"//\")` truncates at the URL and reports 0 for the one-line form — a real \
         construction silently dropped, the one direction a census must never fail in.\n\
         src:\n{in_string_one_line}"
    );
}

/// S279 INSTRUMENT — the census counts OCCURRENCES, and the two rules are separated on input
/// that distinguishes them.
///
/// The pre-S279 rule was `line.contains(needle)`, a BOOLEAN: two constructions on one line
/// counted once. That is not a synthetic worry — measured over this census's own three walk
/// roots, 146 non-comment lines already carry two or more occurrences of the same
/// `Enum::Variant(` spelling, two of them in exactly the match-arm-plus-construction shape a
/// producer revert would take (`ability_utils.rs`'s `TargetRef::Object(id) =>
/// Some(TargetRef::Object(*id))` and `targeting.rs`'s `Player` sibling). `TargetPin::Player(`
/// reads at line/occurrence parity today BY ACCIDENT, not by property.
///
/// # The matched pair, and why the second number is a PROJECTION
///
/// Each arm asserts BOTH numbers: `sites_in_source(..).len()` (occurrences) and
/// [`distinct_lines`] (the lines the old boolean rule would have counted). `distinct_lines` is
/// derived from the SAME `Vec<Site>`, so the two numbers can diverge only because the DATA
/// differs — never because two instruments differ. A second inline implementation of the line
/// rule is the defect this row exists to remove, not a shortcut it may take.
///
/// # Discrimination — one arm flips, two do not
///
/// Revert `sites_in_source` to the boolean form (`if line.contains(needle) { push once }`) ⇒
/// SAME-LINE's occurrence count becomes 1 ⇒ THIS ROW FAILS, while BASE and DISTINCT-LINE stay
/// green. That asymmetry is what makes it a matched pair rather than a single-sided assertion.
///
/// # Wiring requirement
///
/// The arms call `sites_in_source` with their OWN source. They must never call
/// `production_sites`, which takes no source, reads the real tree, and would report the same
/// answer under both rules — measuring nothing.
#[test]
fn the_seat_pin_census_counts_occurrences_and_not_lines() {
    let needle = choice_needle();
    let base = format!("fn production() {{\n    let a = {needle}PlayerId(0));\n}}\n");
    // The SECOND construction is added TO THE EXISTING HIT LINE — the whole point of the arm.
    let same_line = format!(
        "fn production() {{\n    let a = match t {{ {needle}p) => {needle}*p), _ => x }};\n}}\n"
    );
    // The same second construction, on its OWN line.
    let distinct = format!(
        "fn production() {{\n    let a = {needle}PlayerId(0));\n    let b = \
         {needle}PlayerId(1));\n}}\n"
    );

    for (label, src, expected) in [
        ("BASE", &base, (1, 1)),
        ("SAME-LINE", &same_line, (2, 1)),
        ("DISTINCT-LINE", &distinct, (2, 2)),
    ] {
        let sites = sites_in_source(src, &needle, "synthetic.rs");
        assert_eq!(
            (sites.len(), distinct_lines(&sites)),
            expected,
            "arm {label}: (occurrences, lines) must be {expected:?}. BASE is the reach-guard — \
             both rules agree on the trivial case; SAME-LINE is the arm that FLIPS, and it \
             reads (1, 1) if `sites_in_source` is reverted to `line.contains(needle)`; \
             DISTINCT-LINE proves the flip is about CO-LOCATION and not about the second \
             construction existing. got {sites:?}\nsrc:\n{src}"
        );
    }
}
