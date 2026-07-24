//! CR 509.1b + CR 611.2c + CR 613.1f: client projection for temporary
//! `CantBeBlocked` grants, sourced only from current Layer 6 attribution.

use engine::game::derived_views::{
    derive_filtered_views, derive_views, ClientGameState, ClientGameStateRef,
};
use engine::game::filter_state_for_viewer;
use engine::game::game_object::PhaseOutCause;
use engine::game::layers::evaluate_layers;
use engine::game::phasing::phase_out_object;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::{ContinuousModification, Duration, TargetFilter};
use engine::types::attribution::EffectRef;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::layers::Layer;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

fn grant_cant_be_blocked_until_end_of_turn(
    state: &mut GameState,
    source: engine::types::identifiers::ObjectId,
    recipient: engine::types::identifiers::ObjectId,
) {
    state.add_transient_continuous_effect(
        source,
        P0,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificObject { id: recipient },
        vec![
            ContinuousModification::AddPower { value: 1 },
            ContinuousModification::AddStaticMode {
                mode: StaticMode::CantBeBlocked,
            },
        ],
        None,
    );
}

#[test]
fn derives_only_the_live_until_end_of_turn_cant_be_blocked_attribution() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Grant Source", 0, 1).id();
    let recipient = scenario.add_creature(P0, "Grant Recipient", 2, 2).id();
    let permanent_source = scenario.add_creature(P0, "Permanent Grant", 0, 1).id();
    let permanent_recipient = scenario.add_creature(P0, "Permanent Recipient", 2, 2).id();
    let mut runner = scenario.build();

    // The leading P/T modification proves the projection follows the exact
    // recorded `mod_index`, not merely the owning transient effect.
    grant_cant_be_blocked_until_end_of_turn(runner.state_mut(), source, recipient);
    runner.state_mut().add_transient_continuous_effect(
        permanent_source,
        P0,
        Duration::Permanent,
        TargetFilter::SpecificObject {
            id: permanent_recipient,
        },
        vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::CantBeBlocked,
        }],
        None,
    );
    evaluate_layers(runner.state_mut());

    assert!(
        runner.state().objects[&recipient]
            .static_definitions
            .iter()
            .any(|definition| definition.mode == StaticMode::CantBeBlocked),
        "reach guard: Layer 6 applied the temporary CantBeBlocked modification"
    );

    let views = derive_views(runner.state(), None);
    assert_eq!(
        views.temporary_cant_be_blocked.get(&recipient),
        Some(&Some(source)),
        "the first matching live UET AddStaticMode attribution carries its battlefield source"
    );
    assert!(
        !views
            .temporary_cant_be_blocked
            .contains_key(&permanent_recipient),
        "a permanent CantBeBlocked grant must not be presented as temporary"
    );
}

#[test]
fn wrapper_round_trip_keeps_the_badge_but_hides_a_departed_source() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Grant Source", 0, 1).id();
    let recipient = scenario.add_creature(P0, "Grant Recipient", 2, 2).id();
    let mut runner = scenario.build();
    grant_cant_be_blocked_until_end_of_turn(runner.state_mut(), source, recipient);
    evaluate_layers(runner.state_mut());

    let source_owner = runner.state().objects[&source].owner;
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);
    assert_eq!(
        runner.state().objects[&source].owner,
        source_owner,
        "reach guard: moving the source preserves the tracked source object"
    );
    evaluate_layers(runner.state_mut());

    let direct = derive_views(runner.state(), Some(P1));
    assert_eq!(
        direct.temporary_cant_be_blocked.get(&recipient),
        Some(&None),
        "the live grant keeps its recipient badge but withholds a non-battlefield source"
    );

    let filtered = filter_state_for_viewer(runner.state(), P1);
    let filtered_views = derive_filtered_views(runner.state(), &filtered, Some(P1));
    assert_eq!(
        filtered_views.temporary_cant_be_blocked.get(&recipient),
        Some(&None),
        "filtered derivation uses the public filtered state and preserves the source-less badge"
    );

    let json = serde_json::to_string(&ClientGameStateRef::wrap_filtered(
        runner.state(),
        &filtered,
        Some(P1),
    ))
    .expect("serialize filtered client state");
    let wire: serde_json::Value = serde_json::from_str(&json).expect("inspect client wire shape");
    assert!(
        wire["derived"]["temporary_cant_be_blocked"]
            .get(recipient.0.to_string())
            .is_some_and(serde_json::Value::is_null),
        "the wire map represents a departed source as null rather than leaking its id"
    );
    let round: ClientGameState = serde_json::from_str(&json).expect("deserialize client state");
    assert_eq!(
        round.derived.temporary_cant_be_blocked.get(&recipient),
        Some(&None),
        "the owned client wrapper round-trips the source-less badge"
    );

    let empty_json = serde_json::to_string(&ClientGameStateRef::wrap(
        &GameState::new_two_player(42),
        None,
    ))
    .expect("serialize empty client state");
    assert!(
        !empty_json.contains("temporary_cant_be_blocked"),
        "the empty temporary map is omitted from the wire payload"
    );
}

