//! Corpus harness for the infinite-combo detector (Engine A).
//!
//! This is the acceptance suite described in `.planning/combo-detection/`
//! `IMPLEMENTATION.md` §8: one data row per combo, plus a soundness set. The corpus
//! table and the bespoke driver toolkit live in the shared [`crate::analysis::corpus`]
//! module (parameterized on `&CardDatabase`) so the `combo-verify` CLI and these
//! tests drive ONE implementation; this module holds the `#[cfg(test)]` assertions
//! and the test-only synthetic-loop helpers.
//!
//! Three layers, by what each asserts.
//!
//! Driven end-to-end through the real pipeline (`drive_*` / `drive_combo_*`):
//! each is built from the real card-data export with the cards' actual parsed
//! abilities, its action cycle is driven through `apply()` via [`LoopProbe`], and
//! confirmed by [`detect_loop`] against the row's documented family + `WinKind`
//! (the driven set is `corpus::DRIVERS`). The two drain-feedback combos are driven
//! LIVE through the per-beat `apply(PassPriority)` reducer (the persisted
//! `loop_detect_ring` + the §3 reconcile shortcut). Two synthetic loops
//! (`drive_damage_loop_certificate` plus the negatives) exercise the same pipeline
//! without the export. Reverting either `detect_loop` gate flips an assertion.
//!
//! Corpus card-availability over all 53 rows
//! (`corpus_cards_present_and_implementation_status_matches_gating`): every card
//! present, and every non-gated combo fully modeled (no top-level `Unimplemented`).
//! Skips gracefully when the gitignored export is absent.
//!
//! Corpus table (`corpus::CORPUS`) + shape/partition-lock meta-tests: all 53 rows
//! partitioned into driven ∪ gated ∪ deferred. The `combo-verify` CLI
//! ([`corpus::drive_row`]) classifies each via the same drivers;
//! `drive_row_classifies_corpus_via_shared_pipeline` and the `classify_status`
//! revert-probe pin that dispatch.

use crate::analysis::corpus;
use crate::analysis::resource::{CounterClass, ObjectClass, ResourceAxis};
use crate::analysis::{detect_loop, BoardDelta, LoopCertificate, LoopProbe, WinKind};
use crate::database::CardDatabase;
use crate::game::derived_views::derive_views;
use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TargetRef,
};
use crate::types::actions::GameAction;
use crate::types::game_state::{CastPaymentMode, GameState, LoopDetectionMode, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaType;
use crate::types::match_config::MatchConfig;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;

/// The shared card database, loaded from the committed integration fixture
/// (or the full export via `FORGE_TEST_FULL_DB=1`).
fn card_db() -> &'static CardDatabase {
    crate::test_support::shared_card_db()
}

// ===========================================================================
// Meta-tests over the shared corpus table.
// ===========================================================================

/// META-TEST: lock the corpus shape so an accidental row deletion or miscount
/// fails loudly. 53 rows total (3 driving + 50 corpus), exactly 4 card-gated.
#[test]
fn corpus_table_shape_is_locked() {
    assert_eq!(
        corpus::corpus_len(),
        53,
        "corpus must hold all 3 driving + 50 combos"
    );
    let gated = (0..corpus::corpus_len())
        .filter(|&i| corpus::row(i).gated_on.is_some())
        .count();
    assert_eq!(
        gated, 4,
        "exactly 4 rows are card-gated (Doc Aurlock / Professor Onyx / Animate Dead / Grindstone)"
    );
    // Every row's expected axis must be derivable (total match over the enum).
    for i in 0..corpus::corpus_len() {
        let row = corpus::row(i);
        let _ = row.family.expected_axis();
        // A directed win family must classify as a loss condition, never Advantage.
        match row.win_kind {
            WinKind::LethalDamage
            | WinKind::PoisonLoss
            | WinKind::Decking
            | WinKind::ExtraTurns
            | WinKind::ImmediateWin
            | WinKind::Advantage => {}
        }
    }
    // 49 of 53 are testable today (gated count is the complement).
    let testable = corpus::corpus_len() - gated;
    assert_eq!(testable, 49, "49 corpus combos are testable once driven");
}

/// META-TEST: the corpus is a clean partition — every row is exactly one of
/// {driven, gated, deferred}, pairwise disjoint, covering all 53. The driven set
/// is `corpus::DRIVERS`, gated is `gated_on.is_some()`, deferred is
/// `deferral.is_some()`; a driven/gated row must NOT also carry a deferral bucket.
#[test]
fn corpus_partition_is_locked() {
    use std::collections::BTreeSet;
    let n = corpus::corpus_len();
    let driven: BTreeSet<usize> = corpus::DRIVERS.iter().map(|(i, _)| *i).collect();
    let gated: BTreeSet<usize> = (0..n)
        .filter(|&i| corpus::row(i).gated_on.is_some())
        .collect();
    let deferred: BTreeSet<usize> = (0..n)
        .filter(|&i| corpus::row(i).deferral.is_some())
        .collect();

    assert_eq!(driven.len(), 12, "12 driven rows");
    assert_eq!(gated.len(), 4, "4 gated rows");
    assert_eq!(deferred.len(), 37, "37 deferred rows");

    assert!(driven.is_disjoint(&gated), "driven ∩ gated must be empty");
    assert!(
        driven.is_disjoint(&deferred),
        "driven ∩ deferred must be empty"
    );
    assert!(
        gated.is_disjoint(&deferred),
        "gated ∩ deferred must be empty"
    );

    let mut union = BTreeSet::new();
    union.extend(driven.iter().copied());
    union.extend(gated.iter().copied());
    union.extend(deferred.iter().copied());
    assert_eq!(
        union.len(),
        n,
        "driven ∪ gated ∪ deferred must cover every one of the {n} rows"
    );
    assert_eq!(n, 53);

    // Exclusivity: a driven or gated row must not also declare a deferral bucket.
    for &i in driven.iter().chain(gated.iter()) {
        assert!(
            corpus::row(i).deferral.is_none(),
            "row {i} is driven/gated and must not also declare a DeferralBucket"
        );
    }
}

/// True if any top-level ability/trigger/static/replacement of `face` parsed to
/// `Effect::Unimplemented` — i.e. the card is not yet fully modeled.
fn face_has_unimplemented(face: &crate::types::card::CardFace) -> bool {
    let ability_unimpl = |def: &AbilityDefinition| {
        let mut stack = vec![&*def.effect];
        while let Some(e) = stack.pop() {
            if matches!(e, Effect::Unimplemented { .. }) {
                return true;
            }
        }
        false
    };
    face.abilities.iter().any(ability_unimpl)
        || face
            .triggers
            .iter()
            .any(|t| t.execute.as_deref().is_some_and(ability_unimpl))
}

/// ACCEPTANCE OVER THE WHOLE CORPUS (all 53 rows): every card of every combo is
/// present in the real card-data export, and its implementation status matches
/// the row's `gated_on` — a non-gated combo has zero `Effect::Unimplemented`
/// across all its cards, while a gated combo legitimately contains an unmodeled
/// card. Runs against the real export when present; skips gracefully when absent.
#[test]
fn corpus_cards_present_and_implementation_status_matches_gating() {
    let db = card_db();

    let mut missing: Vec<String> = Vec::new();
    // Non-gated rows whose cards unexpectedly carry Unimplemented (a regression
    // that would silently make a "testable" row undriveable).
    let mut unexpected_unimpl: Vec<String> = Vec::new();

    for i in 0..corpus::corpus_len() {
        let row = corpus::row(i);
        for &card in row.cards {
            match db.get_face_by_name(card) {
                None => missing.push(format!("{} (in {})", card, row.name)),
                Some(face) => {
                    // Only the non-gated rows must be fully modeled; the 4 gated
                    // rows legitimately contain an unmodeled card. A nested
                    // Unimplemented in a cost/replacement may not be surfaced by
                    // `face_has_unimplemented` (it walks top-level ability/trigger
                    // effects), so this is a conservative *non-regression* check.
                    if row.gated_on.is_none() && face_has_unimplemented(face) {
                        unexpected_unimpl.push(format!("{} (in {})", card, row.name));
                    }
                }
            }
        }
    }

    assert!(
        missing.is_empty(),
        "every corpus card must exist in the export; missing: {missing:?}"
    );
    assert!(
        unexpected_unimpl.is_empty(),
        "non-gated corpus combos must have no top-level Unimplemented; regressions: {unexpected_unimpl:?}"
    );
}

/// Meta: the driven set is a subset of the non-gated corpus and every driven row
/// is currently driven by a real driver (kept honest as drivers are added/removed
/// in `corpus::DRIVERS`).
#[test]
fn confirmed_drivers_match_expected() {
    for &(idx, _) in corpus::DRIVERS {
        assert!(idx < corpus::corpus_len(), "driven index in range");
        assert!(
            corpus::row(idx).gated_on.is_none(),
            "{}: a driven combo must not be card-gated",
            corpus::row(idx).name
        );
    }
}

// ===========================================================================
// CLI dispatch tests: the shared `corpus::drive_row` classifier.
// ===========================================================================

