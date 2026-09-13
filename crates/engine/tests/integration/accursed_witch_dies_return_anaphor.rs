//! Accursed Witch — dies-trigger "it" must return the dying creature, not the
//! chosen attach-host opponent.
//!
//! Oracle (front face, verified Scryfall):
//!   "Spells your opponents cast that target this creature cost {1} less to cast.
//!    When this creature dies, return it to the battlefield transformed under
//!    your control attached to target opponent."
//!
//! Before the lift-gate fix, a player-only `valid_target` blocked
//! `ParentTarget` → `TriggeringSource`, so "it" inherited the chosen opponent.
//!
//! DISCRIMINATING: after the dies event, the stacked trigger's ChangeZone
//! target is `SelfRef` (the dying Witch). Reverting the player-scope
//! carve-out in `valid_target_blocks_event_source_lift` leaves it
//! `ParentTarget` (the attach-host opponent).
//!
//! CR 608.2k: an effect that refers to a specific untargeted object previously
//! named by the trigger condition still finds that object after it changes
//! zones.
//! CR 603.6: zone-change triggers look for the object in the zone it moved to.
//! CR 400.7e: a leaves-the-battlefield trigger can find the new object in the
//! public zone it moved to.

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::ability::TargetFilter;
use engine::types::game_state::StackEntryKind;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ACCURSED_WITCH: &str = "Spells your opponents cast that target this creature cost {1} less to cast.\n\
When this creature dies, return it to the battlefield transformed under your control attached to target opponent.";

#[test]
fn accursed_witch_dies_return_it_binds_self() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let witch = scenario
        .add_creature_from_oracle(P0, "Accursed Witch", 4, 2, ACCURSED_WITCH)
        .id();
    let mut runner = scenario.build();

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), witch, Zone::Graveyard, &mut events);
    process_triggers(runner.state_mut(), &events);

    assert_eq!(
        runner.state().objects[&witch].zone,
        Zone::Graveyard,
        "precondition: the Witch is in the graveyard"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "dies trigger must be on the stack, waiting_for = {:?}",
        runner.state().waiting_for
    );
    let stacked = match &runner.state().stack[0].kind {
        StackEntryKind::TriggeredAbility { ability, .. } => ability.effect.target_filter().cloned(),
        other => panic!("expected TriggeredAbility on the stack, got {other:?}"),
    };
    assert!(
        matches!(stacked, Some(TargetFilter::SelfRef)),
        "CR 608.2k: \"it\" must be the dying Witch (SelfRef), not the \
         attach-host opponent (ParentTarget); got {stacked:?}"
    );
}
