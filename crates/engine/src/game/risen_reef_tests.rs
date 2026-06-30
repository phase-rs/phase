//! Regression tests for issue #4385 (Discord: "Risen Reef ... Comes back to
//! hand after it's cast ad infinitum").
//!
//! The reported hypothesis was that `TargetFilter::ParentTarget` in the
//! `Dig { keep_count: 0 } -> ChangeZone(Battlefield) -> ChangeZone(Hand)`
//! chain resolves to the trigger SOURCE (Risen Reef itself) instead of the
//! card that was dug, because a pure-peek `Dig` (`keep_count: 0`, used for
//! "look at the top card" with no player selection) never populates
//! `ability.targets` the way an interactive `DigChoice` selection would.
//!
//! Investigation (`crates/engine/src/game/targeting.rs::resolved_targets`,
//! `crates/engine/src/game/effects/mod.rs::resolve_ability_chain`) found that
//! this exact failure mode IS guarded against: when the parent effect is a
//! `Dig`/`RevealTop`/etc. (`effect_writes_last_revealed_ids`) and writes
//! `state.last_revealed_ids` with no chosen targets of its own, the chain
//! walker injects the revealed card id as the parent's `targets` BEFORE
//! evaluating the sub-ability's condition and BEFORE recursing into the
//! sub's own resolution (mod.rs ~6617-6627, ~6943-6964) — only falling back
//! to `ParentTarget -> source` (targeting.rs `resolved_targets`, the
//! `use_self` branch) when `last_revealed_ids` is genuinely empty (a "look at
//! the top card" against an empty library). This generalized fix is the
//! direct descendant of issue #2871 (Currency Converter,
//! `subject_dependent_type_condition_has_no_subject`) and is exercised here
//! end-to-end through the real cast/trigger pipeline for the whole "look at
//! the top card of your library, if it's a land card you may put it onto the
//! battlefield [tapped], else put it into your hand" Bloomburrow/Zendikar
//! Rising cycle (Risen Reef, Rulik Mons Warren Chief, Fecund Greenshell, and
//! siblings sharing the identical Dig/ChangeZone parser shape).
//!
//! CR 608.2c (instructions resolve in the order written / "if you do" gating)
//! + CR 701.20e (look at a card) + CR 614.1c (battlefield-entry triggers).

#![cfg(test)]

use crate::game::combat::AttackTarget;
use crate::game::scenario::GameScenario;
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::game_state::WaitingFor;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

const RISEN_REEF_ORACLE: &str = "Whenever this creature or another Elemental you control enters, look at the top card of your library. If it's a land card, you may put it onto the battlefield tapped. If you don't put the card onto the battlefield, put it into your hand.";

const RULIK_MONS_ORACLE: &str = "Menace\nWhenever Rulik Mons attacks, look at the top card of your library. If it's a land card, you may put it onto the battlefield tapped. If you didn't put a card onto the battlefield this way, create a 1/1 red Goblin creature token.";

const FECUND_GREENSHELL_ORACLE: &str = "Reach\nAs long as you control ten or more lands, creatures you control get +2/+2.\nWhenever this creature or another creature you control with toughness greater than its power enters, look at the top card of your library. If it's a land card, you may put it onto the battlefield tapped. Otherwise, put it into your hand.";

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

// ───────────────────────── Risen Reef: cast-from-hand (self ETB) ──────────────────────────

/// Regression guard for the literal reported bug: casting Risen Reef must
/// never put Risen Reef ITSELF into hand via its own ETB trigger, regardless
/// of whether the top card is a land. Drives the REAL cast pipeline
/// (`runner.cast(..).resolve()`), not a hand-built `ResolvedAbility`, so it
/// exercises the same `apply()` -> stack -> trigger -> resolve path a real
/// game uses.
#[test]
fn risen_reef_cast_never_bounces_itself_to_hand_land_on_top() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reef = scenario
        .add_creature_to_hand_from_oracle(P0, "Risen Reef", 1, 1, RISEN_REEF_ORACLE)
        .id();
    let top = scenario.add_card_to_library_top(P0, "Top Card");
    {
        let obj = scenario.state.objects.get_mut(&top).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
    }
    let mut runner = scenario.build();

    let outcome = runner.cast(reef).resolve();
    assert_eq!(
        outcome.zone_of(reef),
        Zone::Battlefield,
        "Risen Reef must stay on the battlefield, not bounce to hand via its own ETB trigger"
    );
}

