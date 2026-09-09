//! Reproduction probes for the two open Swords to Plowshares target-legality
//! defects, which are opposite failure modes of the same CR 608.2b recheck.
//!
//! Card (verified against the Scryfall API and against the committed IR
//! snapshot
//! `parser/oracle_ir/snapshots/engine__parser__oracle_ir__snapshot_tests__swords_to_plowshares_lowered.snap`):
//! > Exile target creature. Its controller gains life equal to its power.
//!
//! Clause by clause: one target, a creature; and "its controller"/"its power"
//! are anaphors to that same target. So BOTH clauses depend on the one target.
//! CR 608.2b: if every target is illegal as the spell tries to resolve, the
//! spell doesn't resolve and NONE of its effects happen — no exile, no life.
//!
//!   - #5965: the target gains hexproof while the spell is on the stack, so the
//!     spell must fizzle. MEASURED at BASE with a fixture that genuinely grants
//!     hexproof (both keyword stores — see the CR 613.1 note below): BOTH halves
//!     are red — the creature IS exiled AND its controller gains life equal to
//!     its power.
//!
//!     The exile does NOT mean the resolution-time recheck is broken. Measured
//!     on the resolving chain at BASE, the head node's re-validation is
//!     CORRECT: it drops the hexproofed target and its validated `targets` is
//!     empty (CR 702.11b, via `can_target`). The exile happens further
//!     downstream, because the chain failed to fizzle (the rider's propagated
//!     snapshot kept `legal_targets` non-empty), so `execute_effect` ran the
//!     head anyway — and `change_zone::resolve` treats empty `targets` as "this
//!     is an UNTARGETED effect" and falls through to its resolution-time
//!     zone-scan, which re-derives a subject the spell never legally targeted.
//!
//!     That zone-scan fallback is a SECOND, INDEPENDENT defect. It is not fixed
//!     here and it must get its own issue: this fix only makes it unreachable
//!     for this card, because the chain now fizzles before `execute_effect` is
//!     called at all. A chain with two instances of "target" where only one
//!     becomes illegal still reaches it (CR 608.2b fizzles only when ALL targets
//!     are illegal) — e.g. Grave Exchange, "Return target creature card from
//!     your graveyard to your hand. Target player sacrifices a creature of
//!     their choice."
//!   - #8058: the target dies before resolution. The spell correctly moves the
//!     creature nowhere, but its controller still gains life (measured at BASE:
//!     P1 25 vs 20).

use super::rules::{GameScenario, Phase, P0, P1};
use engine::game::layers::evaluate_layers;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::zones::Zone;

const SWORDS: &str = "Exile target creature. Its controller gains life equal to its power.";

