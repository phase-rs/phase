//! Pulse of the Forge's conditional self-return must use the controller of its
//! chosen player-or-planeswalker target after dealing damage.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, Comparator, Effect, PlayerScope, QuantityExpr, QuantityRef,
};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const PULSE_OF_THE_FORGE: &str = "Pulse of the Forge deals 4 damage to target player or planeswalker. Then if that player or that planeswalker's controller has more life than you, return Pulse of the Forge to its owner's hand.";

/// Verifies the parser preserves Pulse's damage and conditional self-return.
#[test]
fn pulse_of_the_forge_parses_damage_and_conditional_return_chain() {
    let parsed = parse_oracle_text(
        PULSE_OF_THE_FORGE,
        "Pulse of the Forge",
        &[],
        &["Instant".into()],
        &[],
    );
    assert!(
        parsed.parse_warnings.is_empty(),
        "Pulse of the Forge must not emit parse warnings: {:?}",
        parsed.parse_warnings
    );

    let ability = parsed
        .abilities
        .first()
        .expect("Pulse of the Forge must parse a spell ability");
    assert!(
        matches!(&*ability.effect, Effect::DealDamage { .. }),
        "the first instruction must deal damage: {:?}",
        ability.effect
    );
    let return_to_hand = ability
        .sub_ability
        .as_ref()
        .expect("the conditional return must remain chained after damage");
    assert!(
        matches!(
            &*return_to_hand.effect,
            Effect::Bounce {
                target: engine::types::ability::TargetFilter::SelfRef,
                destination: None,
                ..
            }
        ),
        "the rider must return the spell to hand: {:?}",
        return_to_hand.effect
    );
    assert!(
        matches!(
            &return_to_hand.condition,
            Some(AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::ParentObjectTargetController,
                    },
                },
                comparator: Comparator::GT,
                rhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeTotal {
                        player: PlayerScope::Controller,
                    },
                },
            })
        ),
        "the rider must compare the target/controller life total to the caster's: {:?}",
        return_to_hand.condition
    );
}

/// Adds a castable Pulse of the Forge to the active player's hand.
fn pulse_spell(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Pulse of the Forge", true, PULSE_OF_THE_FORGE)
        .id()
}

/// Adds a planeswalker owned by P2 for a subsequent P1 control-change fixture.
fn add_stolen_planeswalker(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    // P2 owns this object; the runtime setup changes its controller to P1.
    // Both differ from P0, the spell's controller.
    scenario
        .add_creature(P2, "Borrowed Jace", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 6)
        .id()
}

/// Establishes P1 as the planeswalker's durable controller in the fixture.
fn set_controller(runner: &mut GameRunner, object: engine::types::identifiers::ObjectId) {
    let planeswalker = runner
        .state_mut()
        .objects
        .get_mut(&object)
        .expect("planeswalker must exist");
    assert_eq!(planeswalker.owner, P2, "fixture must be owned by P2");
    // The scenario starts from a stable post-control-change board state. Layer
    // evaluation restores `controller` from `base_controller`, so both fields
    // must name P1 rather than merely mutating the derived controller.
    planeswalker.base_controller = Some(P1);
    planeswalker.controller = P1;
    assert_eq!(
        planeswalker.controller, P1,
        "fixture must be controlled by P1, not its owner or the caster"
    );
}

/// Asserts that Pulse dealt its full four damage to the planeswalker.
fn assert_planeswalker_damage_reached(
    outcome: &engine::game::scenario::CastOutcome,
    planeswalker: engine::types::identifiers::ObjectId,
) {
    assert_eq!(
        outcome.counters(planeswalker, CounterType::Loyalty),
        2,
        "Pulse of the Forge must deal 4 damage to the targeted planeswalker"
    );
}

/// Verifies Pulse returns to hand when the damaged player remains ahead.
#[test]
fn pulse_of_the_forge_returns_after_damaging_a_player_with_more_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P1, 25);
    let spell = pulse_spell(&mut scenario);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();

    outcome.assert_life_delta(P1, -4);
    outcome.assert_zone(&[spell], Zone::Hand);
}

/// Verifies Pulse stays in the graveyard when damage removes the player's lead.
#[test]
fn pulse_of_the_forge_stays_in_graveyard_after_player_damage_removes_life_lead() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P1, 24);
    let spell = pulse_spell(&mut scenario);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();

    outcome.assert_life_delta(P1, -4);
    outcome.assert_zone(&[spell], Zone::Graveyard);
}

/// Verifies a targeted planeswalker's controller supplies Pulse's true gate.
#[test]
fn pulse_of_the_forge_uses_targeted_planeswalkers_controller_for_true_gate() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario
        .at_phase(Phase::PreCombatMain)
        .with_life(P0, 20)
        .with_life(P1, 25)
        .with_life(P2, 10);
    let planeswalker = add_stolen_planeswalker(&mut scenario);
    let spell = pulse_spell(&mut scenario);
    let mut runner = scenario.build();
    set_controller(&mut runner, planeswalker);

    let outcome = runner.cast(spell).target_object(planeswalker).resolve();

    assert_planeswalker_damage_reached(&outcome, planeswalker);
    outcome.assert_zone(&[spell], Zone::Hand);
}

/// Verifies a targeted planeswalker's controller supplies Pulse's false gate.
#[test]
fn pulse_of_the_forge_uses_targeted_planeswalkers_controller_for_false_gate() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario
        .at_phase(Phase::PreCombatMain)
        .with_life(P0, 20)
        .with_life(P1, 10)
        .with_life(P2, 25);
    let planeswalker = add_stolen_planeswalker(&mut scenario);
    let spell = pulse_spell(&mut scenario);
    let mut runner = scenario.build();
    set_controller(&mut runner, planeswalker);

    let outcome = runner.cast(spell).target_object(planeswalker).resolve();

    assert_planeswalker_damage_reached(&outcome, planeswalker);
    outcome.assert_zone(&[spell], Zone::Graveyard);
}