/// The shared CLI dispatch ([`corpus::drive_row`]) classifies each status class
/// through the REAL pipeline. NOTE: these are ARRAY indices; the gated rows are
/// 2 / 21 / 38 / 51 (Doc Aurlock / Professor Onyx / Animate Dead / Grindstone) —
/// NOT the ComboRow doc-comment's FEASIBILITY numbering (array idx 49 is a
/// CONFIRMED driver, Spike Feeder + Archangel, not a gated row).
#[test]
fn drive_row_classifies_corpus_via_shared_pipeline() {
    let db = card_db();

    // Confirmed (offline): Devoted Druid + Vizier names a mana axis, Advantage.
    match corpus::drive_row(db, 6).status {
        corpus::RowStatus::Confirmed {
            unbounded,
            win_kind,
        } => {
            assert_eq!(win_kind, WinKind::Advantage);
            assert!(
                unbounded.iter().any(|a| matches!(a, ResourceAxis::Mana(_))),
                "Devoted/Vizier must name a mana axis (got {unbounded:?})"
            );
        }
        other => panic!("idx 6 must be Confirmed, got {other:?}"),
    }

    // Confirmed (live drain): both drain cascades win LethalDamage via the live path.
    for idx in [17usize, 18] {
        match corpus::drive_row(db, idx).status {
            corpus::RowStatus::Confirmed { win_kind, .. } => {
                assert_eq!(win_kind, WinKind::LethalDamage)
            }
            other => panic!("idx {idx} (live drain) must be Confirmed LethalDamage, got {other:?}"),
        }
    }

    // Gated: never Failed — gated is expected. (ARRAY indices, see the note above.)
    for (idx, card) in [
        (2usize, "Doc Aurlock, Grizzled Genius"),
        (21, "Professor Onyx"),
        (38, "Animate Dead"),
        (51, "Grindstone"),
    ] {
        match corpus::drive_row(db, idx).status {
            corpus::RowStatus::Gated { card: c } => assert_eq!(c, card),
            other => panic!("idx {idx} must be Gated (never Failed), got {other:?}"),
        }
    }

    // Deferred: a non-driven testable row reports its structural bucket, never Failed.
    match corpus::drive_row(db, 22).status {
        corpus::RowStatus::Deferred { bucket } => {
            assert_eq!(bucket, corpus::DeferralBucket::ObjectReentry)
        }
        other => panic!("idx 22 (Kiki-Jiki) must be Deferred(ObjectReentry), got {other:?}"),
    }
}

/// REVERT-PROBE (the discriminating core): `classify_status` compares the driven
/// certificate against the row spec; it must NOT rubber-stamp any `Some(cert)`.
/// Each `Failed` assertion below flips to `Confirmed` under a specific reverted
/// predicate (named inline), and the positive control proves the `Failed` cases
/// are discriminating rather than vacuously always-Failed.
#[test]
fn classify_status_compares_against_spec_not_rubber_stamp() {
    let row = corpus::row(6); // Devoted Druid + Vizier: family = Mana, win_kind = Advantage.

    // Wrong win_kind ⇒ Failed. Revert "classify any Some(cert) as Confirmed" flips this.
    let wrong_win = LoopCertificate {
        unbounded: vec![ResourceAxis::DamageDealt(P1)],
        win_kind: WinKind::LethalDamage,
        mandatory: false,
        residual_board_delta: BoardDelta::default(),
    };
    assert!(
        matches!(
            corpus::classify_status(row, Some(wrong_win)),
            corpus::RowStatus::Failed { .. }
        ),
        "a wrong-win_kind certificate must be Failed"
    );

    // Right win_kind but wrong family ⇒ Failed. Revert "drop the family check" flips this.
    let wrong_family = LoopCertificate {
        unbounded: vec![ResourceAxis::TokensCreated],
        win_kind: WinKind::Advantage,
        mandatory: false,
        residual_board_delta: BoardDelta::default(),
    };
    assert!(
        matches!(
            corpus::classify_status(row, Some(wrong_family)),
            corpus::RowStatus::Failed { .. }
        ),
        "a right-win_kind / wrong-family certificate must be Failed"
    );

    // No certificate ⇒ Failed. Revert "treat None as Confirmed" flips this.
    assert!(
        matches!(
            corpus::classify_status(row, None),
            corpus::RowStatus::Failed { .. }
        ),
        "no certificate must be Failed"
    );

    // POSITIVE CONTROL: a correct (mana + Advantage) certificate Confirms — proving
    // the three Failed assertions above are discriminating, not always-Failed.
    let right = LoopCertificate {
        unbounded: vec![ResourceAxis::Mana(ManaType::Green)],
        win_kind: WinKind::Advantage,
        mandatory: false,
        residual_board_delta: BoardDelta::default(),
    };
    assert!(
        matches!(
            corpus::classify_status(row, Some(right)),
            corpus::RowStatus::Confirmed { .. }
        ),
        "a correct certificate must Confirm"
    );
}

/// REVERT-PROBE (live path), symmetric with the offline probe above:
/// `classify_live` must compare the live `GameOver` winner against the loop's
/// controller (P0), NOT rubber-stamp any `Some(_)`. Reverting `classify_live` to
/// `Some(_) => Confirmed` flips the wrong-winner assertion below; the `None`
/// assertion flips under a `None => Confirmed` revert; the positive control proves
/// the `Failed` cases are discriminating rather than vacuously always-Failed.
#[test]
fn classify_live_compares_winner_not_rubber_stamp() {
    let row = corpus::row(18); // Marauding Blight-Priest + Conqueror: Drain / LethalDamage.

    // Wrong winner (P1, the victim) ⇒ Failed. Revert "Some(_) => Confirmed" flips this.
    assert!(
        matches!(
            corpus::classify_live(row, Some((6, P1))),
            corpus::RowStatus::Failed { .. }
        ),
        "a live drain won by the wrong player (P1, the victim) must be Failed"
    );

    // No GameOver ⇒ Failed. Revert "treat None as Confirmed" flips this.
    assert!(
        matches!(
            corpus::classify_live(row, None),
            corpus::RowStatus::Failed { .. }
        ),
        "no GameOver must be Failed"
    );

    // POSITIVE CONTROL: the controller P0 winning a LethalDamage drain Confirms —
    // proving the Failed assertions above are discriminating, not always-Failed.
    match corpus::classify_live(row, Some((6, P0))) {
        corpus::RowStatus::Confirmed { win_kind, .. } => {
            assert_eq!(win_kind, WinKind::LethalDamage)
        }
        other => panic!("controller (P0) winning must Confirm LethalDamage, got {other:?}"),
    }
}

// ===========================================================================
// Per-combo driven acceptance tests (thin wrappers over the shared drivers).
// Each asserts a confirmed `LoopCertificate` of the documented family + win_kind
// via `assert_combo`, and skips (returns early) if the card-data export is absent.
// ===========================================================================

/// Assert a combo's driven certificate names the row's expected resource family
/// and classifies the expected `win_kind`.
fn assert_combo(idx: usize, cert: &LoopCertificate) {
    let row = corpus::row(idx);
    assert!(
        cert.unbounded
            .iter()
            .any(|a| corpus::family_matches_axis(row.family, a)),
        "{}: certificate {:?} must name a {:?}-family axis",
        row.name,
        cert.unbounded,
        row.family,
    );
    assert_eq!(cert.win_kind, row.win_kind, "{}: win_kind", row.name);
}

/// HELIOD, SUN-CROWNED + WALKING BALLISTA — the canonical driving combo.
/// DISCRIMINATION: the `expect` flips if either `detect_loop` gate is reverted.
#[test]
fn drive_heliod_ballista_certificate() {
    let cert = corpus::drive_offline_heliod_ballista(card_db()).expect(
        "Heliod + Ballista must be confirmed: board identical modulo life/damage, +1 damage/cycle",
    );
    assert_eq!(cert.win_kind, WinKind::LethalDamage);
    assert!(
        cert.covers(&[ResourceAxis::DamageDealt(P1)]),
        "certificate must name unbounded damage to the opponent (got {:?})",
        cert.unbounded
    );
}

/// #4 DEVOTED DRUID + VIZIER OF REMEDIES — infinite green mana.
#[test]
fn drive_combo_04_devoted_vizier() {
    let cert = corpus::drive_offline_devoted_vizier(card_db())
        .expect("Devoted Druid + Vizier must confirm infinite green mana");
    assert_combo(6, &cert);
}

/// #2 GRIM MONOLITH + POWER ARTIFACT — infinite colorless mana.
#[test]
fn drive_combo_02_grim_power() {
    let cert = corpus::drive_offline_grim_power(card_db())
        .expect("Grim Monolith + Power Artifact must confirm infinite colorless mana");
    assert_combo(4, &cert);
}

/// #47 SPIKE FEEDER + ARCHANGEL OF THUNE — infinite +1/+1 counters + life.
#[test]
fn drive_combo_47_spike_archangel() {
    let cert = corpus::drive_offline_spike_archangel(card_db())
        .expect("Spike Feeder + Archangel must confirm infinite counters + life");
    assert_combo(49, &cert);
}

/// #7 BLOOM TENDER + FREED FROM THE REAL — infinite mana.
#[test]
fn drive_combo_07_bloom_freed() {
    let cert = corpus::drive_offline_bloom_freed(card_db())
        .expect("Bloom Tender + Freed must confirm infinite mana");
    assert_combo(9, &cert);
}

/// #11 FAEBURROW ELDER + PEMMIN'S AURA — infinite mana.
#[test]
fn drive_combo_11_faeburrow_pemmin() {
    let cert = corpus::drive_offline_faeburrow_pemmin(card_db())
        .expect("Faeburrow Elder + Pemmin's Aura must confirm infinite mana");
    assert_combo(13, &cert);
}