/// CR 608.2b + CR 702.11b (issue #5965): hexproof granted while Swords is on
/// the stack makes the target illegal for an opponent's spell, so the spell is
/// countered on resolution — the creature is NOT exiled and no life is gained.
///
/// READ THIS BEFORE TRUSTING THE ZONE ASSERTION. `zone == Battlefield` here is
/// an invariant the FIZZLE provides, and nothing more. It is NOT evidence that
/// `change_zone::resolve` handles a dropped target correctly — it demonstrably
/// does not. Measured at BASE: the head's re-validation already emptied its
/// `targets` (the CR 702.11b recheck works), and the creature was exiled ANYWAY
/// by `change_zone::resolve`'s untargeted zone-scan fallback. This assertion is
/// green after the fix only because the spell is now countered at the
/// `check_fizzle` site and `execute_effect` is never reached. If a future change
/// makes this chain resolve again, this assertion will go red for a reason that
/// has nothing to do with target re-validation — fix the zone-scan fallback,
/// do not weaken this assertion. The revert-failing assertion for THIS fix is
/// `life(P1)`.
#[test]
fn issue_5965_hexproof_granted_mid_stack_fizzles_swords() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // P1's creature is the target; P0 casts the removal, so hexproof applies
    // (CR 702.11b scopes hexproof to spells an OPPONENT controls).
    let creature = scenario.add_creature(P1, "Hexproof Target", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    // Put the spell on the stack WITHOUT resolving it.
    runner.cast(spell).target_object(creature).commit();

    // Reach-guard: the creature is a legal target at announcement.
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "reach-guard: the target must be on the battlefield when the spell is cast"
    );

    // Grant hexproof while the spell is on the stack.
    let obj = runner.state_mut().objects.get_mut(&creature).unwrap();
    obj.keywords.push(Keyword::Hexproof);
    // CR 613.1: the grant must survive the layers reseed.
    // `sync_missing_base_characteristics` (game_object.rs:2290) early-returns
    // once `base_characteristics_initialized` is set — which it sets ITSELF at
    // :2365 — so the :2322 whole-vector back-fill cannot be relied on.
    // `seed_live_characteristics_from_base` (layers.rs:2409) then restores
    // `keywords` from `base_keywords` on every pass, wiping a live-only push.
    // Same idiom as scenario.rs:1010 `push_keyword`; casting_tests.rs:23527
    // does exactly this for Hexproof. Do this push LAST: writing to
    // `base_keywords` while it is empty falsifies the `is_empty()` guard on the
    // back-fill at game_object.rs:2321-2322, so a keyword set on the fixture
    // BEFORE this push would be dropped from base. Inert here —
    // `add_creature` grants no other keyword.
    obj.base_keywords.push(Keyword::Hexproof);
    evaluate_layers(runner.state_mut());

    // Reach-guard: without this the fizzle assertions below could pass for the
    // wrong reason (a target that was never hexproof is not a fizzle test).
    assert!(
        runner.state().objects[&creature].has_keyword(&Keyword::Hexproof),
        "reach-guard: the mid-stack grant must survive the CR 613.1 layers reseed \
         (`seed_live_characteristics_from_base` restores `keywords` from \
         `base_keywords` on every pass — layers.rs:2409 — which is why the grant \
         must be written to BOTH stores) — without this, the fizzle assertion \
         below could pass for the wrong reason"
    );

    runner.advance_until_stack_empty();

    // Reach-guard (CR 608.2b): the assertion below is equally satisfied by "the
    // spell never resolved". This pair pins that the spell LEFT THE STACK and
    // reached its owner's graveyard, which distinguishes "fizzled" from "never
    // resolved". That is ALL it proves: it does not prove countering, because an
    // instant that resolves normally is also put into its owner's graveyard
    // (CR 608.2n). What proves the effects did not happen is the assertion
    // below — never delete that one on the strength of this pair.
    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: the spell must have left the stack"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "CR 608.2b: a spell countered on resolution is removed from the stack and \
         put into its owner's graveyard — this distinguishes 'fizzled' from \
         'never resolved'"
    );

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "CR 608.2b: the spell must fizzle — a hexproof creature is an illegal \
         target for an opponent's spell, so it is NOT exiled"
    );
    assert_eq!(
        runner.life(P1),
        p1_life_before,
        "CR 608.2b: a countered-on-resolution spell does none of its effects, \
         so no life is gained either"
    );
}

