//! Issue #3251 — Force of Negation exiles countered spells directly, not via graveyard.
//!
//! https://github.com/phase-rs/phase/issues/3251

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FORCE_OF_NEGATION_ORACLE: &str = "If it's not your turn, you may exile a blue card from your hand rather than pay this spell's mana cost.\n\
Counter target noncreature spell. If that spell is countered this way, exile it instead of putting it into its owner's graveyard.";

fn put_noncreature_spell_on_stack(
    runner: &mut engine::game::scenario::GameRunner,
    controller: engine::types::player::PlayerId,
) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(701),
        controller,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(701),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

#[test]
fn force_of_negation_exiles_countered_spell_without_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let fon = scenario
        .add_spell_to_hand_from_oracle(P0, "Force of Negation", true, FORCE_OF_NEGATION_ORACLE)
        .id();
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);

    let mut runner = scenario.build();
    let opponent_spell = put_noncreature_spell_on_stack(&mut runner, P1);

    runner.cast(fon).target_objects(&[opponent_spell]).resolve();

    assert!(
        runner.state().stack.is_empty(),
        "Force of Negation must counter the target spell"
    );
    assert_eq!(
        runner.state().objects.get(&opponent_spell).map(|o| o.zone),
        Some(Zone::Exile),
        "countered spell must exile directly instead of passing through graveyard"
    );
    assert!(
        !runner.state().players[1]
            .graveyard
            .contains(&opponent_spell),
        "countered spell must not enter the graveyard"
    );
}
