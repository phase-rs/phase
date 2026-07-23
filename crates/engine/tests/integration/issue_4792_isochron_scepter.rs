//! Issue #4792: Isochron Scepter must allow casting the copied imprinted instant.

use engine::game::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLink, ExileLinkKind, StackEntryKind, WaitingFor};
use engine::types::identifiers::TrackedSetId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn fund_generic(runner: &mut GameRunner, amount: u32) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
    }
}

fn link_imprinted_instant(
    runner: &mut GameRunner,
    scepter: engine::types::identifiers::ObjectId,
    imprint: engine::types::identifiers::ObjectId,
) {
    let state = runner.state_mut();
    assert_eq!(
        state.objects.get(&imprint).map(|o| o.zone),
        Some(Zone::Exile),
        "imprint candidate must start in exile"
    );
    state.exile_links.push(ExileLink {
        source_id: scepter,
        exiled_id: imprint,
        kind: ExileLinkKind::TrackedBySource,
    });
    state
        .tracked_object_sets
        .insert(TrackedSetId(0), vec![imprint]);
}

fn drive_isochron_activation(runner: &mut GameRunner, target: engine::types::ability::TargetRef) {
    for step in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional copy/cast");
            }
            WaitingFor::CopyRetarget { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target.clone()),
                    })
                    .expect("choose shock target");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other if runner.state().stack.is_empty() => {
                panic!("unexpected waiting state at step {step}: {other:?}");
            }
            _ => {}
        }
    }
    panic!(
        "Isochron activation did not finish: stack={:?} waiting={:?}",
        runner.state().stack,
        runner.state().waiting_for
    );
}

#[test]
fn isochron_scepter_copies_and_casts_imprinted_instant() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scepter = scenario.add_real_card(P0, "Isochron Scepter", Zone::Battlefield, db);
    let shock = scenario.add_real_card(P0, "Shock", Zone::Exile, db);

    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    link_imprinted_instant(&mut runner, scepter, shock);
    fund_generic(&mut runner, 2);

    let life_before = runner.state().players[1].life;

    runner
        .act(GameAction::ActivateAbility {
            source_id: scepter,
            ability_index: 0,
        })
        .expect("Isochron activation must be legal with imprint and mana");

    drive_isochron_activation(&mut runner, engine::types::ability::TargetRef::Player(P1));
    runner.advance_until_stack_empty();

    assert!(
        runner.state().players[1].life < life_before,
        "Shock copy must deal damage to the chosen creature's controller"
    );
    assert_eq!(
        runner.state().objects.get(&shock).map(|o| o.zone),
        Some(Zone::Exile),
        "imprinted Shock stays exiled after copying"
    );
}

/// CR 608.2g + CR 113.2c + CR 702.60b: Isochron's copied spell is cast through
/// the production `CopySpell` → `CastFromZone { ParentTarget }` reducer path.
/// Its cast-time snapshot must include Thrumming Stone's Ripple grant so the
/// synthesized trigger reaches the stack after the copy's retarget choice.
#[test]
fn isochron_cast_copy_snapshots_thrumming_stone_ripple() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Thrumming Stone",
        1,
        1,
        "Spells you cast have ripple 4. (When you cast a spell, you may reveal the top four cards of your library. You may cast any revealed cards with the same name as the cast spell without paying their mana costs. Put the rest on the bottom of your library in any order.)",
    );
    let scepter = scenario.add_real_card(P0, "Isochron Scepter", Zone::Battlefield, db);
    let shock = scenario.add_real_card(P0, "Shock", Zone::Exile, db);
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    link_imprinted_instant(&mut runner, scepter, shock);
    fund_generic(&mut runner, 2);

    runner
        .act(GameAction::ActivateAbility {
            source_id: scepter,
            ability_index: 0,
        })
        .expect("activate Isochron Scepter");
    let copy_id = (0..20)
        .find_map(|_| match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Isochron copy/cast choice");
                None
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                None
            }
            WaitingFor::CopyRetarget { copy_id, .. } => Some(copy_id),
            other => panic!("unexpected Isochron cast state: {other:?}"),
        })
        .expect("copied Shock must request a target");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Player(P1)),
        })
        .expect("target copied Shock");

    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, ability, .. }
                if *source_id == copy_id && matches!(ability.effect, Effect::Ripple { count: 4 })
        )),
        "the cast copy must create Thrumming Stone's Ripple trigger"
    );
}

/// Resolve Isochron Scepter's REAL "Imprint" ETB trigger (pulled from the
/// object's own `trigger_definitions`, parsed from real Oracle text via the
/// card database) against `imprint_candidate`, populating `state.exile_links`
/// through the actual `change_zone::resolve` runtime path — never by hand-
/// seeding `state.exile_links` directly.
fn resolve_real_imprint(
    runner: &mut GameRunner,
    scepter: engine::types::identifiers::ObjectId,
    imprint_candidate: engine::types::identifiers::ObjectId,
) {
    let trigger = &runner.state().objects[&scepter].trigger_definitions[0];
    let execute = trigger
        .definition
        .execute
        .as_ref()
        .expect("Isochron Scepter's Imprint trigger must carry an execute ability");
    let mut resolved = engine::game::ability_utils::build_resolved_from_def(execute, scepter, P0);
    resolved.targets = vec![engine::types::ability::TargetRef::Object(imprint_candidate)];
    let mut events = Vec::new();
    engine::game::effects::change_zone::resolve(runner.state_mut(), &resolved, &mut events)
        .expect("Imprint exile must resolve");
}

