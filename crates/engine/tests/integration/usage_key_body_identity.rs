//! Usage-key body identity (CR 602.2a + CR 602.5b + CR 602.5c + CR 113.2c).
//!
//! The per-instance usage key carries a fingerprint of the announced
//! definition's `(cost, effect, ability_tag)` triple. Three-part keys alone
//! (`source`, provenance, rules text) collide on undescribed
//! same-acquisition abilities that differ only in body — two equip costs on
//! one object, or reconfigure's attach vs unattach modes (all `description:
//! None`) — so one shared once-per-turn budget wrongly covers both.
//!
//! No printed card exhibits two differently-costed equip abilities on one
//! object, so the equip pair is synthetic: the host's abilities are replaced
//! with two engine-shaped granted equips (built exactly as
//! `synthesize_equip` builds them, one provider stamp on both) under an
//! injected `OnlyOnceEachTurn` each. The reconfigure pair is real cards
//! (Inchblade Companion, whose attach/unattach modes are load-synthesized
//! with the same `None` provenance and no descriptions). Every activation
//! drives the production `GameRunner::activate` path; negatives pair with
//! positives.
//!
//! Revert detectors: dropping the fingerprint collapses each pair back onto
//! one key, so the second activation is refused and the independence
//! assertions fail.

use std::sync::Arc;

use engine::game::casting::{activated_ability_definitions, can_activate_ability_now};
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::restrictions::{ability_fingerprint, ability_usage_key};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AbilityTag, ActivationRestriction,
    AttachCardinality, AttachSelection, BasicLandType, ControllerRef, Effect, TargetFilter,
    TextSubstitution, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// Build an engine-shaped granted equip ability: the exact body
/// `synthesize_equip` produces for one `Keyword::Equip(cost)`, plus the
/// shared provider stamp and a once-per-turn cap (the instrument that makes
/// a shared budget observable).
fn granted_equip(cost_generic: u32, stamp: ObjectIncarnationRef) -> AbilityDefinition {
    let mut def = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Attach {
            attachment: TargetFilter::SelfRef,
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            selection: AttachSelection::AtResolution {
                count: AttachCardinality::One,
            },
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![],
            generic: cost_generic,
        },
    })
    .sorcery_speed();
    def.ability_tag = Some(AbilityTag::Equip);
    def.granting_object = Some(stamp);
    def.activation_restrictions
        .push(ActivationRestriction::OnlyOnceEachTurn);
    def
}