#[test]
fn phased_out_source_keeps_the_badge_but_hides_its_attribution() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Grant Source", 0, 1).id();
    let recipient = scenario.add_creature(P0, "Grant Recipient", 2, 2).id();
    let mut runner = scenario.build();
    grant_cant_be_blocked_until_end_of_turn(runner.state_mut(), source, recipient);
    evaluate_layers(runner.state_mut());

    let mut events = Vec::new();
    phase_out_object(
        runner.state_mut(),
        source,
        PhaseOutCause::Directly,
        &mut events,
    );
    evaluate_layers(runner.state_mut());

    assert!(
        runner.state().objects[&source].is_phased_out(),
        "reach guard: the source remains in the battlefield zone while phased out"
    );
    assert_eq!(
        derive_views(runner.state(), None)
            .temporary_cant_be_blocked
            .get(&recipient),
        Some(&None),
        "a phased-out source is not public attribution for a still-live temporary grant"
    );
}

#[test]
fn first_departed_grant_withholds_later_public_source_attribution() {
    let mut scenario = GameScenario::new();
    let first_source = scenario.add_creature(P0, "First Grant Source", 0, 1).id();
    let later_source = scenario.add_creature(P0, "Later Grant Source", 0, 1).id();
    let recipient = scenario.add_creature(P0, "Grant Recipient", 2, 2).id();
    let mut runner = scenario.build();
    grant_cant_be_blocked_until_end_of_turn(runner.state_mut(), first_source, recipient);
    grant_cant_be_blocked_until_end_of_turn(runner.state_mut(), later_source, recipient);
    evaluate_layers(runner.state_mut());

    let mut events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        first_source,
        Zone::Graveyard,
        &mut events,
    );
    evaluate_layers(runner.state_mut());

    assert!(
        runner.state().objects[&later_source].is_phased_in(),
        "reach guard: the later matching source remains public"
    );
    assert_eq!(
        derive_views(runner.state(), None)
            .temporary_cant_be_blocked
            .get(&recipient),
        Some(&None),
        "the first matching grant remains authoritative, so its departed source withholds attribution instead of falling through to a later source"
    );
}

const ROGUES_PASSAGE_ORACLE: &str =
    "{T}: Add {C}.\n{4}, {T}: Target creature can't be blocked this turn.";

/// CR 509.1b + CR 611.2c + CR 613.1f + CR 514.2: a resolved Rogues Passage
/// activation registers a target-bound, Layer 6 `CantBeBlocked` grant, which
/// remains client-visible only through this turn's cleanup.
#[test]
fn rogues_passage_activation_projects_only_its_target_until_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let passage = scenario
        .add_land_from_oracle(P0, "Rogues Passage", ROGUES_PASSAGE_ORACLE)
        .id();
    let target = scenario.add_creature(P0, "Target Creature", 2, 2).id();
    let non_target = scenario.add_creature(P0, "Non-target Creature", 2, 2).id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    // The verbatim Oracle text supplies the mana ability first and the target
    // grant second; activate the latter through the production target-selection
    // and resolution pipeline rather than installing a synthetic effect.
    runner.activate(passage, 1).target_object(target).resolve();
    evaluate_layers(runner.state_mut());

    let ability_attribution = runner.state().attribution[&target]
        .by_layer
        .get(&Layer::Ability)
        .expect("the target must have a live Layer 6 attribution");
    assert!(
        ability_attribution.iter().any(|effect_ref| {
            let EffectRef::Transient { id, mod_index } = effect_ref else {
                return false;
            };
            runner
                .state()
                .transient_continuous_effects
                .iter()
                .find(|effect| effect.id == *id)
                .is_some_and(|effect| {
                    effect.source_id == passage
                        && effect.duration == Duration::UntilEndOfTurn
                        && matches!(
                            effect.modifications.get(*mod_index),
                            Some(ContinuousModification::AddStaticMode {
                                mode: StaticMode::CantBeBlocked
                            })
                        )
                })
        }),
        "the real Rogues Passage grant must be attributed as its live UET Layer 6 CantBeBlocked modification"
    );

    let views = derive_views(runner.state(), None);
    assert_eq!(
        views.temporary_cant_be_blocked.get(&target),
        Some(&Some(passage)),
        "the targeted creature carries a temporary CantBeBlocked badge attributed to Rogues Passage"
    );
    assert!(
        !views.temporary_cant_be_blocked.contains_key(&non_target),
        "a creature not selected during activation must not receive the badge"
    );

    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::Upkeep);
    evaluate_layers(runner.state_mut());
    assert!(
        !derive_views(runner.state(), None)
            .temporary_cant_be_blocked
            .contains_key(&target),
        "CR 514.2: the this-turn CantBeBlocked grant must disappear after cleanup"
    );
}
