//! CR733 P2 coverage for the object-deletion family.
//!
//! `zones::cease_object` is the only production path that deletes an object
//! outright, and the CR 704.5d SBA sweep is its single caller. It wrote
//! `state.objects.remove` raw, so a retained-prefix replay left the token alive
//! in a zone the rules had already swept it from.
//!
//! Ceasing to exist is deliberately NOT a zone change (CR 400.7): no event is
//! emitted and no "whenever exiled" trigger fires, so this cannot ride the
//! zone-change family — it needs its own command.
//!
//! The test drives the REAL pipeline: a destroy spell resolving at a token, whose
//! move to the graveyard makes the state-based sweep delete it.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, TargetFilter, TypedFilter};
use engine::types::phase::Phase;
use engine::types::resolved_commands::ResolvedRulesCommand;
use engine::types::zones::Zone;

#[test]
fn token_cease_to_exist_journals_an_exact_resolved_removal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let token = scenario.add_creature(P0, "Spirit Token", 1, 1).id();
    let mut spell = scenario.add_spell_to_hand(P0, "Smite", true);
    spell.with_ability(Effect::Destroy {
        target: TargetFilter::Typed(TypedFilter::creature()),
        cant_regenerate: false,
    });
    let spell_id = spell.id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&token)
        .expect("the token exists")
        .is_token = true;

    // Baseline captured AFTER the spell is on the stack: the cast appends its own
    // hand -> stack turn record, and the resolution's zone-change command records
    // its index relative to that.
    let committed = runner.cast(spell_id).target_object(token).commit();
    let pre_state = committed.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();

    let outcome = committed.resolve();
    let state = outcome.state();

    // CR 704.5d reach guard: the token is gone from `state.objects` entirely, not
    // merely moved. Without this the journal assertion could pass vacuously.
    assert!(
        !state.objects.contains_key(&token),
        "CR 704.5d: a token in a zone other than the battlefield ceases to exist"
    );

    // The discriminating assertion: the removal is journaled as an exact resolved
    // command. A raw `objects.remove` records nothing here.
    let ceases: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::ObjectCease(command) if command.object.object_id == token => {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ceases.len(),
        1,
        "the cease-to-exist authority must journal exactly one resolved removal"
    );

    let cease = &ceases[0];
    assert_eq!(
        cease.expected_zone,
        Zone::Graveyard,
        "CR 704.5d: the token ceased from the zone the destroy put it in"
    );
    assert_eq!(cease.owner, P0, "the recorded owner is the token's owner");

    // Replay-exactness: applying the recorded commands to the pre-cast state must
    // delete the same object from the same zone with no re-run of the CR 704.5d
    // eligibility scan.
    let mut replay = pre_state;
    replay.resolved_rules_journal = state.resolved_rules_journal.clone();
    for entry in state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
    {
        let Some(replayed) = entry.command.clone() else {
            continue;
        };
        match &replayed {
            ResolvedRulesCommand::ZoneChange(command) => {
                engine::game::zones::apply_resolved_zone_change(&mut replay, command).unwrap();
            }
            ResolvedRulesCommand::ObjectCease(command) => {
                engine::game::zones::apply_resolved_object_cease(&mut replay, command).unwrap();
                break;
            }
            _ => {}
        }
    }
    assert!(
        !replay.objects.contains_key(&token),
        "replay deletes the exact recorded object"
    );
    assert!(
        !replay.players[usize::from(P0.0)].graveyard.contains(&token),
        "replay also removes the token from the zone list, not just the object map"
    );
}