fn fund(runner_pool: &mut GameScenario, player: PlayerId, units: usize) {
    runner_pool.with_mana_pool(
        player,
        (0..units)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
}

/// The generic-mana equip cost of the ability at `index`.
fn equip_cost(runner: &GameRunner, host: ObjectId, index: usize) -> u32 {
    match runner.state().objects[&host].abilities[index].cost.as_ref() {
        Some(AbilityCost::Mana {
            cost: ManaCost::Cost { generic, .. },
        }) => *generic,
        other => panic!("expected a generic-mana equip cost, got {other:?}"),
    }
}

fn assert_settled(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "stack must be empty, got {:?}",
        runner.state().stack
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected Priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 602.5c + CR 113.2c: two granted equips (`{1}` vs `{3}`) from one
/// provider on one object are distinct instances — each activates once under
/// its own once-per-turn budget.
#[test]
fn granted_equip_pair_activates_independently() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The host is a real Equipment (so attach legality holds) whose abilities
    // the test replaces with the synthetic pair.
    let host = scenario.add_real_card(P0, "Short Sword", Zone::Battlefield, db);
    let provider = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let bear_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let bear_b = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    fund(&mut scenario, P0, 10);
    let mut runner = scenario.build();

    // Synthetic pair (no printed card exhibits it): one provider donates two
    // differently-costed equips to the host, replacing its abilities.
    let stamp = ObjectIncarnationRef::from_object(&runner.state().objects[&provider]);
    {
        // CR 613.1: every layer pass reseeds live `abilities` from
        // `base_abilities`, so the synthetic pair must live on both (the
        // repo's own synthetic setups do the same).
        let obj = runner.state_mut().objects.get_mut(&host).expect("host");
        *Arc::make_mut(&mut obj.base_abilities) =
            vec![granted_equip(1, stamp), granted_equip(3, stamp)];
        obj.abilities = Arc::clone(&obj.base_abilities);
    }
    let cheaper = (0..2)
        .find(|index| equip_cost(&runner, host, *index) == 1)
        .expect("the {1} equip");
    let pricier = (0..2)
        .find(|index| equip_cost(&runner, host, *index) == 3)
        .expect("the {3} equip");

    // The fingerprint is what tells them apart: same source, same provider,
    // no descriptions — cost is the only input that differs.
    let cheap_def = &runner.state().objects[&host].abilities[cheaper];
    let pricey_def = &runner.state().objects[&host].abilities[pricier];
    assert_ne!(
        ability_fingerprint(cheap_def),
        ability_fingerprint(pricey_def),
        "the equip fingerprints must differ in cost"
    );

    // Reach-guard: both equips are offered before either is used.
    assert!(
        can_activate_ability_now(runner.state(), P0, host, cheaper),
        "the {{1}} equip must be offered"
    );
    assert!(
        can_activate_ability_now(runner.state(), P0, host, pricier),
        "the {{3}} equip must be offered"
    );

    // First instance through the driver.
    runner
        .activate(host, cheaper)
        .target_object(bear_a)
        .resolve();
    assert_settled(&runner);
    assert!(
        runner.state().objects[&bear_a].attachments.contains(&host),
        "bear A must wear the host after the {{1}} equip"
    );
    let cheap_key = ability_usage_key(runner.state(), host, cheaper).expect("cheaper resolves");
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&cheap_key)
            .copied(),
        Some(1),
        "the {{1}} equip's own budget is spent"
    );

    // Second instance: the granted copy's own budget is still unused (revert
    // detector: a shared three-part key refuses this activation).
    let pricier_now = (0..runner.state().objects[&host].abilities.len())
        .find(|index| equip_cost(&runner, host, *index) == 3)
        .expect("the {3} equip survives the first activation");
    runner
        .activate(host, pricier_now)
        .target_object(bear_b)
        .resolve();
    assert_settled(&runner);
    assert!(
        runner.state().objects[&bear_b].attachments.contains(&host),
        "bear B must wear the host after the {{3}} equip (re-attached)"
    );
    let pricey_key =
        ability_usage_key(runner.state(), host, pricier_now).expect("pricier resolves");
    assert_ne!(
        cheap_key, pricey_key,
        "the two equips must key independent budgets"
    );
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&pricey_key)
            .copied(),
        Some(1),
        "the {{3}} equip's own budget is spent"
    );

    // Hostile: each cap still binds its own instance — a repeat is refused.
    assert!(
        !can_activate_ability_now(runner.state(), P0, host, cheaper),
        "the spent {{1}} equip must not be offered again"
    );
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: cheaper,
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "repeating the spent {{1}} equip must be rejected"
    );
}

/// Boundary (CR 602.5b): two IDENTICAL granted `Equip {1}` instances share
/// one budget — the fingerprint splits only distinguishable bodies.
#[test]
fn identical_granted_equip_pair_shares_one_budget() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The host is a real Equipment (so attach legality holds) whose abilities
    // the test replaces with the synthetic pair.
    let host = scenario.add_real_card(P0, "Short Sword", Zone::Battlefield, db);
    let provider = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let bear_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let bear_b = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    fund(&mut scenario, P0, 10);
    let mut runner = scenario.build();

    let stamp = ObjectIncarnationRef::from_object(&runner.state().objects[&provider]);
    {
        // CR 613.1: every layer pass reseeds live `abilities` from
        // `base_abilities`, so the synthetic pair must live on both.
        let obj = runner.state_mut().objects.get_mut(&host).expect("host");
        *Arc::make_mut(&mut obj.base_abilities) =
            vec![granted_equip(1, stamp), granted_equip(1, stamp)];
        obj.abilities = Arc::clone(&obj.base_abilities);
    }
    let first_def = &runner.state().objects[&host].abilities[0];
    let second_def = &runner.state().objects[&host].abilities[1];
    assert_eq!(
        ability_fingerprint(first_def),
        ability_fingerprint(second_def),
        "identical bodies must fingerprint identically"
    );

    // Positive reach-guard: the first copy resolves.
    runner.activate(host, 0).target_object(bear_a).resolve();
    assert_settled(&runner);
    assert!(
        runner.state().objects[&bear_a].attachments.contains(&host),
        "bear A must wear the host after the first copy"
    );

    // The twin reads the same spent budget and is refused.
    assert!(
        !can_activate_ability_now(runner.state(), P0, host, 1),
        "the identical twin must not be offered once the budget is spent"
    );
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: 1,
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "the identical twin's activation must be rejected"
    );
    let _ = bear_b;
}