#[test]
fn risen_reef_cast_never_bounces_itself_to_hand_no_land_on_top() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reef = scenario
        .add_creature_to_hand_from_oracle(P0, "Risen Reef", 1, 1, RISEN_REEF_ORACLE)
        .id();
    let top = scenario.add_card_to_library_top(P0, "Top Card (nonland)");
    let mut runner = scenario.build();

    let outcome = runner.cast(reef).resolve();
    assert_eq!(
        outcome.zone_of(reef),
        Zone::Battlefield,
        "Risen Reef must stay on the battlefield even when no land is found"
    );
    // CR 608.2c: "If you don't put the card onto the battlefield, put it into
    // your hand" — the LOOKED-AT card (not Risen Reef) goes to hand.
    assert_eq!(
        outcome.zone_of(top),
        Zone::Hand,
        "the non-land top card (not Risen Reef) must go to hand"
    );
}

/// CR 608.2c: a SECOND Elemental's ETB ("another Elemental you control
/// enters") must trigger Risen Reef's ability without ever moving Risen Reef
/// itself, confirming the bug isn't specific to the self-referential trigger
/// half of the "this creature or another Elemental" condition.
#[test]
fn risen_reef_triggers_off_sibling_elemental_without_moving_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reef = scenario
        .add_creature_from_oracle(P0, "Risen Reef", 1, 1, RISEN_REEF_ORACLE)
        .id();
    let elemental = scenario
        .add_creature_to_hand(P0, "Sibling Elemental", 2, 2)
        .id();
    {
        let obj = scenario.state.objects.get_mut(&elemental).unwrap();
        obj.card_types.subtypes.push("Elemental".to_string());
        obj.base_card_types = obj.card_types.clone();
    }
    let top = scenario.add_card_to_library_top(P0, "Top Card");
    {
        let obj = scenario.state.objects.get_mut(&top).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
    }
    let mut runner = scenario.build();

    let outcome = runner.cast(elemental).resolve();
    assert_eq!(
        outcome.zone_of(reef),
        Zone::Battlefield,
        "Risen Reef must not move when a SIBLING Elemental's ETB triggers its ability"
    );
    assert_eq!(
        outcome.zone_of(elemental),
        Zone::Battlefield,
        "the sibling Elemental itself must resolve onto the battlefield normally"
    );
}

/// CR 608.2c + CR 614.1c: accepting the optional "put it onto the
/// battlefield tapped" must put the LOOKED-AT card (not Risen Reef) onto the
/// battlefield, tapped — never Risen Reef itself.
#[test]
fn risen_reef_accept_puts_dug_land_on_battlefield_tapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reef = scenario
        .add_creature_to_hand_from_oracle(P0, "Risen Reef", 1, 1, RISEN_REEF_ORACLE)
        .id();
    let top = scenario.add_card_to_library_top(P0, "Dug Land");
    {
        let obj = scenario.state.objects.get_mut(&top).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
    }
    let mut runner = scenario.build();

    let outcome = runner.cast(reef).accept_optional().resolve();

    assert_eq!(
        outcome.zone_of(reef),
        Zone::Battlefield,
        "Risen Reef must remain on the battlefield, not become the dug object"
    );
    assert_eq!(
        outcome.zone_of(top),
        Zone::Battlefield,
        "the dug land must be the object that enters the battlefield"
    );
    assert!(
        outcome.state().objects[&top].tapped,
        "Risen Reef puts the dug land onto the battlefield TAPPED"
    );
}

/// CR 608.2c: declining the optional "put it onto the battlefield" must put
/// the dug land into hand — Risen Reef itself never moves either way.
#[test]
fn risen_reef_decline_puts_dug_land_into_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reef = scenario
        .add_creature_to_hand_from_oracle(P0, "Risen Reef", 1, 1, RISEN_REEF_ORACLE)
        .id();
    let top = scenario.add_card_to_library_top(P0, "Dug Land");
    {
        let obj = scenario.state.objects.get_mut(&top).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
    }
    let mut runner = scenario.build();

    let outcome = runner.cast(reef).decline_optional().resolve();

    assert_eq!(outcome.zone_of(reef), Zone::Battlefield);
    assert_eq!(
        outcome.zone_of(top),
        Zone::Hand,
        "declining must put the dug land into hand, per \
         'If you don't put the card onto the battlefield, put it into your hand'"
    );
}

