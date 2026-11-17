//! CR 701.59a + CR 608.2c + CR 117.2e: collect evidence as an effect is a
//! choice made while its ability resolves. The rest of the ability waits for
//! that choice and finishes inside the same public `SelectCards` action,
//! before any player receives priority, reading the same resolution-local
//! results it would have read without the pause. CR 118.3 + CR 608.2d: an
//! optional "exile it and collect evidence N" is offered only when the
//! evidence can be paid in full without the card the same option exiles.

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
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

/// Lamplight Phoenix's printed mana cost, {1}{R}{R} (mana value 3).
fn lamplight_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        generic: 1,
    }
}

/// Kills the Phoenix and puts its dies trigger on the stack.
fn phoenix_dies(scenario: GameScenario, phoenix: ObjectId) -> engine::game::scenario::GameRunner {
    let mut runner = scenario.build();
    let mut events = Vec::new();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(phoenix, Zone::Graveyard, phoenix),
        &mut events,
    ));
    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach: the dies trigger fired and is on the stack"
    );
    runner
}

/// CR 118.3 + CR 608.2d + CR 701.59b: "exile it and collect evidence 4" is one
/// option. With only the Phoenix (mana value 3) and a mana-value-1 card in the
/// graveyard, the evidence can't be paid without the Phoenix the same option
/// exiles (ruling: the Phoenix can't be its own evidence), so the option is
/// impossible: it is never offered, the Phoenix stays in the graveyard and the
/// "If you do" return does not happen.
#[test]
fn lamplight_phoenix_cannot_count_itself_toward_its_own_evidence() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let phoenix = scenario
        .add_creature_from_oracle(P0, "Lamplight Phoenix", 2, 2, LAMPLIGHT_TRIGGER)
        .with_mana_cost(lamplight_cost())
        .id();
    let small = scenario
        .add_creature_to_graveyard(P0, "Evidence", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = phoenix_dies(scenario, phoenix);
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
        "the impossible exile-and-collect option is not offered; got {:?}",
        state.waiting_for
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::CollectEvidenceChoice { .. }),
        "no evidence prompt opens; got {:?}",
        state.waiting_for
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the trigger resolved as declined; got {:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty(), "nothing of the trigger is left");
    assert_eq!(
        state.objects[&phoenix].zone,
        Zone::Graveyard,
        "the Phoenix is neither exiled nor returned"
    );
    assert_eq!(state.objects[&small].zone, Zone::Graveyard);
}

/// CR 701.59b + CR 400.7: the exclusion is the exact card the option exiles,
/// not its name or mana value. A second card named Lamplight Phoenix (mana
/// value 3) plus a mana-value-1 card pay evidence 4 without the dying Phoenix:
/// the option is offered, the prompt omits the dying Phoenix, and choosing both
/// returns it tapped.
#[test]
fn lamplight_phoenix_offers_evidence_payable_without_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let phoenix = scenario
        .add_creature_from_oracle(P0, "Lamplight Phoenix", 2, 2, LAMPLIGHT_TRIGGER)
        .with_mana_cost(lamplight_cost())
        .id();
    let small = scenario
        .add_creature_to_graveyard(P0, "Evidence", 1, 1)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let other_phoenix = scenario
        .add_creature_to_graveyard(P0, "Lamplight Phoenix", 2, 2)
        .with_mana_cost(lamplight_cost())
        .id();
    let mut runner = phoenix_dies(scenario, phoenix);
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "evidence 4 is payable without the dying Phoenix; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("exile the Phoenix and collect evidence");
    let WaitingFor::CollectEvidenceChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "collect evidence 4 asks for graveyard cards; got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(cards.contains(&small) && cards.contains(&other_phoenix));
    assert!(
        !cards.contains(&phoenix),
        "the exiled Phoenix is not evidence"
    );

    let result = runner
        .act(GameAction::SelectCards {
            cards: vec![small, other_phoenix],
        })
        .expect("exile total mana value 4 as evidence");
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "the trigger finished resolving; got {:?}",
        result.waiting_for
    );
    assert!(runner.state().stack.is_empty());
    let phoenix_now = &runner.state().objects[&phoenix];
    assert_eq!(phoenix_now.zone, Zone::Battlefield);
    assert!(phoenix_now.tapped, "the Phoenix returns tapped");
    assert_eq!(runner.state().objects[&small].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&other_phoenix].zone, Zone::Exile);
}