/// #11 SELVALA, HEART OF THE WILDS + STAFF OF DOMINATION — infinite mana.
#[test]
fn drive_combo_11_selvala_staff() {
    let cert = corpus::drive_offline_selvala_staff(card_db())
        .expect("Selvala + Staff of Domination must confirm infinite mana");
    assert_combo(12, &cert);
}

/// D2 KILO, APOGEE MIND + FREED + RELIC OF LEGENDS — infinite proliferate triggers.
#[test]
fn drive_combo_d2_kilo_freed_relic() {
    let cert = corpus::drive_offline_kilo_freed_relic(card_db())
        .expect("Kilo + Freed + Relic must confirm infinite proliferate triggers");
    assert_combo(1, &cert);
}

/// PR-7 #8 (offline acceptance): a REAL Pentad Prism seeded with ≥1 charge counter,
/// under the Kilo/Freed/Relic proliferate engine, is certified as an infinite
/// preserved-`Generic` charge-counter growth loop — `WinKind::Advantage` (CR 104.4b:
/// an optional loop is never a draw) naming the counter axis
/// `Counter(Other, Other)` (Pentad is a non-creature artifact ⇒ `ObjectClass::Other`;
/// charge is a `Generic` counter ⇒ `CounterClass::Other`, CR 122.1).
///
/// A2 (measured, not assumed): the cert being `Some` proves `detect_loop`'s
/// recurrence gate matched — the constant-depth `loop_states_equal_modulo_resources`
/// FAILS on the growing charge (Generic is preserved), so this certification depends
/// on the new `loop_states_cover_modulo_counter_growth` disjunct. Reverting that
/// disjunct flips this `expect` to a panic (no cert).
#[test]
fn drive_pentad_prism_charge_growth_certificate() {
    let cert = corpus::drive_offline_pentad_prism(card_db()).expect(
        "Pentad Prism (seeded ≥1 charge) under the proliferate engine must confirm an \
         infinite charge-counter growth loop",
    );
    assert_eq!(
        cert.win_kind,
        WinKind::Advantage,
        "a charge-counter growth loop is a CR 104.4b optional advantage engine, not a win"
    );
    assert!(
        cert.covers(&[ResourceAxis::Counter(
            CounterClass::Other,
            ObjectClass::Other
        )]),
        "certificate must name the unbounded charge counter axis (got {:?})",
        cert.unbounded
    );
}

/// PR-7 #5 (Sunburst 0-charge dead-loop CONTROL, paired with #8): a Pentad Prism
/// seeded with ZERO charge counters (a generic-mana Sunburst cast ⇒ 0 charge) has no
/// counter for proliferate to grow, so the cycle degrades to the pure Kilo proliferate
/// loop — the cert is still `Some` (certified via the constant-depth equality path on
/// the identical board) but carries NO counter axis. The seed is thus load-bearing:
/// #8's `Counter(Other, Other)` axis appears ONLY because a charge counter was present
/// to grow.
#[test]
fn drive_pentad_prism_zero_charge_has_no_counter_axis() {
    let cert = corpus::drive_offline_pentad_prism_seeded(card_db(), 0).expect(
        "a 0-charge Pentad still rides the pure Kilo proliferate loop (board identical) \
         and must confirm via the equality path",
    );
    assert!(
        !cert
            .unbounded
            .iter()
            .any(|a| matches!(a, ResourceAxis::Counter(..))),
        "a 0-charge Pentad has no counter to grow ⇒ the cert must NOT name a counter axis \
         (got {:?})",
        cert.unbounded
    );
}

/// PR-7 54th (offline acceptance): a REAL Walking Ballista seeded with ≥1 +1/+1
/// counter, under the Kilo/Freed/Relic proliferate engine, is certified as an
/// infinite-DAMAGE loop — `WinKind::LethalDamage` (CR 704.5a / CR 120.3a: each ping is
/// 1 life loss on the opponent) naming the `DamageDealt(P1)` axis. The +1/+1 counter is
/// MONOTONE (CR 122.1a), so `project_object_for_loop` strips it and the EXISTING
/// constant-depth gate-1 (`loop_states_equal_modulo_resources`) covers the growth — no
/// new predicate (contrast the 53rd Pentad's non-monotone `Generic` charge).
///
/// DISCRIMINATION: the paired X=0 control below (same driver, only the seed differs)
/// loses the damage axis, so this `covers` is not vacuously "any cert". The `expect`
/// flips to a panic if `detect_loop`'s recurrence gate is reverted; the `win_kind` /
/// `covers` assertions flip if `classify_win_kind`'s damage branch is dropped
/// (Advantage) — proving the ping actually reached the opponent (a mis-answered
/// `TargetSelection` prompt would leave no `DamageDealt(P1)` axis and fail `covers`).
#[test]
fn drive_kilo_freed_relic_ballista_certificate() {
    let cert = corpus::drive_offline_kilo_freed_relic_ballista(card_db(), 2).expect(
        "Walking Ballista (seeded ≥1 +1/+1) under the proliferate engine must confirm an \
         infinite damage loop: board identical modulo the monotone +1/+1, +1 damage/cycle",
    );
    assert_eq!(
        cert.win_kind,
        WinKind::LethalDamage,
        "a +1 damage/cycle ping on the opponent is an unbounded CR 704.5a lethal-damage loop"
    );
    assert!(
        cert.covers(&[ResourceAxis::DamageDealt(P1)]),
        "certificate must name unbounded damage to the opponent (got {:?})",
        cert.unbounded
    );
    assert!(
        !cert.mandatory,
        "an activated-ability loop is optional (CR 602.1), so mandatory == false"
    );
}

/// PR-7 54th (X=0 dead-loop CONTROL, paired with the acceptance above): a Walking
/// Ballista seeded with ZERO +1/+1 counters is a 0/0 that dies to the SBA (CR 704.5f)
/// with no counter to remove, so the ping activation is rejected and the cycle degrades
/// to the pure Kilo/Freed/Relic proliferate loop — the cert is still `Some` (board
/// identical) but names NO damage axis and classifies `Advantage`. The seed is thus
/// load-bearing: the acceptance's `LethalDamage` + `DamageDealt(P1)` appear ONLY because
/// a +1/+1 counter was present to remove (mirrors
/// `drive_pentad_prism_zero_charge_has_no_counter_axis`). This also pins the corrected
/// wiring: `drive_ballista_ping` would `.expect()`-panic on the dead activation, whereas
/// the driver's `activate_and_resolve` degrades gracefully.
#[test]
fn drive_kilo_freed_relic_ballista_x0_no_damage_axis() {
    let cert = corpus::drive_offline_kilo_freed_relic_ballista(card_db(), 0).expect(
        "a 0-counter Ballista still rides the pure Kilo proliferate loop (board identical) \
         and must confirm via the equality path",
    );
    assert_eq!(
        cert.win_kind,
        WinKind::Advantage,
        "with a dead Ballista the only unbounded axis is the proliferate trigger — an \
         advantage engine, not a lethal win"
    );
    assert!(
        !cert
            .unbounded
            .iter()
            .any(|a| matches!(a, ResourceAxis::DamageDealt(_))),
        "a 0-counter Ballista deals no damage ⇒ the cert must NOT name a damage axis \
         (got {:?})",
        cert.unbounded
    );
}

/// PR-7 "One-Ring" (offline acceptance): a REAL The One Ring on the OPPONENT (P1),
/// seeded ≥1 burden, under the Kilo/Freed/Relic proliferate engine, certifies an
/// infinite preserved-`Generic` burden-growth loop — `WinKind::Advantage`
/// (CR 104.4b) naming `Counter(Other, Other)`.
///
/// A2 (analytic non-vacuity, not asserted directly): the cert being `Some` proves
/// the recurrence gate matched; the constant-depth `loop_states_equal_modulo_resources`
/// FAILS on the growing burden (Generic is preserved), so this rides the
/// `loop_states_cover_modulo_counter_growth` disjunct. The RUNNABLE discriminator is
/// the seed=0 control below (Counter axis appears ONLY with seeded burden). Do NOT
/// overclaim "no burden ⇒ no cert": the loop stays `Some` via `Trigger(Proliferate)`.
#[test]
fn one_ring_burden_growth_certificate() {
    let cert = corpus::drive_offline_kilo_freed_relic_one_ring(card_db(), 1).expect(
        "The One Ring (seeded ≥1 burden) under the proliferate engine must confirm an \
         infinite burden-counter growth loop",
    );
    assert_eq!(
        cert.win_kind,
        WinKind::Advantage,
        "a burden-counter growth loop is a CR 104.4b optional advantage engine, not a win"
    );
    assert!(
        cert.covers(&[ResourceAxis::Counter(
            CounterClass::Other,
            ObjectClass::Other
        )]),
        "certificate must name the unbounded burden counter axis (got {:?})",
        cert.unbounded
    );
    assert!(
        !cert.mandatory,
        "an activated-ability loop is optional (CR 602.1), so mandatory == false"
    );
}

