//! CR733 P2 coverage for the two battlefield-entry retags in the zone delivery
//! tail: the CR 110.2a controller override and the CR 603.6a provenance stamp.
//!
//! Both run in the same block after the object has already been delivered, and
//! both wrote persistent object state raw. They are separate families rather than
//! one parameterized "entry retag" because CR 110.2a (control) and CR 603.6a
//! (which ability placed the permanent) are different rule sections the engine
//! resolves independently.
//!
//! `zones::apply_battlefield_entry_controller_override` is the single authority
//! for CR 110.2a "enters under your control" entries (reanimation, Tergrid-class
//! theft, `reveal_until` entries, and the elimination handoff), but it wrote FIVE
//! fields raw: the object's `base_controller` and `controller`, plus the
//! `controller` of the `zone_changes_this_turn` snapshot, the
//! `battlefield_entries_this_turn` snapshot, and the in-flight `ZoneChanged`
//! event.
//!
//! Only the first four are persistent state. The event fix-up is deliberately
//! outside the command: events are transient carriers consumed by the same
//! resolution, not state a replay reconstructs.
//!
//! The two snapshot POSITIONS are recorded rather than re-found, mirroring
//! `ResolvedZoneChangeCommand::turn_zone_change_index` — CR 400.7 permits the
//! same object to hold several entries in one turn, so a replay-time last-match
//! scan could retag a different one.
//!
//! The test drives the REAL pipeline: casting a reanimation spell at an
//! OPPONENT-owned creature card, so owner and controller genuinely diverge and
//! an applier that skipped the override could not accidentally pass.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::resolved_commands::ResolvedRulesCommand;
use engine::types::zones::Zone;

const REANIMATE_ORACLE: &str =
    "Put target creature card from a graveyard onto the battlefield under your control.";

#[test]
fn entering_under_your_control_journals_an_exact_controller_override() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Owned by the OPPONENT: the override must move control to P0 while P1 stays
    // the owner (CR 110.2), so every assertion below distinguishes the two.
    let corpse = scenario
        .add_creature_to_graveyard(P1, "Stolen Bear", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE_ORACLE)
        .id();

    let mut runner = scenario.build();
    // Baseline captured AFTER the spell is on the stack: the cast itself appends a
    // hand -> stack record to `zone_changes_this_turn`, and the resolution's
    // zone-change command records its index relative to that. Replaying from a
    // pre-cast state would leave the turn record one short.
    let committed = runner.cast(spell).target_object(corpse).commit();
    let pre_state = committed.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();

    let outcome = committed.resolve();
    let state = outcome.state();

    // CR 110.2a reach guards: the card actually entered, and it entered under the
    // caster's control rather than its owner's. Without these the journal
    // assertion could pass vacuously on a reanimation that never resolved.
    let object = &state.objects[&corpse];
    assert_eq!(
        object.zone,
        Zone::Battlefield,
        "the reanimation must put the card onto the battlefield"
    );
    assert_eq!(
        object.controller, P0,
        "CR 110.2a: the card enters under the caster's control"
    );
    assert_eq!(
        object.owner, P1,
        "CR 110.2: the override changes control, never ownership"
    );

    // The discriminating assertion: the override is journaled as an exact
    // resolved command. Five raw field writes record nothing here.
    let overrides: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::ControllerOverride(command)
                if command.object.object_id == corpse =>
            {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        overrides.len(),
        1,
        "the controller-override authority must journal exactly one resolved command"
    );

    let command = &overrides[0];
    assert_eq!(
        command.expected_old_controller, P1,
        "the recorded precondition is the pre-override controller (the owner)"
    );
    assert_eq!(
        command.resulting_controller, P0,
        "the recorded result is the caster"
    );

    // The snapshot retags are part of the same command: a replay that installed
    // only the object's controller would leave "entered under whose control"
    // look-back queries (CR 400.7 / CR 403.3) answering with the owner.
    let zone_change_index = command
        .zone_change_index
        .expect("a battlefield entry records its zone-change snapshot position");
    assert_eq!(
        state.zone_changes_this_turn[zone_change_index].controller, P0,
        "CR 400.7: the zone-change snapshot is retagged to the new controller"
    );
    let entry_index = command
        .battlefield_entry_index
        .expect("a battlefield entry records its entry-snapshot position");
    assert_eq!(
        state.battlefield_entries_this_turn[entry_index].controller, P0,
        "CR 403.3: the battlefield-entry snapshot is retagged to the new controller"
    );

    // Replay-exactness: every command up to and including the override, applied
    // to the pre-cast state, must reproduce the same controller and the same
    // retagged snapshots with no re-derivation of which records to touch.
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
            ResolvedRulesCommand::ControllerOverride(command) => {
                engine::game::zones::apply_resolved_controller_override(&mut replay, command)
                    .unwrap();
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        replay.objects[&corpse].controller, P0,
        "replay installs the exact recorded controller"
    );
    assert_eq!(
        replay.objects[&corpse].base_controller,
        Some(P0),
        "CR 110.2a: replay pins the base controller the override established"
    );
    assert_eq!(
        replay.zone_changes_this_turn[zone_change_index].controller, P0,
        "replay retags the exact recorded zone-change snapshot"
    );
    assert_eq!(
        replay.battlefield_entries_this_turn[entry_index].controller, P0,
        "replay retags the exact recorded battlefield-entry snapshot"
    );
}

/// CR 603.6a: the same ability-driven entry stamps the entering permanent with
/// the ability that placed it, so anti-recursion intervening-ifs ("if it wasn't
/// put onto the battlefield with this ability") can exclude it. A replay that
/// dropped the stamp would let such abilities re-trigger off their own output.
#[test]
fn ability_driven_entry_journals_an_exact_provenance_stamp() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let corpse = scenario
        .add_creature_to_graveyard(P1, "Stolen Bear", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE_ORACLE)
        .id();

    let mut runner = scenario.build();
    // Baseline captured AFTER the spell is on the stack: the cast itself appends a
    // hand -> stack record to `zone_changes_this_turn`, and the resolution's
    // zone-change command records its index relative to that. Replaying from a
    // pre-cast state would leave the turn record one short.
    let committed = runner.cast(spell).target_object(corpse).commit();
    let pre_state = committed.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();

    let outcome = committed.resolve();
    let state = outcome.state();

    // CR 603.6a reach guard: the permanent entered AND carries the placing
    // ability's source, so the journal assertion below cannot pass vacuously.
    let object = &state.objects[&corpse];
    assert_eq!(object.zone, Zone::Battlefield);
    let stamped_source = object
        .entered_via_ability_source
        .expect("CR 603.6a: an ability-driven entry records the placing ability's source");

    let stamps: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::EntryProvenance(command)
                if command.object.object_id == corpse =>
            {
                Some(command)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        stamps.len(),
        1,
        "the provenance authority must journal exactly one resolved stamp"
    );

    let stamp = &stamps[0];
    assert_eq!(
        stamp.expected_old_source, None,
        "CR 603.6a: the entry pipeline cleared the field, so the stamp starts from unstamped"
    );
    assert_eq!(
        stamp.resulting_source, stamped_source,
        "the journaled source is the source the stamp installed"
    );

    // Replay-exactness: the recorded stamp reinstalls the same source with no
    // re-derivation of which ability was responsible.
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
            ResolvedRulesCommand::EntryProvenance(command) => {
                engine::game::zones::apply_resolved_entry_provenance(&mut replay, command).unwrap();
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        replay.objects[&corpse].entered_via_ability_source,
        Some(stamped_source),
        "replay installs the exact recorded placing ability"
    );
}
