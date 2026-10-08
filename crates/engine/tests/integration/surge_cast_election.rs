//! Surge cast election — CR 702.117a.
//!
//! CR 702.117a: Surge is an alternative cost (CR 118.9): "You may cast this
//! spell for its surge cost if you or a teammate has cast another spell this
//! turn." Before this fix, a hand card whose only prepared option was the Surge
//! variant never elected it: the cast fell through to a `Normal` cast, so a
//! Surge-only-affordable card was rejected with "Cannot pay mana cost", and a
//! card whose two costs were both payable silently paid the printed cost with
//! no prompt — leaving "if its surge cost was paid" riders unreachable.
//!
//! These tests drive the real `CastSpell` → `AlternativeCastChoice` →
//! `ChooseAlternativeCast` pipeline with cards loaded from the card database
//! (verbatim Oracle text from the committed fixture):
//!
//! - Reckless Bushwhacker `{2}{R}`: "Surge {1}{R} (You may cast this spell for
//!   its surge cost if you or a teammate has cast another spell this turn.)
//!   Haste. When this creature enters, if its surge cost was paid, other
//!   creatures you control get +1/+0 and gain haste until end of turn."
//! - Ornithopter `{0}`: the free "another spell".

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    AlternativeCastKeyword, CastPaymentMode, CastingVariant, SpellCastRecord, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::support::shared_card_db;

const BUSHWHACKER: &str = "Reckless Bushwhacker";
const ORNITHOPTER: &str = "Ornithopter";

fn add_mountains(scenario: &mut GameScenario, count: usize) -> Vec<ObjectId> {
    (0..count)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect()
}

fn tapped_count(runner: &GameRunner, lands: &[ObjectId]) -> usize {
    lands
        .iter()
        .filter(|id| runner.state().objects[id].tapped)
        .count()
}

/// The cast variant recorded for P0's cast of the object `id` this turn
/// (CR 601.2b: latched at announcement onto the per-turn cast record).
fn recorded_variant(runner: &GameRunner, id: ObjectId) -> CastingVariant {
    runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&P0)
        .and_then(|records| {
            records
                .iter()
                .find(|record| record.spell_object_id == Some(id))
        })
        .unwrap_or_else(|| panic!("P0 must have a cast record for {id:?}"))
        .cast_variant
}

fn power_of(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id].power.unwrap_or(0)
}

fn has_haste(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].has_keyword(&Keyword::Haste)
}

