//! Sulfuric Vortex — "If a player would gain life, that player gains no life
//! instead." fully suppresses life gain (issue #743).
//!
//! CR 119.10 + CR 614.6: this is a lifegain-negation *replacement* effect. The
//! parser previously lowered the body ("that player gains no life") to
//! an `Unimplemented` no-op effect, which the replacement applier treats
//! as a silent passthrough — so the life gain proceeded unmodified and the
//! clause did nothing. The fix lowers the body to a structured
//! `QuantityModification::Prevent`, which `gain_life_applier` reads to return
//! `ApplyResult::Prevented` (CR 614.6: a replaced event never happens).
//!
//! Discriminating end-to-end: with Sulfuric Vortex on the battlefield, casting a
//! real "you gain 3 life" sorcery must leave the caster's life UNCHANGED.
//! Pre-fix the life total goes up by 3 (the Unimplemented passthrough); post-fix
//! it is suppressed entirely. A second creature with no Vortex in play confirms
//! the same spell would otherwise gain life (the replacement, not the spell, is
//! what suppresses it).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::Phase;

const SULFURIC_VORTEX: &str = "At the beginning of each player's upkeep, this enchantment deals 2 damage to that player.\nIf a player would gain life, that player gains no life instead.";

fn card_id_of(runner: &GameRunner, id: ObjectId) -> CardId {
    runner.state().objects.get(&id).unwrap().card_id
}

/// Cast a spell from hand and drive the pipeline to stack-empty.
fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = card_id_of(runner, spell);
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("cast gain-life spell");
    for _ in 0..40 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

#[test]
fn sulfuric_vortex_prevents_lifegain() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Sulfuric Vortex on P0's battlefield (added as a body-bearing permanent;
    // the card type is irrelevant to the lifegain replacement under test). Its
    // second ability registers the GainLife replacement.
    scenario.add_creature_from_oracle(P0, "Sulfuric Vortex", 0, 1, SULFURIC_VORTEX);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain", false, "You gain 3 life.")
        .id();

    let mut runner = scenario.build();
    let before = runner.life(P0);

    cast_and_resolve(&mut runner, spell);

    assert_eq!(
        runner.life(P0),
        before,
        "CR 119.10 + CR 614.6: Sulfuric Vortex must fully prevent the life gain (Prevent replacement fired)"
    );
}

#[test]
fn without_vortex_the_same_spell_gains_life() {
    // Control: the identical spell DOES gain life when no Vortex is in play, so
    // the suppression above is attributable to the replacement, not the spell.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain", false, "You gain 3 life.")
        .id();

    let mut runner = scenario.build();
    let before = runner.life(P0);

    cast_and_resolve(&mut runner, spell);

    assert_eq!(
        runner.life(P0),
        before + 3,
        "without the replacement, the gain-life spell must raise life by 3"
    );
}