/// CR 702.151a: Inchblade Companion's reconfigure attach vs unattach modes
/// differ in effect only (same cost, same `None` provenance, no
/// descriptions) — each activates once under an injected once-per-turn cap.
#[test]
fn reconfigure_modes_activate_independently() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_real_card(P0, "Inchblade Companion", Zone::Battlefield, db);
    let bear_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let bear_b = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    fund(&mut scenario, P0, 10);
    let mut runner = scenario.build();

    // Reach-guard: exactly the two reconfigure modes, told apart by effect.
    let defs = activated_ability_definitions(runner.state(), host);
    let attach = defs
        .iter()
        .find(|(_, def)| matches!(def.effect.as_ref(), Effect::Attach { .. }))
        .map(|(index, _)| *index)
        .expect("the reconfigure attach mode");
    let unattach = defs
        .iter()
        .find(|(_, def)| matches!(def.effect.as_ref(), Effect::UnattachAll { .. }))
        .map(|(index, _)| *index)
        .expect("the reconfigure unattach mode");
    assert_eq!(
        defs.iter()
            .filter(|(_, def)| matches!(
                def.effect.as_ref(),
                Effect::Attach { .. } | Effect::UnattachAll { .. }
            ))
            .count(),
        2,
        "exactly the attach and unattach modes, got {:?}",
        defs.iter().map(|(_, def)| &def.effect).collect::<Vec<_>>()
    );
    let attach_def = &runner.state().objects[&host].abilities[attach];
    let unattach_def = &runner.state().objects[&host].abilities[unattach];
    assert_ne!(
        ability_fingerprint(attach_def),
        ability_fingerprint(unattach_def),
        "the reconfigure fingerprints must differ in effect"
    );

    // Cap instrument: one shared-budget scenario per mode. Injected into
    // `base_abilities` (and reseeded to live) so the layer passes the attach
    // resolution triggers never rewrite the test's own budget (CR 613.1).
    for index in [attach, unattach] {
        let obj = runner.state_mut().objects.get_mut(&host).expect("host");
        Arc::make_mut(&mut obj.base_abilities)[index]
            .activation_restrictions
            .push(ActivationRestriction::OnlyOnceEachTurn);
        obj.abilities = Arc::clone(&obj.base_abilities);
    }

    // Attach mode through the driver.
    runner
        .activate(host, attach)
        .target_object(bear_a)
        .resolve();
    assert_settled(&runner);
    assert!(
        runner.state().objects[&bear_a].attachments.contains(&host),
        "bear A must wear the host after attach mode"
    );

    // Unattach mode: its own budget is still unused (revert detector: a
    // shared three-part key refuses this activation). The mode resolves to a
    // no-op downstream today — `resolve_unattach_all` only detaches from
    // announced targets and the ability builds no target slot — a pre-existing
    // attach-mechanics gap outside this fix's scope. What this fix owns is
    // the independent budget, asserted here through the recorded journal row.
    assert!(
        can_activate_ability_now(runner.state(), P0, host, unattach),
        "unattach mode must be offered while its own budget is unused"
    );
    runner.activate(host, unattach).resolve();
    assert_settled(&runner);
    let attach_key = ability_usage_key(runner.state(), host, attach).expect("attach resolves");
    let unattach_key =
        ability_usage_key(runner.state(), host, unattach).expect("unattach resolves");
    assert_ne!(
        attach_key, unattach_key,
        "the two modes must key independent budgets"
    );
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&unattach_key)
            .copied(),
        Some(1),
        "the unattach mode's own budget is spent"
    );

    // Hostile: re-attaching is refused — each cap binds its own mode.
    assert!(
        !can_activate_ability_now(runner.state(), P0, host, attach),
        "spent attach mode must not be offered again"
    );
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: host,
                ability_index: attach,
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "repeating spent attach mode must be rejected"
    );
    let _ = bear_b;
}