/// PR-7 "One-Ring" (seed=0 CONTROL, paired): a 0-burden Ring has no counter for
/// proliferate to grow, so the cycle degrades to the pure Kilo loop — cert still
/// `Some` (equality path, board identical) but names NO counter axis. The seed is
/// thus load-bearing: the `Counter(Other, Other)` axis appears ONLY because burden
/// was present to grow (mirrors `drive_pentad_prism_zero_charge_has_no_counter_axis`).
#[test]
fn one_ring_zero_burden_has_no_counter_axis() {
    let cert = corpus::drive_offline_kilo_freed_relic_one_ring(card_db(), 0).expect(
        "a 0-burden Ring still rides the pure Kilo proliferate loop (board identical) \
         and must confirm via the equality path",
    );
    assert!(
        !cert
            .unbounded
            .iter()
            .any(|a| matches!(a, ResourceAxis::Counter(..))),
        "a 0-burden Ring has no counter to grow ⇒ cert must NOT name a counter axis (got {:?})",
        cert.unbounded
    );
}

/// #10 PRIEST OF TITANIA + UMBRAL MANTLE — infinite green mana.
#[test]
fn drive_combo_10_priest_umbral() {
    let cert = corpus::drive_offline_priest_umbral(card_db())
        .expect("Priest of Titania + Umbral Mantle must confirm infinite green mana");
    assert_combo(10, &cert);
}

/// REVERT-PROBE for [`drive_combo_10_priest_umbral`]: omit the Umbral untap step ⇒
/// Priest stays tapped after the first cycle, so the second cycle's tap-for-mana
/// fails and the board is NOT identical ⇒ `run_combo` finds no certificate.
#[test]
fn drive_combo_10_priest_umbral_requires_untap() {
    let mut board = corpus::build_board_green(card_db(), corpus::row(10).cards)
        .expect("Priest of Titania board must build from the fixture");
    let priest = board.ids[0];
    let umbral = board.ids[1];
    corpus::seed_subtype_creatures(board.runner.state_mut(), "Elf", 4);
    corpus::attach_aura(board.runner.state_mut(), umbral, priest);
    let cert = corpus::run_combo(board, |probe| {
        if let Some(tap_idx) =
            corpus::ability_index_where(probe.runner().state(), priest, corpus::is_mana_effect)
        {
            corpus::activate_and_resolve(probe, priest, tap_idx, None);
        }
        // No untap: Priest stays tapped, so this is not a repeatable loop.
    });
    assert!(
        cert.is_none(),
        "without the Umbral untap, Priest stays tapped — no loop"
    );
}

/// #14 MARWYN, THE NURTURER + SWORD OF THE PARUNS — infinite green mana.
#[test]
fn drive_combo_14_marwyn_sword() {
    let cert = corpus::drive_offline_marwyn_sword(card_db())
        .expect("Marwyn + Sword of the Paruns must confirm infinite green mana");
    assert_combo(14, &cert);
}

/// REVERT-PROBE for [`drive_combo_14_marwyn_sword`]: omit the Sword untap step ⇒
/// Marwyn stays tapped, the next cycle's tap-for-mana fails, and the board is not
/// identical ⇒ no certificate. Proves the modal untap is load-bearing.
#[test]
fn drive_combo_14_marwyn_sword_requires_untap() {
    let mut board = corpus::build_board_green(card_db(), corpus::row(14).cards)
        .expect("Marwyn board must build from the fixture");
    let marwyn = board.ids[0];
    let sword = board.ids[1];
    {
        let state = board.runner.state_mut();
        if let Some(o) = state.objects.get_mut(&marwyn) {
            o.counters
                .insert(crate::types::counter::CounterType::Plus1Plus1, 6);
        }
        corpus::settle_layers(state);
    }
    corpus::attach_aura(board.runner.state_mut(), sword, marwyn);
    let cert = corpus::run_combo(board, |probe| {
        if let Some(tap_idx) =
            corpus::ability_index_where(probe.runner().state(), marwyn, corpus::is_mana_effect)
        {
            corpus::activate_and_resolve(probe, marwyn, tap_idx, None);
        }
        // No untap: Marwyn stays tapped, so this is not a repeatable loop.
    });
    assert!(
        cert.is_none(),
        "without the Sword untap, Marwyn stays tapped — no loop"
    );
}

// ===========================================================================
// Synthetic loop pipeline tests (no export): board-neutral pinger + negatives.
// ===========================================================================

/// Build a battlefield creature with a board-neutral repeatable activated
/// ability (no cost) that deals `amount` damage to any target. Each activation
/// returns the board to an identical configuration while pumping the damage axis.
fn pinger_scenario(amount: i32) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 40); // survive many pings without an SBA loss mid-test
    let ability = AbilityDefinition::new(
        // CR 602.1: an activated ability ("[cost]: [effect]"); a costless ability
        // is board-neutral, so repeating it is the simplest faithful net-progress
        // loop (each iteration is board-identical).
        AbilityKind::Activated,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
    );
    let pinger = scenario
        .add_creature(P0, "Test Pinger", 1, 1)
        .with_ability_definition(ability)
        .id();
    (scenario, pinger)
}

/// Drive one full activation cycle of the pinger at the opponent through the real
/// pipeline: activate → select target (opponent) → resolve to stack-empty.
fn drive_one_ping(probe: &mut LoopProbe, pinger: ObjectId) {
    // CR 602.1 / CR 601.2: activate the (costless) ability — it goes on the stack.
    let activated = probe
        .act(GameAction::ActivateAbility {
            source_id: pinger,
            ability_index: 0,
        })
        .expect("activate pinger");
    assert!(
        matches!(activated.waiting_for, WaitingFor::TargetSelection { .. }),
        "pinger must prompt for a target"
    );
    // CR 601.2c: target the opponent.
    probe
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P1)],
        })
        .expect("target opponent");
    // CR 608: resolve by passing priority until the stack empties.
    for _ in 0..8 {
        if probe.runner().state().stack.is_empty() {
            break;
        }
        if probe.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

/// END-TO-END DRIVING TEST (the Heliod-shaped damage acceptance): a board-neutral
/// repeatable ping loop, driven through the real `apply()` pipeline via
/// [`LoopProbe`], is confirmed by [`detect_loop`] as a `LethalDamage` net-progress
/// loop whose unbounded axis is damage to the opponent.
///
/// DISCRIMINATION: this is the assertion that flips if the detector is reverted.
#[test]
fn drive_damage_loop_certificate() {
    let (scenario, pinger) = pinger_scenario(1);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
    }

    let mut probe = crate::analysis::LoopProbe::new(&mut runner);

    // WARMUP: drive one full ping cycle first to saturate per-turn bookkeeping so
    // two subsequent steady-state iterations differ only by the monotone resource.
    drive_one_ping(&mut probe, pinger);
    let _ = probe.iteration_delta(); // discard warmup; roll the boundary forward

    // STEADY ITERATION N: snapshot the loop point, drive one cycle, snapshot again.
    let cycle_start = probe.runner().state().clone();
    drive_one_ping(&mut probe, pinger);
    let delta = probe.iteration_delta();
    let cycle_end = probe.runner().state().clone();

    let cert = detect_loop(&cycle_start, &cycle_end, &delta, P0, true)
        .expect("board-identical +damage cycle must be confirmed as a net-progress loop");

    assert_eq!(cert.win_kind, WinKind::LethalDamage);
    assert!(
        cert.covers(&[ResourceAxis::DamageDealt(P1)]),
        "the certificate must name unbounded damage to the opponent (got {:?})",
        cert.unbounded
    );
}

/// END-TO-END SOUNDNESS NEGATIVE: an action that changes the board (cast a
/// creature from hand) yields differing start/end states ⇒ no certificate.
#[test]
fn drive_board_change_is_not_a_loop() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bolt = scenario.add_bolt_to_hand(P0); // a card that leaves hand on cast
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
    }
    let bolt_card = runner.state().objects[&bolt].card_id;

    let start = runner.state().clone();
    let mut probe = crate::analysis::LoopProbe::new(&mut runner);

    // Cast the bolt: it moves Hand -> Stack -> Graveyard, a genuine board change.
    probe
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id: bolt_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast bolt");
    probe
        .act(GameAction::SelectTargets {
            targets: vec![crate::types::ability::TargetRef::Player(P1)],
        })
        .expect("target opponent");
    for _ in 0..8 {
        if probe.runner().state().stack.is_empty() {
            break;
        }
        if probe.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    let delta = probe.iteration_delta();
    let end = probe.runner().state().clone();

    // Even though damage advanced, the board changed (the bolt is now in the
    // graveyard, not the hand) — NOT a repeatable loop.
    assert!(
        detect_loop(&start, &end, &delta, P0, true).is_none(),
        "a cast that moves a card between zones is a board change, not a loop"
    );
}

/// END-TO-END SOUNDNESS NEGATIVE: an idle board with NO driven progress yields no
/// certificate. Revert-probe for the net-progress gate at the driven level.
#[test]
fn drive_idle_board_is_not_a_loop() {
    let (scenario, _pinger) = pinger_scenario(1);
    let mut runner = scenario.build();
    let start = runner.state().clone();
    let mut probe = crate::analysis::LoopProbe::new(&mut runner);
    // Drive nothing; close the iteration. State-readable delta is empty and no
    // events were fed.
    let delta = probe.iteration_delta();
    let end = probe.runner().state().clone();
    assert!(
        detect_loop(&start, &end, &delta, P0, true).is_none(),
        "an idle cycle with no progress is not a loop"
    );
}

