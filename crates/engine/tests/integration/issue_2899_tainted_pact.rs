//! Regression for issue #2899 — Tainted Pact repeat-until loop and same-name
//! unless gate on the optional put-to-hand rider.
//!
//! https://github.com/phase-rs/phase/issues/2899

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::move_to_library_position;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TAINTED_PACT_ORACLE: &str = "Exile the top card of your library. You may put that card into your hand unless it has the same name as another card exiled this way. Repeat this process until you put a card into your hand or you exile two cards with the same name, whichever comes first.";

fn put_library_top(runner: &mut GameRunner, id: ObjectId) {
    let owner = runner.state().objects.get(&id).expect("object").owner;
    let mut events = Vec::new();
    move_to_library_position(runner.state_mut(), id, true, &mut events);
    assert_eq!(
        runner.state().players[owner.0 as usize].library[0],
        id,
        "precondition: card must be library top"
    );
}

fn resolve_tainted_pact(runner: &mut GameRunner, source: ObjectId, optional_accepts: &[bool]) {
    let def = parse_effect_chain(TAINTED_PACT_ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    for &accept in optional_accepts {
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "expected optional put prompt, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept })
            .expect("optional put decision");
    }
}

#[test]
fn tainted_pact_repeats_until_controller_puts_a_card_into_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let third = scenario
        .add_spell_to_library_top(P0, "Third Card", true)
        .id();
    let second = scenario
        .add_spell_to_library_top(P0, "Second Card", true)
        .id();
    let first = scenario
        .add_spell_to_library_top(P0, "First Card", true)
        .id();

    let mut runner = scenario.build();
    put_library_top(&mut runner, first);

    resolve_tainted_pact(&mut runner, ObjectId(900), &[false, false, true]);

    assert_eq!(
        runner.state().objects.get(&third).unwrap().zone,
        Zone::Hand,
        "accepting the third optional put must move the top card into hand"
    );
    assert_eq!(
        runner.state().objects.get(&first).unwrap().zone,
        Zone::Exile,
        "declined iteration must leave the card exiled"
    );
    assert_eq!(
        runner.state().objects.get(&second).unwrap().zone,
        Zone::Exile,
        "declined iteration must leave the card exiled"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "loop must finish after a card is put into hand, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn tainted_pact_stops_when_two_exiled_cards_share_a_name() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bolt_a = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let island = scenario.add_spell_to_library_top(P0, "Island", true).id();
    let bolt_b = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();

    let mut runner = scenario.build();
    // Exile order: bolt_b, island, bolt_a.
    put_library_top(&mut runner, bolt_b);

    resolve_tainted_pact(&mut runner, ObjectId(900), &[false, false]);

    assert_eq!(
        runner.state().objects.get(&bolt_b).unwrap().zone,
        Zone::Exile
    );
    assert_eq!(
        runner.state().objects.get(&island).unwrap().zone,
        Zone::Exile
    );
    assert_eq!(
        runner.state().objects.get(&bolt_a).unwrap().zone,
        Zone::Exile
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "unless gate must block the third optional put when names duplicate, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "duplicate-name stop must end the loop, got {:?}",
        runner.state().waiting_for
    );
}