/// Unit-2 helpers (Cauldron harness): the Untap slots on `holder`, all with
/// the printed "Activate only once each turn" shape.
fn untap_indices(runner: &GameRunner, holder: ObjectId) -> Vec<usize> {
    activated_ability_definitions(runner.state(), holder)
        .into_iter()
        .filter(|(_, def)| {
            def.description
                .as_ref()
                .is_some_and(|text| text.contains("Untap"))
        })
        .map(|(index, _)| index)
        .collect()
}

fn grant_counter(runner: &mut GameRunner, id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("counter carrier")
        .counters
        .insert(CounterType::Plus1Plus1, 1);
}

fn tap_for_untap(runner: &mut GameRunner, id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("tappable")
        .tapped = true;
}

fn journal_rows_for(
    runner: &GameRunner,
) -> Vec<engine::types::game_state::AbilityActivationRecord> {
    runner
        .state()
        .abilities_activated_this_turn_by_player
        .values()
        .flat_map(|rows| rows.iter().cloned())
        .collect()
}

/// U2 positive (CR 602.2a + CR 602.5c): a granted scavenge activation exiles
/// its source as the cost, so the post-payment re-derivation returns `None`
/// (the old record path dropped it silently — P2 proved `None` with zero
/// journal rows). The announced key is captured before the cost is paid and
/// consumed at placement, so the journal row and the turn counter land under
/// it. Varolz grants the scavenge; the Bears pays its own exile.
#[test]
fn granted_scavenge_records_journal_after_self_exile() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _varolz = scenario.add_real_card(P0, "Varolz, the Scar-Striped", Zone::Battlefield, db);
    let victim = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let target = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    fund(&mut scenario, P0, 4);
    let mut runner = scenario.build();

    // Reach-guard: the graveyard Bears carries exactly the granted scavenge,
    // resolved while the card is still in the graveyard.
    let index = activated_ability_definitions(runner.state(), victim)
        .into_iter()
        .find(|(_, def)| matches!(def.effect.as_ref(), Effect::PutCounter { .. }))
        .map(|(index, _)| index)
        .expect("Varolz must grant scavenge to the graveyard Bears");
    let announced = ability_usage_key(runner.state(), victim, index)
        .expect("the granted scavenge resolves before its cost is paid");

    runner
        .activate(victim, index)
        .target_object(target)
        .resolve();
    assert_settled(&runner);

    // Downstream: the cost exiled the source and the counters landed.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "the scavenge cost must exile the Bears"
    );
    assert_eq!(
        runner.state().objects[&target]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(2),
        "the target must get +1/+1 counters equal to the exiled power"
    );

    // The hole, closed: post-payment the slot no longer resolves...
    assert!(
        ability_usage_key(runner.state(), victim, index).is_none(),
        "after the self-exile the live lookup returns None"
    );
    // ...but the journal row and the counter were recorded under the
    // announced key (revert detector: restoring the post-payment derivation
    // leaves no row and a zero count here).
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&announced)
            .copied(),
        Some(1),
        "the granted scavenge must be counted once under its announced key"
    );
    let rows: Vec<_> = journal_rows_for(&runner)
        .into_iter()
        .filter(|row| row.source == victim)
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "exactly one journal row for the granted activation, got {:?}",
        journal_rows_for(&runner)
            .iter()
            .map(|row| row.source)
            .collect::<Vec<_>>()
    );
}