// ===========================================================================
// PR-3 (Option C) live drain-cascade tests. These drive the REAL per-beat
// `apply(PassPriority)` reducer (not the offline `detect_loop` harness) so the
// persisted `loop_detect_ring` accumulation and the reconcile-seam win shortcut
// are exercised end-to-end. Each names its revert-fail line. Board install +
// cascade seeding + per-beat drive use the shared `corpus::` toolkit.
// ===========================================================================

/// C-L1: the PERSISTED loop-detection ring wins idx 18 (Marauding Blight-Priest +
/// Bloodthirsty Conqueror) LIVE under the default per-beat `apply(PassPriority)`
/// drive. P1 starts at 200 life so a natural CR 704.5a death cannot be the cause;
/// the live `GameOver{Some(P0)}` fires by ~beat 6 from the accumulated ring + the
/// §3 reconcile shortcut.
///
/// REVERT-FAIL (the named discriminators, each flips this assertion):
///  (a) remove the §3 block in `reconcile_terminal_result` ⇒ the cascade grinds,
///      no early `GameOver` ⇒ `expect` fails.
///  (b) remove the relocated §2 `record_loop_detect_sample` block in
///      `pass_priority_once_with_pipeline` ⇒ the ring never persists across beats ⇒
///      no early `GameOver` ⇒ `expect` fails.
#[test]
fn drive_drain_idx18_wins_live() {
    let Some(mut board) = corpus::build_drain_board(card_db(), corpus::row(18).cards, 200) else {
        return; // export absent (CI / fresh checkout): skip, never fail spuriously
    };
    corpus::seed_lifegain_cascade(&mut board);
    let trace = corpus::drive_pass_priority(&mut board, 40);
    let (beat, winner) = corpus::first_gameover_beat(&trace)
        .expect("idx18 drain cascade must win LIVE via the persisted ring + §3 shortcut");
    assert_eq!(
        winner, P0,
        "the single non-falling player (P0) must be the winner"
    );
    assert!(
        beat <= 12,
        "the live win must fire from the ring (~beat 6), not the ~400-beat 704.5a death; got beat {beat}"
    );
    // The PERSISTED ring accumulated across beats (≥ 3 snapshots are needed for the
    // modulo-match that fires the win), and the cascade held the stack non-empty.
    let max_ring = trace.iter().map(|t| t.ring_len).max().unwrap_or(0);
    assert!(
        max_ring >= 3,
        "the ring must accumulate ≥3 persisted snapshots before the shortcut (got {max_ring})"
    );
    assert!(
        trace.iter().all(|t| t.stack_len >= 1),
        "a self-refilling cascade keeps the stack non-empty at every beat"
    );
    // Drain direction: the victim P1 drained (and is well above 0 — the win is the
    // SHORTCUT, not a real CR 704.5a death), while the controller P0 gained.
    let last = trace.last().expect("at least one beat");
    assert!(
        last.min_opponent_life() > 100 && last.min_opponent_life() < 200,
        "P1 must have drained but still be at high life when shortcut-eliminated (got {})",
        last.min_opponent_life()
    );
    assert!(
        last.lives[0] >= 41,
        "controller P0 gained life across the cascade"
    );
}

/// C-L2 (a genuinely SECOND loop shape — idx 17, Sanguine Bond + Exquisite Blood,
/// the TARGETED `LoseLife` variant). The per-beat drive auto-resolves the "target
/// opponent loses that much life" trigger to the sole legal target (opponent P1)
/// with NO target-selection stop, so this targeted shape also wins live.
///
/// REVERT-FAIL: same as C-L1 — removing the §3 block or the §2 sample drops the
/// early `GameOver`.
#[test]
fn drive_drain_idx17_targeted_wins_live() {
    let Some(mut board) = corpus::build_drain_board(card_db(), corpus::row(17).cards, 200) else {
        return; // export absent: skip
    };
    corpus::seed_lifegain_cascade(&mut board);
    let trace = corpus::drive_pass_priority(&mut board, 40);
    // The targeted trigger auto-resolves: no target window appears at any beat.
    assert!(
        trace.iter().all(|t| !matches!(
            t.wf,
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
        )),
        "idx17's targeted trigger must auto-resolve (no target-selection stop) under the per-beat drive"
    );
    let (beat, winner) = corpus::first_gameover_beat(&trace)
        .expect("idx17 targeted drain cascade must win LIVE (auto-resolved target)");
    assert_eq!(winner, P0);
    assert!(
        beat <= 12,
        "live win from the ring, not the 704.5a death; got beat {beat}"
    );
    // P1 (the targeted opponent) is the faller — the auto-resolved target is correct.
    assert!(
        board.runner.state().players[1].life < 200,
        "the auto-resolved target must be the opponent P1 (its life drained)"
    );
}

/// C-L1-probe — Defect-2 BOUNDED-TERMINATION regression guard. After the live drive
/// has populated the ring at the RESOLVING `Priority{non-active}` window (ring ≥ 2,
/// stack≠∅, BEFORE any `GameOver`), call `legal_actions` directly. The legality
/// probe clones-and-applies `PassPriority`, which resolves the top and re-enters
/// `reconcile_terminal_result` §3 — the seam the Defect-2 recursion concern is about.
/// This asserts the call TERMINATES with a bounded, non-empty action list.
#[test]
fn drive_drain_idx18_legal_actions_terminates_bounded() {
    let Some(mut board) = corpus::build_drain_board(card_db(), corpus::row(18).cards, 200) else {
        return; // export absent: skip
    };
    corpus::seed_lifegain_cascade(&mut board);
    // Drive until the ring is populated (≥ 2 snapshots) at the RESOLVING priority
    // window — `Priority{non-active}` (the last passer), where the NEXT all-pass
    // resolves the top and would push the ring to a modulo MATCH.
    let active = board.runner.state().active_player;
    let mut reached = false;
    for _ in 0..40 {
        if matches!(
            board.runner.state().waiting_for,
            WaitingFor::GameOver { .. }
        ) {
            break;
        }
        if board.runner.act(GameAction::PassPriority).is_err() {
            break;
        }
        let s = board.runner.state();
        if s.loop_detect_ring.len() >= 2
            && !s.stack.is_empty()
            && matches!(s.waiting_for, WaitingFor::Priority { player } if player != active)
        {
            reached = true;
            break;
        }
    }
    assert!(
        reached,
        "must reach a populated-ring RESOLVING priority window before GameOver"
    );
    // The decisive call: with the guard, this terminates and returns a bounded list.
    // Without the guard it would stack-overflow (SIGABRT) and the test could not pass.
    let actions = crate::ai_support::legal_actions(board.runner.state());
    assert!(
        !actions.is_empty(),
        "legal_actions must return a bounded, non-empty action list (no recursion)"
    );
    // The immutable probe must not have ended the live game.
    assert!(
        !matches!(
            board.runner.state().waiting_for,
            WaitingFor::GameOver { .. }
        ),
        "legal_actions takes &state — it must not mutate the live game to GameOver"
    );
}

/// Install a board-neutral artifact on `player` with a costless "draw 1" activated
/// ability — a meaningful (loop-ending) priority action.
fn add_meaningful_action_artifact(state: &mut GameState, player: PlayerId) -> ObjectId {
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;
    let id = crate::game::zones::create_object(
        state,
        CardId(state.next_object_id),
        player,
        "Loop-Ending Artifact".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).expect("just created");
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.summoning_sick = false;
    std::sync::Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    ));
    id
}

/// C-neg-A (+ the live face of the all-players §9 probe): a victim that holds a
/// MEANINGFUL loop-ending action is NEVER shortcut-eliminated. The victim P1 holds
/// a costless "draw 1" ability, so `no_living_player_has_meaningful_priority_action`
/// finds the out ⇒ the §3 shortcut is refused and no early `GameOver` fires.
///
/// REVERT-FAIL:
///  - remove the §9 gate call at the §3 site ⇒ `GameOver{Some(P0)}` fires while P1
///    had an out (unsound) — this assertion (`no early GameOver`) flips.
///  - swap §9 for the current-holder-only probe ⇒ it misses P1's masked out and the
///    shortcut wrongly fires.
#[test]
fn drive_drain_idx18_victim_with_out_is_not_eliminated() {
    let Some(mut board) = corpus::build_drain_board(card_db(), corpus::row(18).cards, 200) else {
        return; // export absent: skip
    };
    add_meaningful_action_artifact(board.runner.state_mut(), P1);
    corpus::settle_layers(board.runner.state_mut());
    corpus::seed_lifegain_cascade(&mut board);
    let trace = corpus::drive_pass_priority(&mut board, 40);
    assert!(
        corpus::first_gameover_beat(&trace).is_none(),
        "P1 holds a loop-ending action — the §9 gate must refuse the shortcut (no GameOver)"
    );
}