/// POSITIVE INSTRUMENT CONTROL for BOTH `SWORDS` regression pins above and
/// below. Every assertion those two tests make is an ABSENCE — nothing exiled,
/// no life gained, spell in the graveyard. If `SWORDS` ever stopped lowering to
/// `ChangeZone` + the `GainLife` anaphor rider — a parse regression to
/// `Effect::Unimplemented`, or a lost rider — all of those absences would still
/// hold and both pins would rot silently to green. Nothing upstream catches it:
/// `add_spell_to_hand_from_oracle` asserts nothing about the parse, and
/// `SpellCast::try_commit` never checks that `.target_object(..)` was actually
/// consumed by a surfaced slot (`CastCommit` has no `Drop`), so a target handed
/// to a spell that surfaces no slot is discarded in silence.
///
/// This test is the only thing in the file that proves the instrument is alive
/// for the PRIMARY card: `SWORDS` on a legal target really does exile it AND
/// really does gain its controller life equal to its power.
///
/// NOTE FOR THE NEXT MAINTAINER — apply this to the CLASS, not the instance.
/// The file already stated this doctrine and enforced it for the *control*
/// (`bare_exile_exiles_legal_target`, below) while leaving the primary card
/// uncovered. That instance-not-class gap is the single error shape that has
/// recurred through this entire investigation. Every negative regression pin
/// here needs a paired positive that proves its instrument still works — if you
/// add one, add the pair.
#[test]
fn swords_exiles_and_gains_life_on_legal_target() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Same fixture as the two pins, minus the hexproof grant and minus the
    // death — so the ONLY difference is that the target stays legal.
    let creature = scenario.add_creature(P1, "Legal Target", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    runner.cast(spell).target_object(creature).commit();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "reach-guard: the target must be on the battlefield when the spell is cast"
    );

    runner.advance_until_stack_empty();

    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: the spell must have left the stack"
    );

    // CR 608.2b: a spell that resolves normally DOES both of its things. These
    // two assertions are what prove the instrument is alive; if either goes red,
    // the two fizzle pins in this file are no longer measuring anything.
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Exile,
        "instrument check: `SWORDS` must actually EXILE a legal target — if it \
         does not, the ChangeZone head is gone and both fizzle pins in this file \
         pass vacuously"
    );
    assert_eq!(
        runner.life(P1),
        p1_life_before + 2,
        "instrument check: `SWORDS`'s anaphoric rider must actually fire on a \
         legal target — the 2/2's controller gains life equal to its power. If \
         this is red, the GainLife rider is gone and `issue_8058`'s and \
         `issue_5965`'s life assertions pass vacuously"
    );
}

/// CR 608.2b (issue #8058): the target dies before Swords resolves. The spell
/// is countered on resolution, so the life-gain clause must NOT fire.
#[test]
fn issue_8058_dead_target_gains_no_life() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_creature(P1, "Doomed Target", 5, 5).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    runner.cast(spell).target_object(creature).commit();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "reach-guard: the target must be live when the spell is cast"
    );

    // The target dies while Swords is still on the stack.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), creature, Zone::Graveyard, &mut events);

    runner.advance_until_stack_empty();

    // Reach-guard (CR 608.2b): the assertion below is equally satisfied by "the
    // spell never resolved". This pair pins that the spell LEFT THE STACK and
    // reached its owner's graveyard, which distinguishes "fizzled" from "never
    // resolved". That is ALL it proves: it does not prove countering, because an
    // instant that resolves normally is also put into its owner's graveyard
    // (CR 608.2n). What proves the effects did not happen is the assertion
    // below — never delete that one on the strength of this pair.
    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: the spell must have left the stack"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "CR 608.2b: a spell countered on resolution is removed from the stack and \
         put into its owner's graveyard — this distinguishes 'fizzled' from \
         'never resolved'"
    );

    assert_eq!(
        runner.life(P1),
        p1_life_before,
        "CR 608.2b: Swords is countered on resolution because its only target is \
         gone, so 'its controller gains life equal to its power' must NOT fire"
    );
    assert_ne!(
        runner.state().objects[&creature].zone,
        Zone::Exile,
        "a countered spell exiles nothing"
    );
}

/// A bare exile with NO anaphoric rider — the control's instrument.
const BARE_EXILE: &str = "Exile target creature.";