/// U2 cap (CR 602.5b + CR 602.2a): the Cauldron-granted Quirion Untap carries
/// its printed "Activate only once each turn". The first granted activation
/// records through the announced transport; the second is refused because
/// the check side reads the announced-recorded budget.
#[test]
fn cauldron_granted_capped_untap_enforces_cap_through_announced_key() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cauldron = scenario.add_real_card(P0, "Agatha's Soul Cauldron", Zone::Battlefield, db);
    let ranger = scenario.add_real_card(P0, "Quirion Ranger", Zone::Battlefield, db);
    let exiled = scenario.add_real_card(P0, "Quirion Ranger", Zone::Exile, db);
    let bear_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let bear_b = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let forest1 = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let forest2 = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let mut runner = scenario.build();
    grant_counter(&mut runner, ranger);
    runner.state_mut().exile_links.push(ExileLink {
        source_id: cauldron,
        exiled_id: exiled,
        kind: ExileLinkKind::TrackedBySource,
    });
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    tap_for_untap(&mut runner, bear_a);
    tap_for_untap(&mut runner, bear_b);

    let exile_stamp = ObjectIncarnationRef::from_object(&runner.state().objects[&exiled]);
    let granted = untap_indices(&runner, ranger)
        .into_iter()
        .find(|index| {
            runner.state().objects[&ranger].abilities[*index].granting_object == Some(exile_stamp)
        })
        .expect("the Cauldron-granted Untap");

    // First granted activation through the driver.
    runner
        .activate(ranger, granted)
        .target_object(bear_b)
        .pay_with(&[forest2])
        .resolve();
    assert_settled(&runner);
    assert!(
        !runner.state().objects[&bear_b].tapped,
        "bear B must be untapped by the granted instance"
    );
    let granted_key =
        ability_usage_key(runner.state(), ranger, granted).expect("the grant still resolves");
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&granted_key)
            .copied(),
        Some(1),
        "the granted instance's own budget is spent"
    );

    // Hostile: the same granted instance is refused — the cap reads the
    // announced-recorded budget, not a re-derived one.
    let granted_now = untap_indices(&runner, ranger)
        .into_iter()
        .find(|index| {
            runner.state().objects[&ranger].abilities[*index].granting_object == Some(exile_stamp)
        })
        .expect("the grant survives its own activation");
    assert!(
        !can_activate_ability_now(runner.state(), P0, ranger, granted_now),
        "the spent granted Untap must not be offered again"
    );
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: ranger,
                ability_index: granted_now,
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "repeating the spent granted Untap must be rejected"
    );
    let _ = (bear_a, forest1);
}

/// U2 special-action preservation (CR 116.2k + CR 702.170b): Fblthp plots the
/// top of its controller's library from exile-permission. Plot never touches
/// the stack, so no placement runs and no usage row or counter may appear.
/// (Follows the `dandan_look_top_of_library::top_plot` harness.)
#[test]
fn fblthp_plot_leaves_no_usage_row() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let top = scenario.add_real_card(P0, "Mental Note", Zone::Library, db);
    scenario.add_real_card(P0, "Island", Zone::Library, db);
    let _fblthp = scenario.add_real_card(P0, "Fblthp, Lost on the Range", Zone::Battlefield, db);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Reach-guards: the pile top carries the library plot ability and the
    // action list offers it.
    let index = activated_ability_definitions(runner.state(), top)
        .into_iter()
        .find(|(_, def)| def.activation_zone == Some(Zone::Library))
        .map(|(index, _)| index)
        .expect("the library top must carry the plot ability");
    assert!(
        engine::ai_support::legal_actions(runner.state())
            .iter()
            .any(
                |a| matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == top)
            ),
        "the plot must be offered"
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: top,
            ability_index: index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the holder plots the library top");
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Exile,
        "the plotted card must be exiled"
    );

    // Preservation: a special action (CR 116.2k) records no usage row and no
    // counter for the plot ability.
    assert!(
        journal_rows_for(&runner)
            .iter()
            .all(|row| row.source != top),
        "plot must leave no journal row"
    );
    assert!(
        !runner
            .state()
            .activated_abilities_this_turn
            .keys()
            .any(|key| key.source == top),
        "plot must leave no turn counter"
    );
    assert!(
        !runner
            .state()
            .activated_abilities_this_game
            .keys()
            .any(|key| key.source == top),
        "plot must leave no game counter"
    );
}