/// G2 (multi-trigger ring-survival MECHANISM). Two DISTINGUISHABLE "whenever you gain
/// life" drainers — Marauding Blight-Priest (fixed: each opponent loses 1) and Vito,
/// Thorn of the Dusk Rose (dynamic: target opponent loses that much) — plus Bloodthirsty
/// Conqueror (the refiller: whenever an opponent loses life, you gain that much). Each
/// life-gain fires BOTH drainers simultaneously, so CR 603.3b forces a real
/// `OrderTriggers` window every cycle (the two abilities are order-dependent, so no
/// auto-ordering short-circuit). This drives the loop through that window and asserts
/// the `loop_detect_ring` SURVIVES it and accrues >= 2 samples.
///
/// SCOPE: the committed assertions are the ring-survival MECHANISM — the loop-detect
/// ring survives a multi-trigger `OrderTriggers` beat (CR 603.3b) so CR 732.2a detection
/// can accrue samples. NOTE (measured, both seam edits present): this mixed
/// untargeted+targeted fixture ALSO reaches end-to-end `GameOver { winner: Some(P0) }` at
/// beat 10 (Vito's targeted trigger auto-resolves forced-unique in 2p, so the untargeted
/// Blight-Priest -> Conqueror drain carries the loop and the CR 732.2a shortcut fires;
/// detection is via the COVER path — the cascade is super-critical, so the ring tops out at
/// exactly `max_ring == 2`). That win is deliberately NOT asserted here: `max_ring == 2`
/// sits at the current 2-sample detection edge, so a min-samples tightening would flip the
/// `GameOver` for a reason unrelated to ring-survival — the wrong granularity for a
/// ring-survival test. Ring survival is the robust invariant; the robust E2E multi-trigger
/// detection witness = the 52nd/G1 (non-edge).
///
/// REVERT-FAIL (measured, both seams load-bearing): reverting EDIT A (the SEAM1
/// `!matches!(wf, OrderTriggers)` clear guard) alone, or EDIT B (the SEAM2
/// `GameAction::OrderTriggers` clear exemption) alone, drops `max_ring` from 2 to 1 (and
/// the end-to-end `GameOver` from `Some(P0)` to `None`) — the ring is wiped on the
/// OrderTriggers beat and never accrues 2 samples.
#[test]
fn drive_multi_trigger_ring_survives_order_triggers_beat() {
    const FIXTURE: &[&str] = &[
        "Marauding Blight-Priest",
        "Vito, Thorn of the Dusk Rose",
        "Bloodthirsty Conqueror",
    ];
    let Some(mut board) = corpus::build_drain_board(card_db(), FIXTURE, 200) else {
        return; // export absent (CI / fresh checkout): skip, never fail spuriously
    };
    corpus::seed_lifegain_cascade(&mut board);
    let trace = corpus::drive_with_trigger_ordering(&mut board, 60);
    // NON-VACUITY: the two distinguishable drainers must actually force a real
    // OrderTriggers window — otherwise the ring-survival assertion is meaningless.
    assert!(
        trace
            .iter()
            .any(|t| matches!(t.wf, WaitingFor::OrderTriggers { .. })),
        "the two distinguishable 'whenever you gain life' drainers must force a real \
         OrderTriggers beat (CR 603.3b)"
    );
    // RING SURVIVAL (the discriminating signal): the ring survives the OrderTriggers
    // beat and accrues >= 2 Priority{active} samples across the cascade.
    let max_ring = trace.iter().map(|t| t.ring_len).max().unwrap_or(0);
    assert!(
        max_ring >= 2,
        "the loop-detect ring must survive the OrderTriggers beat and reach >= 2 samples \
         (got {max_ring})"
    );
}

/// G2 non-regression + driver-equivalence: idx18 (Marauding Blight-Priest + Bloodthirsty
/// Conqueror) is the SINGLE-trigger drain path — only Blight-Priest triggers on life
/// gain, so no simultaneous-trigger group and no `OrderTriggers` window ever forms.
/// Driving it through the new trigger-ordering driver must therefore behave exactly like
/// `drive_pass_priority`: ZERO OrderTriggers beats, and it still wins LIVE via the ring.
#[test]
fn drive_drain_idx18_single_trigger_has_no_order_beat() {
    let Some(mut board) = corpus::build_drain_board(card_db(), corpus::row(18).cards, 200) else {
        return; // export absent: skip
    };
    corpus::seed_lifegain_cascade(&mut board);
    let trace = corpus::drive_with_trigger_ordering(&mut board, 40);
    assert!(
        trace
            .iter()
            .all(|t| !matches!(t.wf, WaitingFor::OrderTriggers { .. })),
        "idx18 is single-trigger — no OrderTriggers window must ever form"
    );
    let (_, winner) = corpus::first_gameover_beat(&trace)
        .expect("idx18 still wins LIVE under the trigger-ordering driver (single-trigger path)");
    assert_eq!(winner, P0);
}

/// G2 false-positive control: the SAME two distinguishable drainers WITHOUT the refiller
/// (drop Bloodthirsty Conqueror). The seed life-gain still fires both drainers → a real
/// OrderTriggers beat occurs — but with no refill the cascade is FINITE (P1 loses a
/// couple of life from 200 and stops). The surviving ring must NOT manufacture a false
/// loop: no `GameOver` fires within the window.
#[test]
fn drive_multi_trigger_no_refiller_is_finite() {
    const FIXTURE_NO_REFILL: &[&str] = &["Marauding Blight-Priest", "Vito, Thorn of the Dusk Rose"];
    let Some(mut board) = corpus::build_drain_board(card_db(), FIXTURE_NO_REFILL, 200) else {
        return; // export absent: skip
    };
    corpus::seed_lifegain_cascade(&mut board);
    let trace = corpus::drive_with_trigger_ordering(&mut board, 60);
    // Reach-guard: the OrderTriggers window still forms (both drainers fire off the seed
    // gain) — so the finite-ness assertion below is exercised past the OrderTriggers beat,
    // not vacuous on a board that never triggered.
    assert!(
        trace
            .iter()
            .any(|t| matches!(t.wf, WaitingFor::OrderTriggers { .. })),
        "both drainers must fire off the seed gain, forcing an OrderTriggers beat"
    );
    assert!(
        corpus::first_gameover_beat(&trace).is_none(),
        "with no refiller the cascade is finite — the surviving ring must not manufacture \
         a false loop/GameOver"
    );
}

// ===========================================================================
// Combo-detector OPT-IN gate (PR #4603): `GameState::loop_detection`.
// The live detector (ring sampler + reconcile-seam shortcut + `∞` producer) is
// gated behind a user-controllable toggle, default OFF. These tests pin the gate
// end-to-end on the SAME idx18 drain loop the tests above drive: ON wins + marks
// `∞`; OFF restores exact pre-detector behavior (no shortcut, no `∞`, no ring).
// ===========================================================================

/// PR6-GATE-1 (LIVE PRODUCER): with the detector ON, the idx18 drain loop both wins
/// LIVE (CR 704.5a via the §3 shortcut) AND marks `GameState::unbounded_resources`
/// for the winner, so `derive_views` projects ≥1 `∞` HUD row. This is the maintainer's
/// requested live producer — before this fix the only writer of `unbounded_resources`
/// was the debug `SetInfiniteMana` toggle.
///
/// REVERT-FAIL: delete the `mark_unbounded_loop(winner, …)` call at the §3 site ⇒
/// `unbounded_resources` stays empty ⇒ both the map and the `∞`-row assertions flip.
#[test]
fn pr6_gate_on_drain_marks_unbounded_resources() {
    let Some(mut board) = corpus::build_drain_board(card_db(), corpus::row(18).cards, 200) else {
        return; // export absent: skip
    };
    // build_drain_board opts the detector ON; assert that precondition explicitly so
    // a future change to the harness default cannot silently make this test vacuous.
    assert!(
        board.runner.state().loop_detection.is_on(),
        "the live-detector harness must run with loop_detection ON"
    );
    corpus::seed_lifegain_cascade(&mut board);
    let trace = corpus::drive_pass_priority(&mut board, 40);
    let (_, winner) = corpus::first_gameover_beat(&trace)
        .expect("idx18 must win LIVE with the detector ON (§3 shortcut)");
    assert_eq!(winner, P0, "the non-falling player wins");

    let state = board.runner.state();
    // The live producer recorded the confirmed loop's unbounded axes for the winner.
    let winner_axes = state
        .unbounded_resources
        .get(&winner)
        .expect("the live producer must mark unbounded_resources for the winning controller");
    assert!(
        !winner_axes.is_empty(),
        "the producer must name ≥1 unbounded axis for the confirmed drain loop"
    );
    // The `∞` HUD projection sees it: derive_views emits ≥1 row attributed to the winner.
    let views = derive_views(state, None);
    assert!(
        views
            .unbounded_resources
            .iter()
            .any(|row| row.player == winner),
        "derive_views must project an ∞ row for the winner (got {:?})",
        views.unbounded_resources
    );
}

/// PR6-GATE-2 (OFF == PRE-FEATURE + PERF GATE): the SAME idx18 drain loop with the
/// detector OFF must behave exactly as it did before the combo-detector existed —
/// (a) NO early `GameOver` (the natural ~400-beat CR 704.5a death is far outside the
/// 40-beat window), (b) NO `∞` marked, and (c) the loop-detection ring is NEVER
/// populated (proving the per-resolution `normalize_for_loop` sampling cost is gated
/// off in the default configuration).
///
/// REVERT-FAIL:
///  - remove the `is_on()` guard at the §2 ring sampler ⇒ the ring populates ⇒ the
///    `ring stays empty` assertion flips.
///  - (the §3 shortcut guard is pinned separately by PR6-GATE-3 below.)
#[test]
fn pr6_gate_off_drain_is_pre_feature() {
    let Some(mut board) = corpus::build_drain_board(card_db(), corpus::row(18).cards, 200) else {
        return; // export absent: skip
    };
    // Opt the detector OFF (the engine default; build_drain_board flips it ON).
    board.runner.state_mut().loop_detection = LoopDetectionMode::Off;
    corpus::seed_lifegain_cascade(&mut board);
    let trace = corpus::drive_pass_priority(&mut board, 40);
    assert!(
        corpus::first_gameover_beat(&trace).is_none(),
        "with the detector OFF the loop must NOT be shortcut to a win (pre-feature behavior)"
    );
    assert!(
        board.runner.state().unbounded_resources.is_empty(),
        "OFF must never mark ∞ (the detector producer is gated; the debug toggle is untouched)"
    );
    assert!(
        trace.iter().all(|t| t.ring_len == 0),
        "OFF must never populate the loop-detection ring (the sampler's per-resolution clone is gated)"
    );
}