fn cast_spell_action(runner: &GameRunner, id: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id: id,
        card_id: runner.state().objects[&id].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

fn mana_cost(generic: u32, shards: Vec<ManaCostShard>) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// Acceptance 1 + 3: with only the surge cost payable, the cast elects Surge
/// directly — but only after another spell has been cast this turn.
#[test]
fn surge_is_the_only_payable_cost_after_another_spell() {
    let db = shared_card_db().expect("card fixture must be present for Surge tests");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountains = add_mountains(&mut scenario, 2);
    let ornithopter = scenario.add_real_card(P0, ORNITHOPTER, Zone::Hand, db);
    let bushwhacker = scenario.add_real_card(P0, BUSHWHACKER, Zone::Hand, db);
    let mut runner = scenario.build();

    // (a) CR 702.117a: no other spell cast this turn → no Surge, and the printed
    // {2}{R} is unpayable from two Mountains, so the cast is refused.
    let action = cast_spell_action(&runner, bushwhacker);
    assert!(
        runner.act(action).is_err(),
        "CR 702.117a: surge is unavailable before another spell is cast this turn"
    );
    assert_eq!(runner.state().objects[&bushwhacker].zone, Zone::Hand);
    assert_eq!(tapped_count(&runner, &mountains), 0);

    // (b) Another spell this turn.
    runner.cast(ornithopter).resolve();

    // (c) CR 702.117a + CR 118.9: now the surge cost {1}{R} is the only payable
    // cost, so the cast elects Surge and the "if its surge cost was paid" rider
    // (CR 603.4) applies.
    runner.cast(bushwhacker).resolve();
    assert_eq!(
        recorded_variant(&runner, bushwhacker),
        CastingVariant::Surge,
        "CR 702.117a: the cast must be for the surge cost"
    );
    assert_eq!(tapped_count(&runner, &mountains), 2);
    assert_eq!(
        power_of(&runner, ornithopter),
        1,
        "CR 603.4: the surge rider gives other creatures +1/+0"
    );
    assert!(
        has_haste(&runner, ornithopter),
        "CR 603.4: the surge rider grants haste"
    );
}

/// Acceptance 2: with both costs payable the engine offers the choice.
#[test]
fn surge_offers_normal_or_surge_when_both_are_payable() {
    let db = shared_card_db().expect("card fixture must be present for Surge tests");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_mountains(&mut scenario, 3);
    let ornithopter = scenario.add_real_card(P0, ORNITHOPTER, Zone::Hand, db);
    let bushwhacker = scenario.add_real_card(P0, BUSHWHACKER, Zone::Hand, db);
    let mut runner = scenario.build();

    runner.cast(ornithopter).resolve();
    let action = cast_spell_action(&runner, bushwhacker);
    runner.act(action).expect("cast must be accepted");

    match &runner.state().waiting_for {
        WaitingFor::AlternativeCastChoice {
            keyword,
            normal_cost,
            alternative_cost,
            alternative_additional_cost,
            ..
        } => {
            // CR 702.117a + CR 118.9: the caster may choose the printed cost or
            // the surge cost.
            assert_eq!(*keyword, AlternativeCastKeyword::Surge);
            assert_eq!(*normal_cost, mana_cost(2, vec![ManaCostShard::Red]));
            assert_eq!(
                *alternative_cost,
                Some(mana_cost(1, vec![ManaCostShard::Red]))
            );
            assert_eq!(*alternative_additional_cost, None);
        }
        other => panic!("expected a Surge AlternativeCastChoice, got {other:?}"),
    }
}

/// Answering `Normal` pays the printed cost and the surge rider does not apply.
#[test]
fn surge_choice_normal_pays_printed_cost() {
    let db = shared_card_db().expect("card fixture must be present for Surge tests");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountains = add_mountains(&mut scenario, 3);
    let ornithopter = scenario.add_real_card(P0, ORNITHOPTER, Zone::Hand, db);
    let bushwhacker = scenario.add_real_card(P0, BUSHWHACKER, Zone::Hand, db);
    let mut runner = scenario.build();

    runner.cast(ornithopter).resolve();
    let action = cast_spell_action(&runner, bushwhacker);
    runner.act(action).expect("cast must be accepted");
    // CR 702.117a: both costs payable → the caster is asked.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Surge,
                ..
            }
        ),
        "CR 702.117a: expected the Surge prompt, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Normal,
        })
        .expect("Normal answer must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        recorded_variant(&runner, bushwhacker),
        CastingVariant::Normal
    );
    assert_eq!(runner.state().objects[&bushwhacker].zone, Zone::Battlefield);
    assert_eq!(tapped_count(&runner, &mountains), 3);
    // CR 603.4: the surge cost was not paid, so the rider does nothing.
    assert_eq!(power_of(&runner, ornithopter), 0);
    assert!(!has_haste(&runner, ornithopter));
}

/// Answering `Alternative` pays the surge cost and the rider applies.
#[test]
fn surge_choice_alternative_pays_surge_cost() {
    let db = shared_card_db().expect("card fixture must be present for Surge tests");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountains = add_mountains(&mut scenario, 3);
    let ornithopter = scenario.add_real_card(P0, ORNITHOPTER, Zone::Hand, db);
    let bushwhacker = scenario.add_real_card(P0, BUSHWHACKER, Zone::Hand, db);
    let mut runner = scenario.build();

    runner.cast(ornithopter).resolve();
    runner
        .cast(bushwhacker)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();

    assert_eq!(
        recorded_variant(&runner, bushwhacker),
        CastingVariant::Surge,
        "CR 702.117a: the Alternative answer casts for the surge cost"
    );
    assert_eq!(tapped_count(&runner, &mountains), 2);
    // CR 603.4: the surge cost was paid, so the rider applies.
    assert_eq!(power_of(&runner, ornithopter), 1);
    assert!(has_haste(&runner, ornithopter));
}

