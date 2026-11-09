//! A public concession that cancels one member of a resolving activated
//! family still owes the ordinary SBA/trigger boundary before priority.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ReplacementChoiceKind, WaitingFor, ZoneDeparture};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

// Verbatim AtomicCards Oracle text, including the loyalty minus signs.
const LILIANA_VESS: &str = "[+1]: Target player discards a card.\n[−2]: Search your library for a card, then shuffle and put that card on top.\n[−8]: Put all creature cards from all graveyards onto the battlefield under your control.";
const CLONE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";
const WALKING_BALLISTA: &str = "This creature enters with X +1/+1 counters on it.\n{4}: Put a +1/+1 counter on this creature.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.";
const SOUL_WARDEN: &str = "Whenever another creature enters, you gain 1 life.";
const THRAGTUSK: &str = "When this creature enters, you gain 5 life.\nWhen this creature leaves the battlefield, create a 3/3 green Beast creature token.";

fn move_count(events: &[GameEvent], object: ObjectId, from: Zone, to: Zone) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged { object_id, from: Some(origin), to: destination, .. }
                    if *object_id == object && *origin == from && *destination == to
            )
        })
        .count()
}

fn liliana_clone_ballista_scenario() -> (GameScenario, [ObjectId; 5]) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Nine loyalty is reachable from her printed five via earlier +1 uses;
    // paying -8 leaves her alive, so no source-loyalty SBA can mask the bug.
    let liliana = scenario
        .add_planeswalker_from_oracle(P0, "Liliana Vess", "Liliana", 9, LILIANA_VESS)
        .id();
    let warden = scenario
        .add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    // A legal historical Bribery-style entry under P0's control. This is
    // P1's card, but its leave trigger belongs to surviving controller P0.
    // Both it and Soul Warden are legal battlefield models for Clone.
    let thragtusk = scenario
        .add_creature_from_oracle(P1, "Thragtusk", 5, 3, THRAGTUSK)
        .with_subtypes(vec!["Beast"])
        .controlled_by(P0)
        .id();
    // APNAP graveyard order reaches P1's Clone before P2's Ballista.
    let clone = scenario
        .add_creature_to_graveyard(P1, "Clone", 0, 0)
        .with_subtypes(vec!["Shapeshifter"])
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue],
        })
        .from_oracle_text(CLONE)
        .id();
    let ballista = scenario
        .add_creature_to_graveyard(P2, "Walking Ballista", 0, 0)
        .as_artifact_creature()
        .with_subtypes(vec!["Construct"])
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::X, ManaCostShard::X],
        })
        .from_oracle_text(WALKING_BALLISTA)
        .id();
    (scenario, [liliana, warden, thragtusk, clone, ballista])
}

/// CR 107.3g: Ballista's X is zero off the stack. CR 704.5f + CR 704.3:
/// the resulting 0/0 dies before the resumed ability's triggers are ordered,
/// not on the next action. CR 603.6c: a stolen Thragtusk leaving with its
/// owner supplies an independently owned departure-prefix trigger, which
/// must not be collected again by the resumed segment's convergence pass.
#[test]
fn liliana_vess_concession_resumes_family_through_sba_before_triggers() {
    let (scenario, [liliana, warden, thragtusk, clone, ballista]) =
        liliana_clone_ballista_scenario();

    let mut runner = scenario.build();
    let outcome = runner.activate(liliana, 2).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice {
                player: P0,
                kind: ReplacementChoiceKind::OptionalBranch,
                ..
            }
        ),
        "reach: Clone's actual optional copy replacement is unanswered: {:?}",
        outcome.final_waiting_for()
    );
    let state = runner.state();
    assert_eq!(state.objects[&liliana].loyalty, Some(1));
    assert_eq!(state.objects[&clone].zone, Zone::Graveyard);
    assert_eq!(state.objects[&ballista].zone, Zone::Graveyard);
    assert_eq!(state.objects[&thragtusk].owner, P1);
    assert_eq!(state.objects[&thragtusk].controller, P0);
    assert_eq!(
        move_count(
            outcome.events(),
            ballista,
            Zone::Graveyard,
            Zone::Battlefield
        ),
        0
    );

    let result = runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes publicly at the actual optional Clone replacement");

    // No priority drive, trigger answer, or other action may precede this
    // assertion: it discriminates convergence from merely resuming the family.
    let state = runner.state();
    assert_eq!(
        state.objects[&ballista].zone,
        Zone::Graveyard,
        "the surviving loyalty family must finish its SBA boundary before Concede returns"
    );
    assert!(state.players[P1.0 as usize].is_eliminated);
    assert!(!state.players[P0.0 as usize].is_eliminated);
    assert!(!state.players[P2.0 as usize].is_eliminated);
    assert!(state.players[P2.0 as usize].graveyard.contains(&ballista));
    assert!(!state.battlefield.contains(&ballista));
    assert_eq!(state.objects[&clone].zone, Zone::Exile);
    assert_eq!(state.objects[&liliana].zone, Zone::Battlefield);
    assert!(state.pending_replacement.is_none());
    assert!(state.resolution_stack.is_empty());
    assert!(state.resolving_stack_entry.is_none());
    assert_eq!(result.waiting_for, state.waiting_for);

    // The caller receives the departure prefix and resumed suffix once each.
    assert_eq!(
        move_count(&result.events, clone, Zone::Graveyard, Zone::Exile),
        1
    );
    assert_eq!(
        move_count(&result.events, clone, Zone::Graveyard, Zone::Battlefield),
        0
    );
    assert_eq!(
        move_count(&result.events, thragtusk, Zone::Battlefield, Zone::Exile),
        1
    );
    assert_eq!(
        move_count(&result.events, ballista, Zone::Graveyard, Zone::Battlefield),
        1
    );
    assert_eq!(
        move_count(&result.events, ballista, Zone::Battlefield, Zone::Graveyard),
        1
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| { matches!(event, GameEvent::PlayerEliminated { player_id: P1 }) })
            .count(),
        1
    );
    let death = result
        .events
        .iter()
        .find_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Graveyard,
                record,
                ..
            } if *object_id == ballista => Some(record),
            _ => None,
        })
        .expect("the resumed Ballista actually entered and then died");
    assert_eq!(
        death.controller, P0,
        "Liliana returned it under her controller's control"
    );
    assert_eq!(death.owner, P2, "the SBA uses its independent owner");
    assert_eq!(death.departure, ZoneDeparture::Moved);

    // The real departure observer and the real resumed-entry observer share
    // one orderable batch. Rescanning the prefix duplicates Thragtusk; losing
    // the resumed segment loses Soul Warden. Neither trigger has resolved.
    let WaitingFor::OrderTriggers { player, triggers } = &result.waiting_for else {
        panic!(
            "P0 orders the departure and entry triggers after SBAs: {:?}",
            result.waiting_for
        );
    };
    assert_eq!(*player, P0);
    let mut sources: Vec<_> = triggers.iter().map(|trigger| trigger.source_id).collect();
    sources.sort();
    let mut expected = vec![warden, thragtusk];
    expected.sort();
    assert_eq!(
        sources, expected,
        "each event segment is collected exactly once"
    );
    assert!(
        state.stack.is_empty(),
        "the complete batch still awaits ordering"
    );
    assert_eq!(runner.life(P0), 20);
}