/// PR6-GATE-3 (SHORTCUT GUARD, flag A/B from an identical pre-state): drive the idx18
/// loop ON until the exact pre-win state (populated ring, detector ON), then re-apply
/// the SINGLE winning beat twice from clones that differ ONLY in `loop_detection`:
/// ON ends the game, OFF does not. Because the two probes share a byte-identical
/// populated ring and differ only in the flag, this isolates the §3 shortcut gate from
/// the §2 sampler gate (which PR6-GATE-2 covers).
///
/// REVERT-FAIL: remove the `&& state.loop_detection.is_on()` conjunct at the §3
/// shortcut ⇒ the OFF probe also ends the game ⇒ the `OFF must not GameOver` flips.
#[test]
fn pr6_gate_shortcut_guard_isolated_by_flag() {
    let Some(mut board) = corpus::build_drain_board(card_db(), corpus::row(18).cards, 200) else {
        return; // export absent: skip
    };
    corpus::seed_lifegain_cascade(&mut board);
    // Capture the state from the beat immediately BEFORE the ON drive's GameOver — the
    // exact populated-ring state whose next `PassPriority` fires the §3 shortcut.
    let mut pre_win: Option<GameState> = None;
    for _ in 0..40 {
        if matches!(
            board.runner.state().waiting_for,
            WaitingFor::GameOver { .. }
        ) {
            break;
        }
        let snapshot = board.runner.state().clone();
        if board.runner.act(GameAction::PassPriority).is_err() {
            break;
        }
        if matches!(
            board.runner.state().waiting_for,
            WaitingFor::GameOver { .. }
        ) {
            pre_win = Some(snapshot);
            break;
        }
    }
    let pre = pre_win.expect("idx18 ON must reach a §3-shortcut GameOver within 40 beats");
    assert!(
        !pre.loop_detect_ring.is_empty() && pre.loop_detection.is_on(),
        "the captured pre-win state must carry a populated ring with the detector ON"
    );

    // ON probe: re-applying the winning beat from a clone ends the game (deterministic).
    let on_res = GameRunner::from_state(pre.clone())
        .act(GameAction::PassPriority)
        .expect("re-applying the winning beat must dispatch");
    assert!(
        matches!(on_res.waiting_for, WaitingFor::GameOver { winner: Some(_) }),
        "with loop_detection ON, the captured beat shortcuts the loop to a win"
    );

    // OFF probe: same pre-state and same populated ring, only the flag flipped (set
    // directly on the clone so the ring is RETAINED — this isolates the §3 shortcut
    // flag gate, which is all this probe exercises).
    let mut off_state = pre;
    off_state.loop_detection = LoopDetectionMode::Off;
    assert!(
        !off_state.loop_detect_ring.is_empty(),
        "the OFF probe must keep the populated ring so only the flag differs"
    );
    let off_res = GameRunner::from_state(off_state)
        .act(GameAction::PassPriority)
        .expect("re-applying the winning beat must dispatch");
    assert!(
        !matches!(off_res.waiting_for, WaitingFor::GameOver { .. }),
        "with loop_detection OFF, the SAME populated-ring beat must NOT shortcut the game — \
         only the §3 flag gate differs between the two probes"
    );
}

/// PR6-GATE-4 (CONFIG PROVENANCE + IMMUTABILITY): the combo-detector opt-in is a
/// match-creation setting on `MatchConfig`, projected onto the runtime
/// `GameState::loop_detection` flag by `set_match_config`, and IMMUTABLE during play.
/// The mid-game `SetLoopDetection` action was REMOVED as a security fix — previously
/// any networked seat could flip the whole table's game-ending detector mid-match
/// (PR #4603 review). There is now no `GameAction` that mutates the flag.
///
/// REVERT-FAIL: delete the `self.loop_detection = config.loop_detection` line in
/// `GameState::set_match_config` ⇒ the ON-projection assertion flips (the detector can
/// never be enabled), proving the projection is the sole provenance.
#[test]
fn pr6_gate_config_provenance_and_immutability() {
    let mut state = GameScenario::new().build().state().clone();

    // Default config leaves the detector OFF (pre-feature; opt-in invariant #4603).
    state.set_match_config(MatchConfig::default());
    assert_eq!(
        state.loop_detection,
        LoopDetectionMode::Off,
        "default MatchConfig must leave the detector OFF"
    );

    // Opting in via config projects ON onto the runtime gate.
    state.set_match_config(MatchConfig {
        loop_detection: LoopDetectionMode::On,
        ..MatchConfig::default()
    });
    assert!(
        state.loop_detection.is_on(),
        "set_match_config must project MatchConfig.loop_detection onto the runtime flag"
    );

    // Immutability: no `GameAction` mutates the flag, so driving a beat leaves it at
    // the value set at creation. (PassPriority may be a no-op/illegal from this state;
    // either way the flag must be invariant.)
    let before = state.loop_detection;
    let mut runner = GameRunner::from_state(state);
    let _ = runner.act(GameAction::PassPriority);
    assert_eq!(
        runner.state().loop_detection,
        before,
        "loop_detection is immutable during play (set only at creation by set_match_config)"
    );
}

/// PR6-GATE-5 (LOOP-EQUALITY EXCLUSION): `loop_detection` is control state, excluded
/// from `impl PartialEq for GameState` (and therefore from `loop_states_equal`), like
/// `unbounded_resources`. Two states identical except for the flag must compare EQUAL,
/// or CR 732.2a loop detection / AI-search dedup would see spurious differences.
///
/// REVERT-FAIL: add `&& self.loop_detection == other.loop_detection` to the manual
/// `PartialEq` ⇒ `a == b` becomes false ⇒ both assertions flip.
#[test]
fn pr6_gate_loop_detection_excluded_from_equality() {
    let base = GameScenario::new().build().state().clone();
    let mut a = base.clone();
    let mut b = base;
    a.loop_detection = LoopDetectionMode::On;
    b.loop_detection = LoopDetectionMode::Off;
    assert_ne!(
        a.loop_detection, b.loop_detection,
        "fixture must actually differ in loop_detection"
    );
    assert_eq!(
        a, b,
        "manual PartialEq must EXCLUDE loop_detection (control state)"
    );
    assert!(
        crate::types::game_state::loop_states_equal(&a, &b),
        "CR 732.2a loop_states_equal must EXCLUDE loop_detection"
    );
}

/// PR6-GATE-6 (SERDE BACK-COMPAT): a serialized `GameState` predating this field must
/// deserialize with `loop_detection == Off` (the `#[serde(default)]`), so existing
/// wire/saved states load as pre-feature. Non-vacuous: dropping `#[serde(default)]`
/// makes the deserialize fail.
#[test]
fn pr6_gate_loop_detection_serde_defaults_off() {
    let state = GameScenario::new().build().state().clone();
    let mut json = serde_json::to_value(&state).expect("serialize");
    // Simulate a pre-field wire state by removing the key.
    json.as_object_mut()
        .expect("state serializes as an object")
        .remove("loop_detection");
    let restored: GameState = serde_json::from_value(json).expect("deserialize without the field");
    assert_eq!(
        restored.loop_detection,
        LoopDetectionMode::Off,
        "a state without `loop_detection` must default to Off (pre-feature)"
    );
}

/// MP-B (CR 104.2a last-standing): a 4-player table where P0 runs an UNTARGETED
/// "whenever you gain life, each opponent loses life" drain (Marauding Blight-Priest +
/// Bloodthirsty Conqueror), all three opponents at 1 life. One drain cycle takes all
/// three to 0; the CR 704.5a state-based actions eliminate them in ONE simultaneous
/// batch and CR 104.2a leaves P0 the sole survivor → `GameOver { winner: Some(P0) }`.
/// This is the NATURAL win (real SBA deaths), not the 2-player §3 shortcut (gated off
/// at >2 living) — the production path the commander audit validated as safe.
///
/// DISCRIMINATION (the audit's revert-probe, run inline): swapping to a TARGETED
/// "target opponent loses life" drain (Sanguine Bond) drains only ONE chosen opponent
/// per cycle, so it cannot eliminate all three at once — it stops for an opponent
/// choice and never reaches a GameOver. If the untargeted assertion were vacuous, this
/// contrast (same life totals, different drain shape) would not diverge.
#[test]
fn mp_each_opponent_drain_natural_win_continues_to_sole_survivor() {
    let db = card_db();
    let Some(mut board) = corpus::build_drain_board_n(
        db,
        &["Marauding Blight-Priest", "Bloodthirsty Conqueror"],
        4,
        &[1, 1, 1],
        40,
    ) else {
        return; // export absent (CI / fresh checkout): skip, never fail spuriously
    };
    corpus::seed_lifegain_cascade(&mut board);
    let trace = corpus::drive_pass_priority(&mut board, 40);
    let (_, winner) = corpus::first_gameover_beat(&trace)
        .expect("untargeted each-opponent drain must eliminate every opponent → a winner");
    assert_eq!(winner, P0, "the sole surviving player P0 wins (CR 104.2a)");
    let eliminated: Vec<bool> = board
        .runner
        .state()
        .players
        .iter()
        .map(|p| p.is_eliminated)
        .collect();
    assert_eq!(
        eliminated,
        vec![false, true, true, true],
        "all three opponents eliminated in one simultaneous SBA batch; P0 survives"
    );

    // DISCRIMINATION: a targeted single-opponent drain at the same life totals cannot
    // auto-eliminate the whole table — it stops for an opponent choice, no auto-win.
    let Some(mut targeted) =
        corpus::build_drain_board_n(db, &["Sanguine Bond", "Exquisite Blood"], 4, &[1, 1, 1], 40)
    else {
        return;
    };
    corpus::seed_lifegain_cascade(&mut targeted);
    let ttrace = corpus::drive_pass_priority(&mut targeted, 40);
    assert!(
        corpus::first_gameover_beat(&ttrace).is_none(),
        "a targeted single-opponent drain cannot auto-eliminate all opponents — no auto-win"
    );
}