/// U2 printed-path preservation (CR 400.7 + CR 602.2b): printed activated
/// abilities whose costs move their own sources still resolve their slots
/// post-payment, so the announced capture and the old derivation agree —
/// each records exactly one journal row and one counter. Ainok Strike Leader
/// sacrifices itself; Drannith Stinger cycles (discards itself).
#[test]
fn printed_self_cost_activations_still_record() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ainok = scenario.add_real_card(P0, "Ainok Strike Leader", Zone::Battlefield, db);
    let stinger = scenario.add_real_card(P0, "Drannith Stinger", Zone::Hand, db);
    for _ in 0..5 {
        scenario.add_card_to_library_top(P0, "Plains");
    }
    fund(&mut scenario, P0, 4);
    let mut runner = scenario.build();

    // Reach-guards: one printed activated ability each.
    let ainok_index = activated_ability_definitions(runner.state(), ainok)
        .into_iter()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(
        ainok_index.len(),
        1,
        "Ainok must offer its sacrifice ability"
    );
    let stinger_index = activated_ability_definitions(runner.state(), stinger)
        .into_iter()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(stinger_index.len(), 1, "the Stinger must offer its cycling");
    let (ainok_index, stinger_index) = (ainok_index[0], stinger_index[0]);

    runner.activate(ainok, ainok_index).resolve();
    assert_settled(&runner);
    assert_eq!(
        runner.state().objects[&ainok].zone,
        Zone::Graveyard,
        "the sacrificed Leader must be in the graveyard"
    );

    runner.activate(stinger, stinger_index).resolve();
    assert_settled(&runner);
    assert_eq!(
        runner.state().objects[&stinger].zone,
        Zone::Graveyard,
        "the cycled Stinger must be in the graveyard"
    );

    // Preservation: exactly one journal row and one counter per source.
    let rows = journal_rows_for(&runner);
    assert_eq!(
        rows.len(),
        2,
        "both printed activations must journal, got {:?}",
        rows.iter().map(|row| row.source).collect::<Vec<_>>()
    );
    for (source, index) in [(ainok, ainok_index), (stinger, stinger_index)] {
        assert!(
            rows.iter().any(|row| row.source == source),
            "a journal row must exist for {source:?}"
        );
        let key = ability_usage_key(runner.state(), source, index)
            .expect("printed slots stay resolvable");
        assert_eq!(
            runner
                .state()
                .activated_abilities_this_turn
                .get(&key)
                .copied(),
            Some(1),
            "the printed activation must be counted once"
        );
    }
}