/// CR 608.2b + CR 702.11b — CONTROL, and the falsifier for this fix's premise.
/// A bare "Exile target creature." has no rider, so nothing can retain a
/// propagated target: the head node's own re-validation is the whole story.
/// This must pass at BASE_SHA as well as after the fix. If it FAILS at BASE,
/// the resolution-time hexproof recheck is independently broken and the
/// chain-retention fix is not sufficient.
///
/// The positive reach-guard that this control is not measuring a dead
/// instrument lives in the sibling `bare_exile_exiles_legal_target`, which
/// proves the very same `BARE_EXILE` text CAN exile. Without that sibling every
/// assertion here is equally satisfied by "the spell resolved and did nothing"
/// (e.g. the text lowering to `Effect::Unimplemented`), and a falsifier that
/// passes when the instrument is dead falsifies nothing.
#[test]
fn bare_exile_fizzles_under_mid_stack_hexproof() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // P1's creature is the target; P0 casts the removal, so hexproof applies
    // (CR 702.11b scopes hexproof to spells an OPPONENT controls).
    let creature = scenario.add_creature(P1, "Hexproof Target", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bare Exile", true, BARE_EXILE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    // Put the spell on the stack WITHOUT resolving it.
    runner.cast(spell).target_object(creature).commit();

    // Reach-guard: the creature is a legal target at announcement.
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "reach-guard: the target must be on the battlefield when the spell is cast"
    );

    // Grant hexproof while the spell is on the stack. Both stores — see the
    // CR 613.1 note in `issue_5965_hexproof_granted_mid_stack_fizzles_swords`;
    // `layers.rs:2409` restores `keywords` from `base_keywords` on every pass.
    // This push goes LAST (game_object.rs:2321-2322 back-fill guard).
    let obj = runner.state_mut().objects.get_mut(&creature).unwrap();
    obj.keywords.push(Keyword::Hexproof);
    obj.base_keywords.push(Keyword::Hexproof);
    evaluate_layers(runner.state_mut());

    assert!(
        runner.state().objects[&creature].has_keyword(&Keyword::Hexproof),
        "reach-guard: the mid-stack grant must survive the CR 613.1 layers reseed \
         (`seed_live_characteristics_from_base` restores `keywords` from \
         `base_keywords` on every pass — layers.rs:2409 — which is why the grant \
         must be written to BOTH stores) — without this, the fizzle assertion \
         below could pass for the wrong reason"
    );

    runner.advance_until_stack_empty();

    // Reach-guard (CR 608.2b): the assertion below is equally satisfied by "the
    // spell never resolved". This pair pins that the spell LEFT THE STACK and
    // reached its owner's graveyard, which distinguishes "fizzled" from "never
    // resolved". That is ALL it proves: it does not prove countering, because an
    // instant that resolves normally is also put into its owner's graveyard
    // (CR 608.2n). What proves the effect did not happen is the assertion
    // below — never delete that one on the strength of this pair.
    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: the spell must have left the stack"
    );
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "CR 608.2b: a spell countered on resolution is removed from the stack and \
         put into its owner's graveyard — this distinguishes 'fizzled' from \
         'never resolved'"
    );

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "CR 608.2b + CR 702.11b: with NO rider to retain a propagated target, the \
         head node's own re-validation is the whole story — a hexproof creature \
         is an illegal target for an opponent's spell, so the spell is countered \
         on resolution and the creature is NOT exiled. If this is red at BASE, \
         the resolution-time hexproof recheck is independently broken"
    );
}

/// POSITIVE REACH-GUARD for `bare_exile_fizzles_under_mid_stack_hexproof`.
/// Every assertion that control carries is equally satisfied by "the spell
/// resolved and did nothing", so the negative result only means something if
/// this exact `BARE_EXILE` text CAN exile a legal target. Identical fixture
/// minus the hexproof grant.
#[test]
fn bare_exile_exiles_legal_target() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_creature(P1, "Hexproof Target", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Bare Exile", true, BARE_EXILE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    runner.cast(spell).target_object(creature).commit();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "reach-guard: the target must be on the battlefield when the spell is cast"
    );

    runner.advance_until_stack_empty();

    assert!(
        runner.state().stack.is_empty(),
        "reach-guard: the spell must have left the stack"
    );

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Exile,
        "instrument check: `Exile target creature.` must actually exile a LEGAL \
         target, or the sibling control's negative result proves nothing"
    );
}
