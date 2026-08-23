#![cfg(test)]
//! THE SINGLE AUTHORITY ON "WHICH PART OF THIS LINE IS CODE", for every guard test in this
//! repository that counts needles in Rust source.
//!
//! # Why this file exists at all
//!
//! The engine carries a family of source censuses — tests that read `.rs` text and pin how many
//! times a construct is written, so that a producer added or deleted without adjudication is a
//! counted event. Every one of them had its OWN comment policy, and they disagreed: some rejected
//! a line only when the WHOLE line opened with `//`, one had no comment rule at all. Both shapes
//! fail in the direction that matters. A needle written after a trailing `//` was counted as a
//! real site, so:
//!
//! * a pure PROSE edit moved a pinned multiset with no code change (measured: a doc comment that
//!   quoted a census's own needle counted ITSELF, reading 39 against a pin of 38); and
//! * deleting a real construction while naming the same spelling in a trailing comment HELD the
//!   count — the substitution class those censuses exist to catch.
//!
//! The first repair of that 39-vs-38 failure edited the PROSE (dropping a brace from a
//! quotation) rather than the counter. That fixed one sentence and left the next author to
//! rediscover the bug. Seven counters each carrying a private comment policy IS the defect; this
//! module is the producer-level fix, and
//! [`tests::no_source_reading_file_carries_a_private_comment_policy`] is what stops an eighth
//! private policy from appearing silently.
//!
//! # Venue
//!
//! `src/` unit tests reach this as `crate::source_census`; the integration binary reaches THE
//! SAME FILE through a `#[path]` module declaration in `tests/integration/main.rs`. That is
//! deliberate, and it is why the module is `#![cfg(test)]` rather than `pub`: `cfg(test)` holds
//! in both venues (an integration target is built with `--test`), so one source file serves both
//! without shipping guard infrastructure in a release build. The repo's older answer to this
//! problem is `test_support.rs`'s "TWIN-SYNC: keep the fixture path here in lockstep with
//! `tests/integration/support.rs`" — two copies and a comment asking a human to keep them equal.
//! This module is the shape that does not need the comment.
//!
//! Each venue compiles its own instance, so [`tests`] runs in both. That is a feature, not
//! duplication to remove: the copy is validated where it is used.

/// The byte range of `line` that is CODE: a LEADING `/* … */` comment and a TRAILING `//` comment
/// are excluded.
///
/// Both exclusions only ever REMOVE text, and each is guarded so that it cannot remove code:
///
/// * The leading form fires only when the TRIMMED line starts with `/*`, and drops exactly up to
///   and including the first `*/` — so `/*count=*/ state.last_created_token_ids.push(id)` keeps
///   its call.
/// * The trailing form fires only when no `"` precedes the `//` in the remaining code — so
///   `let u = "http://x"; state.last_created_token_ids.push(id);` keeps its call. A naive
///   `split("//")` truncates at the URL and UNDER-counts, silently dropping a real site, which is
///   the one direction a census must never fail in.
///
/// Offsets rather than a substring, because a caller may need to index back into the ORIGINAL
/// line (`battlefield_entry_authority_census::classify_container_tails` resolves a call tail from
/// an absolute offset). [`code`] is the substring form and is what most callers want.
///
/// Whatever survives both guards — a `//` after a quote, a `/* … */` opened mid-line, a string
/// literal quoting the defect, or the interior lines of a multi-line block comment — is still
/// scanned, and would ADD a hit. That direction is fail-CLOSED: spurious red, never a missed
/// site. What is closed here are the two shapes a census's own prose is most likely to take.
pub(crate) fn code_span(line: &str) -> (usize, usize) {
    let trimmed = line.trim_start();
    let mut lo = 0usize;
    if trimmed.starts_with("/*") {
        let start = line.len() - trimmed.len();
        lo = match line[start..].find("*/") {
            Some(end) => start + end + 2,
            None => line.len(),
        };
    }
    let mut hi = line.len();
    if let Some(slash) = line[lo..].find("//") {
        if !line[lo..lo + slash].contains('"') {
            hi = lo + slash;
        }
    }
    (lo, hi)
}

/// The CODE half of one line — [`code_span`] applied. A whole-line comment yields `""`, so a
/// caller needs no separate `starts_with("//")` rule.
pub(crate) fn code(line: &str) -> &str {
    let (lo, hi) = code_span(line);
    &line[lo..hi]
}