/// CR 800.4b: a surviving owner's card cannot enter under the departed
/// resolving controller. CR 117.2e + CR 608.2m: retiring that exact unanswered
/// delivery must finish the begun family before anybody receives priority.
#[test]
fn liliana_vess_departed_clone_chooser_completes_family_before_priority() {
    let (scenario, [liliana, warden, thragtusk, clone, ballista]) =
        liliana_clone_ballista_scenario();
    let mut runner = scenario.build();
    let outcome = runner.activate(liliana, 2).resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice {
                player: P0,
                kind: ReplacementChoiceKind::OptionalBranch,
                ..
            }
        ),
        "reach: the resolving controller owes Clone's actual optional replacement"
    );
    assert_eq!(runner.state().objects[&clone].owner, P1);
    assert_eq!(runner.state().objects[&ballista].owner, P2);
    assert_eq!(runner.state().objects[&clone].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&ballista].zone, Zone::Graveyard);

    let result = runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("the latched Clone chooser concedes through the public action");

    // Inspect the public action's immediate result, with no follow-up answer,
    // priority action, manual SBA pass, or private continuation drain.
    let state = runner.state();
    assert!(state.players[P0.0 as usize].is_eliminated);
    assert!(!state.players[P1.0 as usize].is_eliminated);
    assert!(!state.players[P2.0 as usize].is_eliminated);
    for (object, owner) in [(clone, P1), (ballista, P2)] {
        assert_eq!(state.objects[&object].zone, Zone::Graveyard);
        assert_eq!(state.objects[&object].owner, owner);
        assert_eq!(state.objects[&object].controller, owner);
        assert!(state.players[owner.0 as usize].graveyard.contains(&object));
        assert!(!state.battlefield.contains(&object));
        assert_eq!(
            move_count(&result.events, object, Zone::Graveyard, Zone::Battlefield),
            0,
            "CR 800.4b does not transfer the intended destination controller"
        );
        assert_eq!(
            move_count(&result.events, object, Zone::Graveyard, Zone::Exile),
            0,
            "the surviving owners' cards do not leave with the chooser"
        );
    }
    assert!(state.pending_replacement.is_none());
    assert!(state.resolution_stack.is_empty());
    assert!(state.resolving_stack_entry.is_none());
    assert!(state.stack.is_empty());
    assert_eq!(result.waiting_for, state.waiting_for);
    assert!(matches!(
        result.waiting_for,
        WaitingFor::Priority { player: P1 | P2 }
    ));
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved { source_id, .. } if *source_id == liliana
            ))
            .count(),
        1,
        "the begun family completes exactly once even though its controller left"
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::PlayerEliminated { player_id: P0 }))
            .count(),
        1
    );
    assert_eq!(
        move_count(&result.events, liliana, Zone::Battlefield, Zone::Exile),
        1
    );
    assert_eq!(
        move_count(&result.events, warden, Zone::Battlefield, Zone::Exile),
        1
    );
    assert_eq!(
        move_count(&result.events, thragtusk, Zone::Battlefield, Zone::Exile),
        1
    );
    assert_eq!(state.objects[&thragtusk].owner, P1);
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P2), 20);
}
