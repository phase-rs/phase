//! CR 701.59a + CR 608.2c + CR 117.2e: collect evidence as an effect is a
//! choice made while its ability resolves. The rest of the ability waits for
//! that choice and finishes inside the same public `SelectCards` action,
//! before any player receives priority, reading the same resolution-local
//! results it would have read without the pause.

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Lamplight Phoenix's dies trigger.
const LAMPLIGHT_TRIGGER: &str = "When this creature dies, you may exile it and collect evidence 4. If you do, return this card to the battlefield tapped. (To collect evidence 4, exile cards with total mana value 4 or greater from your graveyard.)";

fn moved_to(events: &[GameEvent], object: ObjectId, zone: Zone) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(event, GameEvent::ZoneChanged { object_id, to, .. }
                if *object_id == object && *to == zone)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Lamplight Phoenix: accepting exiles the Phoenix and asks for evidence. The
/// prompt can't be backed out of (CR 608.2d: the option was already chosen),
/// and choosing the evidence exiles it, then returns the Phoenix from exile
/// exactly once, tapped, all within that one action. The return names the
/// Phoenix through the result its own exile published earlier in the same
/// resolution (CR 400.7j), so a resume that reset resolution-local state would
/// lose it.
#[test]
fn lamplight_phoenix_evidence_choice_finishes_the_return_within_the_action() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let phoenix = scenario
        .add_creature_from_oracle(P0, "Lamplight Phoenix", 2, 2, LAMPLIGHT_TRIGGER)
        .id();
    let evidence = scenario
        .add_creature_to_graveyard(P0, "Evidence", 4, 4)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let mut runner = scenario.build();

    let mut events = Vec::new();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(phoenix, Zone::Graveyard, phoenix),
        &mut events,
    ));
    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the dies trigger offers its optional exile; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("exile the Phoenix and collect evidence");
    let prompt = runner.state().waiting_for.clone();
    assert!(
        matches!(prompt, WaitingFor::CollectEvidenceChoice { .. }),
        "collect evidence 4 asks for graveyard cards; got {prompt:?}"
    );
    assert_eq!(
        runner.state().objects[&phoenix].zone,
        Zone::Exile,
        "the Phoenix's own exile happened before the evidence prompt"
    );

    assert!(
        runner.act(GameAction::CancelCast).is_err(),
        "a resolving ability's evidence prompt can't be cancelled"
    );
    assert_eq!(
        runner.state().waiting_for,
        prompt,
        "the rejected cancel leaves the prompt in place"
    );

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![evidence],
        })
        .expect("exile mana value 4 as evidence");

    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "the trigger finished resolving in this action; got {:?}",
        result.waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing of the trigger is left to resolve"
    );
    let exiled = moved_to(&result.events, evidence, Zone::Exile);
    let returned = moved_to(&result.events, phoenix, Zone::Battlefield);
    assert_eq!(exiled.len(), 1, "the evidence is exiled once");
    assert_eq!(returned.len(), 1, "the Phoenix returns exactly once");
    assert!(
        exiled[0] < returned[0],
        "the return waits for the evidence exile"
    );
    let phoenix_now = &runner.state().objects[&phoenix];
    assert_eq!(phoenix_now.zone, Zone::Battlefield);
    assert!(phoenix_now.tapped, "the Phoenix returns tapped");
    assert_eq!(runner.state().objects[&evidence].zone, Zone::Exile);
}