/// MP-D (CR 601/608 targeted resolution): a 4-player table where P0 runs a TARGETED
/// "whenever you gain life, target opponent loses that much life" drain (Sanguine Bond +
/// Exquisite Blood) with THREE legal opponents. `auto_select_targets` refuses to choose
/// among 2+ legal targets, so the trigger STOPS at a target-selection window — it never
/// silently auto-resolves into a game-ending drain. This is why the targeted shape is
/// safe in multiplayer: a human (or AI) must pick the victim each iteration.
///
/// DISCRIMINATION: the SAME cards with a SINGLE legal opponent (2-player) auto-resolve
/// to the sole target with no selection window. So the stop is caused by 2+ legal
/// opponents, not by the card — proving the assertion is not vacuous.
#[test]
fn mp_targeted_drain_stops_for_opponent_choice() {
    let db = card_db();
    let Some(mut board) = corpus::build_drain_board_n(
        db,
        &["Sanguine Bond", "Exquisite Blood"],
        4,
        &[20, 20, 20],
        40,
    ) else {
        return; // export absent: skip
    };
    corpus::seed_lifegain_cascade(&mut board);
    let trace = corpus::drive_pass_priority(&mut board, 30);
    assert!(
        corpus::first_gameover_beat(&trace).is_none(),
        "a targeted drain with 3 legal opponents must not auto-resolve to a GameOver"
    );
    let last = trace.last().expect("at least one beat");
    match &last.wf {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            assert_eq!(
                target_slots[0].legal_targets.len(),
                3,
                "all three opponents are legal targets (got {:?})",
                target_slots[0].legal_targets
            );
        }
        other => panic!("expected a target-selection stop, got {other:?}"),
    }

    // DISCRIMINATION: with a SINGLE legal opponent (2-player), the trigger auto-resolves
    // to the sole target — no selection window ever appears.
    let Some(mut duel) =
        corpus::build_drain_board_n(db, &["Sanguine Bond", "Exquisite Blood"], 2, &[20], 40)
    else {
        return;
    };
    corpus::seed_lifegain_cascade(&mut duel);
    let dtrace = corpus::drive_pass_priority(&mut duel, 30);
    assert!(
        dtrace
            .iter()
            .all(|t| !matches!(t.wf, WaitingFor::TriggerTargetSelection { .. })),
        "with a single legal opponent the targeted drain auto-resolves — no selection stop"
    );
}

/// MP-A (CR 104.4b mandatory DRAW): the live mandatory-loop draw gate fires
/// `GameOver { winner: None }` when a mandatory iteration repeats a prior NORMALIZED
/// state — a cycle that made NO net progress. That decision hinges on the exact
/// predicate `loop_states_equal(normalize_for_loop(a), normalize_for_loop(b))` (the
/// engine `apply`-loop CR 104.4b block). At a 4-player (Commander, no range-of-influence)
/// table a true net-ZERO unbreakable loop is a WHOLE-GAME DRAW — never a single winner:
/// every seat is alive and no resource changed, so the repeat must compare EQUAL (→ the
/// gate draws) AND no faller can be named (→ not a win). The complementary net-PROGRESS
/// cycle (one opponent's life fell) must NOT compare equal (→ the gate does not fire and
/// the game continues — no wrongful draw of a still-advancing loop).
///
/// Scope: this pins the 4-player correctness of the draw gate's equality predicate and
/// the no-false-winner guarantee. A full live auto-resolve of a net-zero loop to
/// `GameOver{winner:None}` needs a specific net-zero mandatory combo, which the
/// committed card fixture does not contain (every corpus loop is net-progress).
///
/// REVERT-FAIL: the net-progress half — were `loop_states_equal` to ignore life totals,
/// the faller state would compare EQUAL and a still-progressing loop would wrongly draw;
/// the `assert!(!loop_states_equal(...))` flips.
#[test]
fn mp_unbreakable_net_zero_mandatory_loop_whole_game_draw() {
    let base = GameState::new(crate::types::format::FormatConfig::standard(), 4, 7);
    assert_eq!(
        base.players.iter().filter(|p| !p.is_eliminated).count(),
        4,
        "fixture sanity: four living players (no range-of-influence)"
    );

    // NET-ZERO repeat ⇒ byte-identical normalized state ⇒ the CR 104.4b gate draws.
    assert!(
        crate::types::game_state::loop_states_equal(
            &base.normalize_for_loop(),
            &base.normalize_for_loop(),
        ),
        "a net-zero 4-player repeat must compare EQUAL — the gate then draws (winner:None)"
    );
    // A net-zero cycle has no life faller ⇒ no player is named a winner (it is a draw).
    let zero = crate::analysis::resource::ResourceVector::default();
    assert_eq!(
        crate::analysis::loop_check::live_mandatory_loop_winner(&base, &base, &zero),
        None,
        "a net-zero loop is a draw, never a single winner — no faller to lose"
    );

    // NET-PROGRESS contrast (the revert-probe): one opponent's life fell, so the cycle
    // did NOT return to the same state — the equality is FALSE and the draw gate must
    // not fire.
    let mut progressed = base.clone();
    progressed.players[1].life -= 1;
    assert!(
        !crate::types::game_state::loop_states_equal(
            &base.normalize_for_loop(),
            &progressed.normalize_for_loop(),
        ),
        "a net-progress repeat (an opponent's life fell) must NOT compare equal — \
         the draw gate must not fire on a still-advancing loop"
    );
}

/// C-neg-D — ring HYGIENE: a finite, non-refilling multi-spell stack that drains to
/// empty NEVER accumulates loop snapshots and NEVER produces a `GameOver`. Three
/// costless "gain 1 life" abilities are stacked, then resolved one-per-beat; each
/// resolution SHRINKS the stack, so the §2 refill gate's `stack.len() >=
/// stack_len_before` clause is false ⇒ the clear arm runs ⇒ the ring stays empty.
///
/// REVERT-FAIL: removing the `state.stack.len() >= stack_len_before` clause from the
/// §2 refill gate makes a normal shrinking resolution RECORD a snapshot ⇒ the ring
/// becomes non-empty ⇒ the `is_empty()` assertion flips.
#[test]
fn drive_finite_stack_keeps_ring_empty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 40);
    scenario.with_life(P1, 40);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
    }
    // A board-neutral costless "gain 1 life" ability — resolvable repeatedly with no
    // decking SBA and no triggers on this synthetic board, so the only effect of each
    // resolution is to SHRINK the stack.
    let artifact = {
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;
        let state = runner.state_mut();
        let id = crate::game::zones::create_object(
            state,
            CardId(state.next_object_id),
            P0,
            "Gain-Life Artifact".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).expect("just created");
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.summoning_sick = false;
        std::sync::Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        ));
        id
    };
    corpus::settle_layers(runner.state_mut());
    let gain_idx = corpus::ability_index_where(runner.state(), artifact, |e| {
        matches!(e, Effect::GainLife { .. })
    })
    .expect("the artifact has a gain-life ability");
    // Stack three non-refilling abilities (each ActivateAbility clears the ring, the
    // §2.3 invalidation — so we start each measurement from an empty ring).
    for _ in 0..3 {
        runner
            .act(GameAction::ActivateAbility {
                source_id: artifact,
                ability_index: gain_idx,
            })
            .expect("activate costless gain-life");
    }
    assert_eq!(runner.state().stack.len(), 3, "three abilities stacked");
    // Drive PassPriority ONLY until the finite stack drains to empty. The ring must
    // stay empty at EVERY shrinking-resolution beat, and no shortcut GameOver may
    // occur while the stack is non-empty.
    let mut resolutions = 0;
    let mut max_ring = 0usize;
    for _ in 0..20 {
        if runner.state().stack.is_empty() {
            break;
        }
        assert!(
            !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
            "a finite non-loop stack must not be shortcut to a GameOver"
        );
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
        max_ring = max_ring.max(runner.state().loop_detect_ring.len());
        resolutions += 1;
    }
    assert!(
        runner.state().stack.is_empty(),
        "the finite stack must drain to empty within the window"
    );
    assert_eq!(
        max_ring, 0,
        "a shrinking finite stack must never record a loop snapshot (ring stayed empty)"
    );
    assert!(resolutions >= 3, "the drive must have processed real beats");
}