/// T2 (B2): a Magical Hack rewrite preserves the spent activation budget
/// (CR 612.1 + CR 602.5c).
///
/// Board: two Quirion Rangers (one spent, one fresh), a Plains and
/// Forests for the rewritten and printed bounce costs, and Magical Hacks.
/// Refusals are probed through the raw announcement action (the fluent
/// driver's announcement step panics instead of returning `Err`).
/// Ranger A activates once (spent). Hack rewrites Forest→Plains on Ranger A;
/// the text changes but the ability persists (CR 612.1), so the spent
/// budget follows it and the second activation is still refused — the
/// post-rewrite live derivation equals the announced key because the
/// fingerprint is deworded through the engine's own carrier classifier.
/// Ranger B is hacked while unspent and still activates once afterwards (no
/// phantom spend), then refuses its own second activation.
///
/// Verbatim Oracle texts (Scryfall): Quirion Ranger — "Return a Forest you
/// control to its owner's hand: Untap target creature. Activate only once
/// each turn." Magical Hack — "Change the text of target spell or permanent
/// by replacing all instances of one basic land type with another. (For
/// example, you may change "swampwalk" to "plainswalk." This effect lasts
/// indefinitely.)"
///
/// Revert detector: with today's word-sensitive fingerprint the post-rewrite
/// re-derivation misses the journal and Ranger A's second activation
/// succeeds.
#[test]
fn rewrite_preserves_spent_activation_budget() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ranger_a = scenario.add_real_card(P0, "Quirion Ranger", Zone::Battlefield, db);
    let ranger_b = scenario.add_real_card(P0, "Quirion Ranger", Zone::Battlefield, db);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let plains = scenario.add_real_card(P0, "Plains", Zone::Battlefield, db);
    let forest1 = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);
    let hack = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
    let hack_b = scenario.add_real_card(P0, "Magical Hack", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    let index_a = untap_indices(&runner, ranger_a)
        .into_iter()
        .next()
        .expect("Ranger A must carry its printed Untap");
    tap_for_untap(&mut runner, bear);

    // Unit stability (no board): the fingerprint is invariant under the
    // engine's own rewrite on a Forest-cost definition.
    let def_a = activated_ability_definitions(runner.state(), ranger_a)
        .into_iter()
        .find(|(index, _)| *index == index_a)
        .map(|(_, def)| def.clone())
        .expect("Ranger A's definition resolves");
    let substitution = TextSubstitution::BasicLandType {
        from: BasicLandType::Forest,
        to: BasicLandType::Plains,
    };
    let (rewritten_a, count) = substitution
        .rewrite_counted(&def_a)
        .expect("the Forest cost sits at a classified carrier");
    assert!(
        count > 0,
        "the rewrite must move at least one word, got {count}"
    );
    assert_eq!(
        ability_fingerprint(&def_a),
        ability_fingerprint(&rewritten_a),
        "CR 612.1: dewording erases exactly what a text change may move"
    );

    // Ranger A activates once (spent).
    runner
        .activate(ranger_a, index_a)
        .target_object(bear)
        .pay_with(&[forest1])
        .resolve();
    assert!(
        !runner.state().objects[&bear].tapped,
        "the bear must be untapped by Ranger A's activation"
    );
    let spent_key =
        ability_usage_key(runner.state(), ranger_a, index_a).expect("the slot resolves");
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(&spent_key)
            .copied(),
        Some(1),
        "Ranger A's budget is spent before any rewrite"
    );

    // Reach-guard: the pre-rewrite second activation is refused (the budget
    // existed before the Hack, so a post-rewrite refusal is gate-caused).
    // The refusal is announced through the raw production action: the
    // fluent driver's announcement step panics instead of returning `Err`.
    tap_for_untap(&mut runner, bear);
    let gate_err = match runner.act(GameAction::ActivateAbility {
        source_id: ranger_a,
        ability_index: index_a,
        payment_mode: CastPaymentMode::Auto,
    }) {
        Err(err) => err,
        Ok(_) => panic!("the spent budget must refuse before any rewrite"),
    };
    let gate_msg = format!("{gate_err:?}");

    // Hack Ranger A: Forest becomes Plains in its cost.
    runner
        .cast(hack)
        .target_objects(&[ranger_a])
        .choose_option("Forest -> Plains")
        .resolve();
    assert_settled(&runner);

    // The rewritten cost is payable (the Plains is on the battlefield), yet
    // the activation is refused with the SAME gate verdict as pre-rewrite —
    // gate-caused, not cost-caused. Revert detector: this succeeds pre-fix.
    assert_eq!(
        runner.state().objects[&plains].zone,
        Zone::Battlefield,
        "the Plains must be on the battlefield to pay the rewritten cost"
    );
    let index_now = untap_indices(&runner, ranger_a)
        .into_iter()
        .next()
        .expect("the rewrite keeps the ability on its slot");
    let err_now = match runner.act(GameAction::ActivateAbility {
        source_id: ranger_a,
        ability_index: index_now,
        payment_mode: CastPaymentMode::Auto,
    }) {
        Err(err) => err,
        Ok(_) => panic!("the spent budget must survive the rewrite"),
    };
    assert_eq!(
        format!("{err_now:?}"),
        gate_msg,
        "CR 602.5c: post-rewrite refusal is the same gate verdict, not a cost failure"
    );

    // Hostile: hacking the UNSPENT Ranger B fabricates no count — it
    // activates once under its rewritten (Plains) cost, then refuses its
    // own second activation.
    runner
        .cast(hack_b)
        .target_objects(&[ranger_b])
        .choose_option("Forest -> Plains")
        .resolve();
    assert_settled(&runner);
    let index_b = untap_indices(&runner, ranger_b)
        .into_iter()
        .next()
        .expect("the rewrite keeps Ranger B on its slot");
    runner
        .activate(ranger_b, index_b)
        .target_object(bear)
        .pay_with(&[plains])
        .resolve();
    assert!(
        !runner.state().objects[&bear].tapped,
        "the bear must be untapped by Ranger B's first activation"
    );
    assert_eq!(
        runner
            .state()
            .activated_abilities_this_turn
            .get(
                &ability_usage_key(runner.state(), ranger_b, index_b)
                    .expect("Ranger B's slot resolves")
            )
            .copied(),
        Some(1),
        "Ranger B's first activation counts exactly once (no phantom spend)"
    );
    tap_for_untap(&mut runner, bear);
    assert!(
        runner
            .act(GameAction::ActivateAbility {
                source_id: ranger_b,
                ability_index: index_b,
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "Ranger B's own second activation must be refused"
    );
}
