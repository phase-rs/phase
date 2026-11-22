//! Bronze Horse — runtime witness for the conditional source-scoped prevention
//! clause "As long as you control another creature, prevent all damage that
//! would be dealt to this creature by spells that target it."
//!
//! CR 611.3a: a static ability with an "as long as" condition is on only while
//!   the condition is true.
//! CR 615.1a + CR 615.2 + CR 609.7c: "prevent" creates a prevention effect that
//!   applies to damage from a source with the stated properties.
//! CR 115.1 + CR 601.2c: a spell targets the objects its controller announces.
//! CR 120.3a: damage dealt to a player causes that player to lose that much life.
//!
//! Verbatim printed Oracle text (MTGJSON AtomicCards):
//! "Trample
//! As long as you control another creature, prevent all damage that would be
//! dealt to this creature by spells that target it."

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::TargetRef;

use crate::support::shared_card_db as load_db;

fn db() -> &'static CardDatabase {
    load_db().expect("the committed integration card fixture is present")
}

fn mana(kind: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(kind, ObjectId(0), false, Vec::new()))
        .collect()
}

/// Pass priority once, from the active player to P1 (two-player game).
fn pass_to_p1(runner: &mut GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "P0 must hold priority before passing, got {:?}",
        runner.state().waiting_for
    );
    runner.act(GameAction::PassPriority).expect("pass priority");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 must hold priority after the pass, got {:?}",
        runner.state().waiting_for
    );
}

/// Completion boundary: an empty stack in a clean priority window for `player`.
fn assert_settled(runner: &GameRunner, player: PlayerId) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player),
        "the game must settle with priority for {player:?}, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing may remain on the stack"
    );
}

/// P0 casts a printed, untargeted spell from hand and lets it resolve.
fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the spell is castable");
    runner.advance_until_stack_empty();
    assert_settled(runner, P0);
}

/// The ability index of the printed "{T}: This creature deals 1 damage to any
/// target." (Prodigal Sorcerer).
fn damage_ability_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| matches!(ability.effect.as_ref(), Effect::DealDamage { .. }))
        .expect("the printed damage ability")
}

/// CR 611.3a + CR 115.1 + CR 615.1a + CR 615.2: with another P0 creature on the
/// battlefield the condition holds, and P1's Lightning Bolt — a spell that
/// targets Bronze Horse — deals no damage.
#[test]
fn bronze_horse_prevents_damage_from_a_spell_that_targets_it() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let horse = scenario.add_real_card(P0, "Bronze Horse", Zone::Battlefield, db);
    scenario.add_vanilla(P0, 1, 1);
    let bolt = scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P1, mana(ManaType::Red, 1));

    let mut runner = scenario.build();
    pass_to_p1(&mut runner);

    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Lightning Bolt");
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Lightning Bolt must open a target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "P1 is the spell's controller and its announcer");
    assert!(
        target_slots
            .iter()
            .flat_map(|slot| slot.legal_targets.iter())
            .any(|target| *target == TargetRef::Object(horse)),
        "CR 115.1: Bronze Horse must be in the Bolt's legal target set"
    );
    assert!(
        selection
            .current_legal_targets
            .contains(&TargetRef::Object(horse)),
        "the announced target must be the horse"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(horse)),
        })
        .expect("targeting Bronze Horse is legal");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&horse].zone,
        Zone::Battlefield,
        "reach: the horse survives"
    );
    assert_eq!(
        runner.state().objects[&horse].damage_marked,
        0,
        "the Bolt is a spell that targets the horse, so its 3 damage must be prevented"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "reach guard: the Bolt was aimed at the horse, not the player"
    );
    assert_settled(&runner, P0);
}

/// CR 611.3a: with no other creature, the "as long as" condition is false, so
/// the same Bolt is dealt in full (3 of the horse's 4 toughness).
#[test]
fn bronze_horse_without_another_creature_takes_spell_damage() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let horse = scenario.add_real_card(P0, "Bronze Horse", Zone::Battlefield, db);
    let bolt = scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(P1, mana(ManaType::Red, 1));

    let mut runner = scenario.build();
    pass_to_p1(&mut runner);

    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Lightning Bolt");
    // Lightning Bolt has several legal targets, so the engine always asks: assert the
    // chooser and that the intended target is offered before answering.
    let WaitingFor::TargetSelection {
        player, selection, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Lightning Bolt must ask for its target, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player,
        runner.state().objects[&bolt].owner,
        "the caster chooses the target"
    );
    assert!(
        selection
            .current_legal_targets
            .contains(&TargetRef::Object(horse)),
        "the intended target is offered"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(horse)),
        })
        .expect("targeting Bronze Horse is legal");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&horse].zone,
        Zone::Battlefield,
        "a 4/4 survives 3 damage"
    );
    assert_eq!(
        runner.state().objects[&horse].damage_marked,
        3,
        "with no other creature the shield is off, so the Bolt's damage is dealt"
    );
    assert_settled(&runner, P0);
}

/// CR 615.1a + CR 115.1: the clause covers only spells that TARGET the horse.
/// Pyroclasm ("Pyroclasm deals 2 damage to each creature.") targets nothing, so
/// its 2 damage is dealt even though the condition holds.
#[test]
fn bronze_horse_takes_damage_from_an_untargeted_spell() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let horse = scenario.add_real_card(P0, "Bronze Horse", Zone::Battlefield, db);
    let bystander = scenario.add_vanilla(P0, 1, 1);
    let pyroclasm = scenario.add_real_card(P0, "Pyroclasm", Zone::Hand, db);
    let mut pool = mana(ManaType::Red, 1);
    pool.extend(mana(ManaType::Colorless, 1));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    cast_and_resolve(&mut runner, pyroclasm);

    assert_eq!(
        runner.state().objects[&bystander].zone,
        Zone::Graveyard,
        "reach guard: Pyroclasm resolved and killed the 1/1"
    );
    assert_eq!(
        runner.state().objects[&horse].zone,
        Zone::Battlefield,
        "the 4/4 survives Pyroclasm's 2 damage"
    );
    assert_eq!(
        runner.state().objects[&horse].damage_marked,
        2,
        "Pyroclasm does not target the horse, so the shield does not apply"
    );
}

/// CR 615.1a + CR 615.2: the shield covers damage from SPELLS. A Prodigal
/// Sorcerer's activated ability ("{T}: This creature deals 1 damage to any
/// target.") is not a spell, so its 1 damage is marked on the horse.
#[test]
fn bronze_horse_takes_damage_from_an_activated_ability() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let horse = scenario.add_real_card(P0, "Bronze Horse", Zone::Battlefield, db);
    scenario.add_vanilla(P0, 1, 1);
    let pinger = scenario.add_real_card(P1, "Prodigal Sorcerer", Zone::Battlefield, db);

    let mut runner = scenario.build();
    pass_to_p1(&mut runner);

    let index = damage_ability_index(&runner, pinger);
    runner
        .activate(pinger, index)
        .target_object(horse)
        .resolve();

    assert_eq!(
        runner.state().objects[&horse].damage_marked,
        1,
        "an activated ability is not a spell, so its 1 damage is dealt"
    );
    assert_eq!(
        runner.state().objects[&horse].zone,
        Zone::Battlefield,
        "the 4/4 survives 1 damage"
    );
    assert_settled(&runner, P0);
}