/// With no other spell cast this turn only the printed cost exists (no
/// prompt); that cast then counts as "another spell" for the next surge card.
#[test]
fn first_surge_card_is_hard_cast_and_enables_the_next() {
    let db = shared_card_db().expect("card fixture must be present for Surge tests");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountains = add_mountains(&mut scenario, 5);
    let first = scenario.add_real_card(P0, BUSHWHACKER, Zone::Hand, db);
    let second = scenario.add_real_card(P0, BUSHWHACKER, Zone::Hand, db);
    let mut runner = scenario.build();

    // CR 702.117a: no other spell yet → Normal, no prompt (the driver would
    // panic on an unanswered prompt).
    runner.cast(first).resolve();
    assert_eq!(recorded_variant(&runner, first), CastingVariant::Normal);
    assert_eq!(tapped_count(&runner, &mountains), 3);
    assert_eq!(power_of(&runner, first), 2);

    // CR 702.117a: the first Bushwhacker is "another spell"; with two Mountains
    // left only the surge cost is payable, so Surge is elected.
    runner.cast(second).resolve();
    assert_eq!(recorded_variant(&runner, second), CastingVariant::Surge);
    assert_eq!(tapped_count(&runner, &mountains), 5);
    assert_eq!(
        power_of(&runner, first),
        3,
        "CR 603.4: the second Bushwhacker's surge rider pumps the first"
    );
}

/// CR 702.117a "you or a teammate": a spell cast this turn by a Two-Headed
/// Giant teammate enables Surge; a spell cast by an opponent does not.
#[test]
fn surge_counts_teammate_spells_not_opponent_spells() {
    const OPPONENT: PlayerId = PlayerId(2);

    let run = |caster_of_other_spell: PlayerId| {
        let db = shared_card_db().expect("card fixture must be present for Surge tests");
        // 2HG seating: P0+P1 are one team, P2+P3 the other.
        let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let mountains = add_mountains(&mut scenario, 2);
        let bushwhacker = scenario.add_real_card(P0, BUSHWHACKER, Zone::Hand, db);
        let mut runner = scenario.build();
        // Labelled fixture: seed the per-turn cast ledger with another player's
        // spell this turn.
        runner
            .state_mut()
            .spells_cast_this_turn_by_player
            .entry(caster_of_other_spell)
            .or_default()
            .push_back(SpellCastRecord {
                name: "Lightning Bolt".into(),
                ..Default::default()
            });
        (runner, mountains, bushwhacker)
    };

    // Board 1: an opponent's spell does not enable Surge, and the printed cost
    // is unpayable.
    let (mut runner, mountains, bushwhacker) = run(OPPONENT);
    let action = cast_spell_action(&runner, bushwhacker);
    assert!(
        runner.act(action).is_err(),
        "CR 702.117a: an opponent's spell must not enable surge"
    );
    assert_eq!(runner.state().objects[&bushwhacker].zone, Zone::Hand);
    assert_eq!(tapped_count(&runner, &mountains), 0);

    // Board 2: the same spell cast by the teammate enables Surge.
    let (mut runner, mountains, bushwhacker) = run(P1);
    runner.cast(bushwhacker).resolve();
    assert_eq!(
        recorded_variant(&runner, bushwhacker),
        CastingVariant::Surge,
        "CR 702.117a: a teammate's spell enables surge"
    );
    assert_eq!(tapped_count(&runner, &mountains), 2);
}