// ───────────────────────── Sibling cards in the same Oracle family ────────────────────────

/// Rulik Mons, Warren Chief: same Dig/ChangeZone skeleton as Risen Reef but
/// triggers on ATTACK (not ETB) and has a "didn't put a card onto the
/// battlefield -> create a token" rider instead of a hand fallback. Confirms
/// the fix generalizes across trigger modes (`Attacks` vs `ChangesZone`) and
/// across the rider's effect type (token creation vs `ChangeZone`).
#[test]
fn rulik_mons_attack_trigger_never_bounces_itself_and_creates_token_when_no_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rulik = scenario
        .add_creature_from_oracle(P0, "Rulik Mons, Warren Chief", 3, 2, RULIK_MONS_ORACLE)
        .id();
    let top = scenario.add_card_to_library_top(P0, "Top Card (nonland)");
    let mut runner = scenario.build();

    runner.advance_to_combat();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ));
    runner
        .declare_attackers(&[(rulik, AttackTarget::Player(P1))])
        .expect("Rulik Mons should be a legal attacker");

    // Drain the stack so the attack trigger fully resolves, answering any
    // optional ("you may put it onto the battlefield") prompt by declining —
    // the non-land top card means there is no legal optional put anyway, but
    // the loop stays generic across both branches of this card family.
    for _ in 0..8 {
        runner.advance_until_stack_empty();
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline the optional put-onto-battlefield");
            }
            _ => break,
        }
    }

    assert_eq!(
        runner.state().objects[&rulik].zone,
        Zone::Battlefield,
        "Rulik Mons must never bounce to hand from its own attack trigger"
    );
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Library,
        "a non-land top card is neither put onto the battlefield nor moved by Rulik Mons \
         (the rider creates a token instead of a hand fallback)"
    );
}

/// CR 608.2c: when Rulik Mons finds a land and the controller accepts the
/// optional put, the land enters tapped and NO Goblin token is created — the
/// `Not(OptionalEffectPerformed)` rider must not fire on the accept branch.
#[test]
fn rulik_mons_accepts_land_no_token_created() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rulik = scenario
        .add_creature_from_oracle(P0, "Rulik Mons, Warren Chief", 3, 2, RULIK_MONS_ORACLE)
        .id();
    let top = scenario.add_card_to_library_top(P0, "Dug Land");
    {
        let obj = scenario.state.objects.get_mut(&top).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
    }
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(rulik, AttackTarget::Player(P1))])
        .expect("Rulik Mons should be a legal attacker");

    let tokens_before = runner.state().battlefield.len();
    for _ in 0..8 {
        runner.advance_until_stack_empty();
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional put-onto-battlefield");
            }
            _ => break,
        }
    }

    assert_eq!(runner.state().objects[&rulik].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Battlefield,
        "the dug land must enter the battlefield"
    );
    assert!(
        runner.state().objects[&top].tapped,
        "Rulik Mons puts the dug land onto the battlefield TAPPED"
    );
    assert_eq!(
        runner.state().battlefield.len(),
        tokens_before + 1,
        "only the dug land should be a new battlefield object — no Goblin token \
         when a land WAS put onto the battlefield"
    );
}

/// Fecund Greenshell: the "this creature or another creature you control
/// with toughness greater than its power" half exercises the structural-cast
/// (not simple subtype) variant of the same trigger condition shape, with an
/// "Otherwise, put it into your hand" rider phrased differently from Risen
/// Reef's "If you don't put the card onto the battlefield". Confirms the
/// `ParentTarget` fix is condition-text-agnostic.
#[test]
fn fecund_greenshell_self_etb_never_bounces_itself_to_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let greenshell = scenario
        .add_creature_to_hand_from_oracle(P0, "Fecund Greenshell", 2, 4, FECUND_GREENSHELL_ORACLE)
        .id();
    let top = scenario.add_card_to_library_top(P0, "Top Card (nonland)");
    let mut runner = scenario.build();

    let outcome = runner.cast(greenshell).resolve();
    assert_eq!(
        outcome.zone_of(greenshell),
        Zone::Battlefield,
        "Fecund Greenshell must never bounce to hand from its own ETB trigger"
    );
    assert_eq!(
        outcome.zone_of(top),
        Zone::Hand,
        "the non-land top card must go to hand per the 'Otherwise' rider"
    );
}