/// A whole source text with every line's comment half removed, line structure preserved.
///
/// For the censuses that count over a REGION rather than per line. Derived from [`code`] so the
/// region form and the line form cannot drift apart.
pub(crate) fn code_lines(src: &str) -> String {
    src.lines().map(code).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::{code, code_lines, code_span};

    /// THE discrimination arm for the shared rule — one test on the authority, rather than a
    /// near-duplicate in each of its callers.
    ///
    /// SYNTHETIC INPUT BY NECESSITY, not by preference. Measured when this rule was extracted: no
    /// line in any walked root carried a needle after a trailing `//`, so every census that scans
    /// the real tree passes byte-identically with the rule and without it. A repo-scanning
    /// assertion for this property is vacuous BY CONSTRUCTION; only planted input separates the
    /// two rules.
    ///
    /// # The arms, and what each is under the whole-line-only rule this replaced
    ///
    /// 1. SUBSTITUTION, the arm that matters and therefore asserted FIRST — the construction is
    ///    DELETED and only a trailing-comment mention survives: 0, not 1. Under the old rule a
    ///    census counted a removed producer as still present.
    /// 2. INFLATION — a real construction plus a mention of the same spelling in a trailing
    ///    comment on that line: 1, not 2. Under the old rule a pure PROSE edit moved a pinned
    ///    multiset with no code change.
    /// 3. FAIL-CLOSED — a `//` inside a STRING LITERAL preceding a real construction: 1, not 0.
    ///    A naive `split("//")` truncates there and UNDER-counts.
    /// 4. Whole-line and leading-block comments, and the offsets contract [`code_span`] owes its
    ///    one offset-taking caller.
    #[test]
    fn the_code_half_rule_separates_a_trailing_comment_from_the_code_beside_it() {
        // Assembled, so this test's own source cannot be counted by a census that walks `src/`.
        let needle = format!("{}::{}(", "TargetPin", "Player");
        let count = |line: &str| code(line).matches(&needle).count();

        assert_eq!(
            count(&format!("    let a = 0; // was {needle}PlayerId(0))")),
            0,
            "SUBSTITUTION: the construction is gone and only a trailing-comment mention \
             survives, so the count must fall to 0. Counting the whole line makes this 1 — a \
             DELETED producer reported as still present."
        );
        assert_eq!(
            count(&format!(
                "    let a = {needle}PlayerId(0)); // also {needle}PlayerId(9))"
            )),
            1,
            "INFLATION: a needle in a trailing comment is prose; the code half holds exactly ONE \
             construction. Counting the whole line makes this 2."
        );
        assert_eq!(
            count(&format!(
                "    let u = \"http://x\"; let a = {needle}PlayerId(0));"
            )),
            1,
            "FAIL-CLOSED: a `//` inside a string literal is not a comment opener. A naive \
             `split(\"//\")` truncates at the URL and reports 0 — a real construction silently \
             dropped."
        );

        assert_eq!(
            count(&format!("    // {needle}PlayerId(0))")),
            0,
            "whole-line comment"
        );
        assert_eq!(
            count(&format!("    /// {needle}PlayerId(0))")),
            0,
            "doc comment"
        );
        assert_eq!(
            count(&format!("/*n=*/ let a = {needle}PlayerId(0));")),
            1,
            "a LEADING block comment is dropped up to `*/` and the code after it survives"
        );

        // The offsets contract: `code` is exactly the span, and the span indexes the ORIGINAL
        // line, which is what the one offset-taking caller relies on.
        let line = format!("    let a = {needle}PlayerId(0)); // tail");
        let (lo, hi) = code_span(&line);
        assert_eq!(&line[lo..hi], code(&line), "the substring form IS the span");
        assert!(hi < line.len(), "the trailing comment is outside the span");

        // The region form is the line form, applied line-wise.
        assert_eq!(
            code_lines(&format!("let a = 1; // {needle}\n// {needle}\nlet b = 2;")),
            "let a = 1; \n\nlet b = 2;",
            "code_lines preserves line structure and strips both comment shapes"
        );
    }

    /// THE PRODUCER GUARD — no file that reads Rust source may carry its own comment policy.
    ///
    /// Sweeping the seven known counters fixes seven instances and leaves the eighth author to
    /// re-invent the bug. This row is what makes the change a producer fix: a NEW file that reads
    /// `.rs` text and counts in it must either route through this module or be adjudicated into
    /// [`EXEMPT`] with a stated reason. Neither is possible to do silently.
    ///
    /// The population predicate is mechanical, not a judgement: a file READS RUST SOURCE if it
    /// `include_str!`s a `.rs` path, or walks for `.rs` files (`rs_files(` / an `== "rs"`
    /// extension test). Every such file must carry a path-qualified `source_census::` call or be
    /// EXEMPT.
    ///
    /// Both halves of the predicate read the file through [`code_lines`] — this census obeys its
    /// own rule, so a file that merely QUOTES `include_str!("x.rs")` or the marker in prose is
    /// neither pulled into the population nor credited with routing.
    ///
    /// # Discrimination
    ///
    /// Delete the `source_census::` import from any routed census ⇒ that file is neither routed
    /// nor exempt ⇒ this row reds naming it. Verified by revert-probe on
    /// `loop_shortcut_seat_pin_census.rs`.
    #[test]
    fn no_source_reading_file_carries_a_private_comment_policy() {
        /// Files that read Rust source but do NOT count needles in code, each with the measured
        /// reason. An entry here is an ADJUDICATION, not a suppression: it says "this file's
        /// matching is not the comment-blind counting class", and the reason has to survive a
        /// reader who disagrees.
        const EXEMPT: &[(&str, &str)] = &[
            (
                "src/bin/rules_audit.rs",
                "INVERSE POLARITY BY DESIGN: it scans ONLY comment lines, because CR annotations \
                 live in comments. Stripping comments would leave it nothing to read.",
            ),
            (
                "tests/integration/cr_annotations.rs",
                "Same inverse polarity: the asserted subject IS comment text (CR annotations).",
            ),
            (
                "tests/integration/loop_shortcut.rs",
                "Parses with `syn` rather than by substring, so comments are excluded by \
                 construction — the stronger instrument this module approximates.",
            ),
            (
                "src/game/ability_scan.rs",
                "SECOND WAVE, disclosed not routed: whole-FILE boolean signals \
                 (`src.contains(..)`), not per-line counts. Same comment-blind class, different \
                 shape; `code_lines` is the routing tool when it is adjudicated.",
            ),
            (
                "src/game/turns.rs",
                "SECOND WAVE, disclosed not routed: region slices of a source string.",
            ),
            (
                "src/parser/oracle_condition.rs",
                "SECOND WAVE, disclosed not routed: whole-file `body.contains(..)` family probe.",
            ),
            (
                "tests/integration/interaction_contract.rs",
                "SECOND WAVE, disclosed not routed: whole-file signature/arithmetic probes.",
            ),
            (
                "tests/integration/deterministic_game_state_serde.rs",
                "SECOND WAVE, disclosed not routed: parses serde ATTRIBUTE regions, not needles.",
            ),
            (
                "tests/integration/no_top_level_test_binaries.rs",
                "SECOND WAVE, disclosed not routed: collects `mod` registrations from main.rs.",
            ),
            (
                "tests/integration/waiting_for_actor_authority_census.rs",
                "Parses with `syn` rather than by substring, so comments are excluded by \
                 construction — the stronger instrument this module approximates.",
            ),
        ];

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let mut stack = vec![root.join("src"), root.join("tests/integration")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}")) {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        files.sort();
        assert!(files.len() > 100, "reach-guard: the walk found the crate");

        // Assembled so this file's own text cannot satisfy the predicate it defines.
        let walks_rs = [format!("rs_files{}", '('), format!("== {}rs{}", '"', '"')];
        // THE ROUTING MARKER IS THE PATH-QUALIFIED CALL, `source_census::`, NOT the bare module
        // name — measured, after the bare name gave a FALSE PASS. `source_census` is already an
        // unrelated engine identifier: `ability_rw.rs` declares a `SourceCensus` type and a
        // `source_census` field, and `triggers_ordering_parity_tests.rs` calls
        // `source_census_overlaps_filter`. None of those is a comment rule, and a counter added
        // to one of those files would have been accepted as routed on the strength of a
        // coincidence. The `::` form appears only in a use/call of THIS module.
        let marker = format!("source_census{}", "::");
        let mut unrouted: Vec<String> = Vec::new();
        let mut routed = 0usize;
        for path in &files {
            let rel = path
                .strip_prefix(root)
                .expect("under the crate root")
                .to_string_lossy()
                .replace('\\', "/");
            if rel == "src/source_census.rs" {
                continue;
            }
            let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
            let src = super::code_lines(&text);
            let reads_rust_source = src.contains(".rs\")") && src.contains("include_str!(")
                || walks_rs.iter().any(|w| src.contains(w.as_str()));
            if !reads_rust_source {
                continue;
            }
            if src.contains(marker.as_str()) {
                routed += 1;
            } else if !EXEMPT.iter().any(|(f, _)| *f == rel) {
                unrouted.push(rel);
            }
        }

        assert!(
            unrouted.is_empty(),
            "these files read Rust source but neither route through `source_census` nor carry an \
             adjudicated EXEMPT reason. A source census that brings its own comment policy is the \
             defect this module exists to remove: a needle written after a trailing `//` counts \
             as a real site, so prose alone can move a pinned number and a deleted producer can \
             hide behind a comment that names it. Route the file through `source_census::code` / \
             `code_lines`, or add it to EXEMPT with the reason it is not a counter.\n\
             unrouted: {unrouted:#?}"
        );
        assert!(
            routed >= 7,
            "reach-guard: the seven repaired counters must still be visible to this census as \
             ROUTED, or the predicate above stopped matching and the assertion is vacuous. \
             routed={routed}"
        );
    }
}