/// Discriminating regression test for issue #4792 / #3246 / #3674 (parser
/// half): Isochron Scepter's activated ability must copy the card LINKED to
/// it via CR 406.6/607.2a exile links (`TargetFilter::ExiledBySource`), not
/// whatever object happens to occupy the ephemeral `TrackedSet(0)` slot at
/// activation time.
///
/// Root cause (pre-fix): "copy the exiled card" parsed to
/// `CopySpell { target: TrackedSet { id: 0 } }` unconditionally — even though
/// the exile happened in an EARLIER, separately-resolved ability (the ETB
/// Imprint trigger), not in the SAME resolution chain as the activated
/// ability. `TrackedSet(0)` is a chain-local sentinel that any LATER,
/// unrelated exile-producing resolution (an impulse draw, a Dig, etc.) can
/// clobber before the activated ability ever gets used, causing Isochron to
/// copy whatever that unrelated resolution most recently published instead
/// of the imprinted card.
///
/// This test drives the REAL ETB Imprint trigger (via
/// `resolve_real_imprint`, which resolves the actual parsed trigger through
/// `change_zone::resolve` — no hand-seeded `exile_links`), then simulates an
/// UNRELATED resolution publishing a decoy card (Lightning Bolt) to
/// `TrackedSet(0)`, then activates Isochron for real. Pre-fix, the decoy in
/// `TrackedSet(0)` wins and Lightning Bolt (3 damage) is copied and cast.
/// Post-fix, `ExiledBySource` correctly resolves via `exile_links` to the
/// imprinted Shock (2 damage) regardless of what unrelated activity clobbered
/// `TrackedSet(0)`.
#[test]
fn isochron_scepter_copies_correct_card_across_unrelated_tracked_set_activity() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scepter = scenario.add_real_card(P0, "Isochron Scepter", Zone::Battlefield, db);
    let shock = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
    // The decoy sits in exile (a prerequisite for `copy_source_from_tracked_set`'s
    // `obj.zone == Zone::Exile` guard) as if some unrelated earlier resolution
    // in the same game had exiled and tracked it — completely unconnected to
    // Isochron Scepter's own Imprint exile.
    let bolt_decoy = scenario.add_real_card(P0, "Lightning Bolt", Zone::Exile, db);

    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);

    // Real ETB Imprint — exiles Shock from hand via the actual trigger.
    resolve_real_imprint(&mut runner, scepter, shock);
    assert_eq!(
        runner.state().objects.get(&shock).map(|o| o.zone),
        Some(Zone::Exile),
        "reach-guard: Imprint must actually exile Shock before the discriminator is meaningful"
    );
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.source_id == scepter && link.exiled_id == shock),
        "reach-guard: the real Imprint resolution must link Shock to Isochron Scepter \
         (CR 406.6 + CR 607.2a) — the discriminator depends on ExiledBySource finding it here"
    );

    // Unrelated activity clobbers the ephemeral chain-local TrackedSet(0)
    // sentinel with a decoy — simulating some other resolution in the same
    // game (an impulse draw, a Dig, etc.) that has nothing to do with
    // Isochron Scepter.
    runner
        .state_mut()
        .tracked_object_sets
        .insert(TrackedSetId(0), vec![bolt_decoy]);

    fund_generic(&mut runner, 2);
    let life_before = runner.state().players[1].life;

    runner
        .act(GameAction::ActivateAbility {
            source_id: scepter,
            ability_index: 0,
        })
        .expect("Isochron activation must be legal with the real imprint and mana");

    drive_isochron_activation(&mut runner, engine::types::ability::TargetRef::Player(P1));
    runner.advance_until_stack_empty();

    let life_after = runner.state().players[1].life;
    let damage_dealt = life_before - life_after;

    // DISCRIMINATOR: exactly Shock's 2 damage, never Lightning Bolt's 3.
    // Pre-fix (CopySpell { TrackedSet(0) }) this reads 3 (the clobbering
    // decoy); post-fix (CopySpell { ExiledBySource }) this reads 2 (the
    // correctly-linked imprint) regardless of TrackedSet(0)'s contents.
    assert_eq!(
        damage_dealt, 2,
        "Isochron must copy the imprinted Shock (2 damage) via ExiledBySource, \
         not whatever unrelated activity clobbered TrackedSet(0) (Lightning Bolt would deal 3)"
    );
    assert_eq!(
        runner.state().objects.get(&shock).map(|o| o.zone),
        Some(Zone::Exile),
        "imprinted Shock stays exiled after copying"
    );
    assert_eq!(
        runner.state().objects.get(&bolt_decoy).map(|o| o.zone),
        Some(Zone::Exile),
        "the unrelated decoy must be untouched by Isochron's activation"
    );
}
