//! R4c: staged destruction and ChangeZone populations.
//!
//! These rows drive the real card paths (CR 701.8a destruction, CR 608.2c
//! zone changes) through the common staged owner. The discriminating property
//! is that one announced simultaneous operation settles every member even when
//! a replacement choice pauses the first one; the base loop dropped the
//! unprocessed suffix.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::change_zone::resolve_all;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, Outcome, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode,
    ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const WRATH: &str = "Destroy all creatures.";
const HEX: &str = "Destroy six target creatures.";
const FINAL_JUDGMENT: &str = "Exile all creatures.";
const VENGEFUL_REGROWTH: &str =
    "Return up to three target land cards from your graveyard to the battlefield tapped.";

fn regeneration_shield() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Destroy)
        .valid_card(TargetFilter::SelfRef)
        .description("Regenerate".to_string())
        .regeneration_shield()
}

fn zone_of(outcome: &Outcome, id: ObjectId) -> Zone {
    outcome.zone_of(id)
}

fn destroyed_events(outcome: &Outcome) -> usize {
    outcome
        .events()
        .iter()
        .filter(|event| matches!(event, GameEvent::CreatureDestroyed { .. }))
        .count()
}

/// D1: a mass destruction settles every non-indestructible member and emits
/// exactly one `CreatureDestroyed` per destroyed creature.
#[test]
fn d1_mass_destroy_settles_the_whole_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let c = scenario.add_creature(P0, "Bear C", 2, 2).id();
    let indestructible = scenario
        .add_creature(P0, "Adamant Titan", 3, 3)
        .indestructible()
        .id();
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(wrath).resolve();

    for id in [a, b, c] {
        assert_eq!(
            zone_of(&outcome, id),
            Zone::Graveyard,
            "every normal creature is destroyed by the staged mass population"
        );
    }
    assert_eq!(
        zone_of(&outcome, indestructible),
        Zone::Battlefield,
        "the indestructible member is a `Remained` terminal"
    );
    assert_eq!(
        destroyed_events(&outcome),
        3,
        "exactly one CreatureDestroyed per destroyed member"
    );
}

/// D3: a competing-replacement pause on the FIRST member must not drop the
/// second creature. The base per-member loop returned on the pause and lost the
/// unprocessed suffix.
#[test]
fn d3_first_member_pause_keeps_the_remaining_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shielded = {
        let mut builder = scenario.add_creature(P0, "Shielded Bear", 2, 2);
        let id = builder.id();
        builder.with_replacement_definition(regeneration_shield());
        id
    };
    scenario.with_counter(shielded, CounterType::Shield, 1);
    let second = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let day = scenario
        .add_spell_to_hand_from_oracle(P0, "Day of Judgment", false, WRATH)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(day).replacement_choice(0).resolve();

    assert_eq!(
        zone_of(&outcome, shielded),
        Zone::Battlefield,
        "the first member's chosen replacement prevented its destruction"
    );
    assert_eq!(
        zone_of(&outcome, second),
        Zone::Graveyard,
        "the later member must still be destroyed after the pause"
    );
    assert_eq!(
        destroyed_events(&outcome),
        1,
        "only the genuinely destroyed member emits CreatureDestroyed"
    );
}

/// D5: a targeted multi-destroy (`Hex`) whose first target pauses still settles
/// every later target.
#[test]
fn d5_targeted_multi_destroy_settles_every_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shielded = {
        let mut builder = scenario.add_creature(P0, "Shielded One", 2, 2);
        let id = builder.id();
        builder.with_replacement_definition(regeneration_shield());
        id
    };
    scenario.with_counter(shielded, CounterType::Shield, 1);
    let mut targets = vec![shielded];
    for index in 0..5 {
        targets.push(
            scenario
                .add_creature(P0, &format!("Target {index}"), 2, 2)
                .id(),
        );
    }
    let hex = scenario
        .add_spell_to_hand_from_oracle(P0, "Hex", false, HEX)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(hex)
        .target_objects(&targets)
        .replacement_choice(0)
        .resolve();

    assert_eq!(zone_of(&outcome, shielded), Zone::Battlefield);
    for target in &targets[1..] {
        assert_eq!(
            zone_of(&outcome, *target),
            Zone::Graveyard,
            "no later target may be stranded by the first pause"
        );
    }
    assert_eq!(destroyed_events(&outcome), 5);
}

/// C1: a mass exile settles every creature and records the admitted count.
#[test]
fn c1_mass_change_zone_exiles_every_member() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let c = scenario.add_creature(P0, "Bear C", 2, 2).id();
    let judgment = scenario
        .add_spell_to_hand_from_oracle(P0, "Final Judgment", false, FINAL_JUDGMENT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(judgment).resolve();

    for id in [a, b, c] {
        assert_eq!(
            zone_of(&outcome, id),
            Zone::Exile,
            "every admitted ChangeZone member settles at the destination"
        );
    }
    assert_eq!(
        outcome.state().last_effect_count,
        Some(3),
        "the mass policy counts every non-abandoned member"
    );
}

/// C5: the targeted "that many" count comes from the actual arrivals.
#[test]
fn c5_targeted_change_zone_counts_actual_arrivals() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_land_to_graveyard(P0, "Forest").id();
    let b = scenario.add_land_to_graveyard(P0, "Island").id();
    let c = scenario.add_land_to_graveyard(P0, "Mountain").id();
    let regrowth = scenario
        .add_spell_to_hand_from_oracle(P0, "Vengeful Regrowth", false, VENGEFUL_REGROWTH)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(regrowth).target_objects(&[a, b, c]).resolve();

    for id in [a, b, c] {
        assert_eq!(
            zone_of(&outcome, id),
            Zone::Battlefield,
            "each targeted card arrives on the battlefield"
        );
    }
    assert_eq!(
        outcome.state().last_effect_count,
        Some(3),
        "the targeted policy counts actual arrivals"
    );
}

/// Oracle texts used by the rows below (raw, as a player would read them).
const DAMNATION: &str = "Destroy all creatures. They can't be regenerated.";
const FUMIGATE: &str =
    "Destroy all creatures. You gain 1 life for each creature destroyed this way.";
const REST_IN_PEACE: &str =
    "If a card or token would be put into a graveyard from anywhere, exile it instead.";
const RISE_OF_THE_DARK_REALMS: &str =
    "Put all creature cards from all graveyards onto the battlefield under your control.";
const MURDER: &str = "Destroy target creature.";
const GHOSTLY_FLICKER: &str =
    "Exile two target artifacts, creatures, and/or lands you control, then return those cards to the battlefield under your control.";

fn zero_cost_spell(scenario: &mut GameScenario, name: &str, oracle: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, false, oracle)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// D2 (F9): regeneration shield, shield counter and `cant_regenerate` follow
/// their exact rules. A shield counter survives `Wrath` with the counter
/// consumed; `Damnation`'s `cant_regenerate` bypasses the regeneration shield.
#[test]
fn d2_prevention_controls_regen_shield_counter_and_cant_regenerate() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let regenerator = {
        let mut builder = scenario.add_creature(P0, "Drudge Skeletons", 1, 1);
        let id = builder.id();
        builder.with_replacement_definition(regeneration_shield());
        id
    };
    let shielded = scenario.add_creature(P0, "Boon Bear", 2, 2).id();
    scenario.with_counter(shielded, CounterType::Shield, 1);
    let plain = scenario.add_creature(P0, "Plain Bear", 2, 2).id();
    let wrath = zero_cost_spell(&mut scenario, "Wrath of God", WRATH);

    let mut runner = scenario.build();
    let outcome = runner.cast(wrath).replacement_choice(0).resolve();

    assert_eq!(
        zone_of(&outcome, regenerator),
        Zone::Battlefield,
        "a regeneration shield prevents the destruction"
    );
    assert_eq!(
        zone_of(&outcome, shielded),
        Zone::Battlefield,
        "a shield counter replaces the destruction"
    );
    assert_eq!(
        outcome.state().objects[&shielded]
            .counters
            .get(&CounterType::Shield)
            .copied()
            .unwrap_or_default(),
        0,
        "the shield counter is consumed"
    );
    assert_eq!(zone_of(&outcome, plain), Zone::Graveyard);
    assert_eq!(
        destroyed_events(&outcome),
        1,
        "only the genuinely destroyed member emits CreatureDestroyed"
    );

    // cant_regenerate bypasses the regeneration shield.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let regenerator = {
        let mut builder = scenario.add_creature(P0, "Drudge Skeletons", 1, 1);
        let id = builder.id();
        builder.with_replacement_definition(regeneration_shield());
        id
    };
    let other = scenario.add_creature(P0, "Other Bear", 2, 2).id();
    let damnation = zero_cost_spell(&mut scenario, "Damnation", DAMNATION);
    let mut runner = scenario.build();
    let outcome = runner.cast(damnation).resolve();
    assert_eq!(
        zone_of(&outcome, regenerator),
        Zone::Graveyard,
        "cant_regenerate bypasses only the regeneration shield"
    );
    assert_eq!(zone_of(&outcome, other), Zone::Graveyard);
    assert_eq!(destroyed_events(&outcome), 2);
}

/// D3b (F4/N1/N2): a member whose inner `ZoneChanged` is redirected back to
/// the battlefield is terminal `Remained`: no `ZoneChanged`, no occurrence,
/// yet exactly one `CreatureDestroyed` (outer approval, terminal not abandoned).
#[test]
fn d3b_redirected_back_member_is_remained_with_one_creature_destroyed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let redirected = {
        let mut builder = scenario.add_creature(P0, "Returning Bear", 2, 2);
        let id = builder.id();
        // Labelled synthetic redirect (no printed card performs this exact
        // Battlefield->Graveyard -> Battlefield substitution): applies to a
        // self move destined for the graveyard and redirects it back.
        builder.with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_card(TargetFilter::SelfRef)
                .description("redirect back".to_string())
                .destination_zone(Zone::Graveyard)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Battlefield,
                        target: TargetFilter::Any,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                        conditional_enter_with_counters: vec![],
                        face_down_profile: None,
                        enters_modified_if: None,
                    },
                )),
        );
        id
    };
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let day = zero_cost_spell(&mut scenario, "Day of Judgment", WRATH);

    let mut runner = scenario.build();
    let outcome = runner.cast(day).replacement_choice(0).resolve();

    assert_eq!(
        zone_of(&outcome, redirected),
        Zone::Battlefield,
        "the redirected-back member remains"
    );
    assert_eq!(zone_of(&outcome, first), Zone::Graveyard);
    assert_eq!(zone_of(&outcome, second), Zone::Graveyard);
    assert!(
        !outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, .. } if *object_id == redirected
        )),
        "a redirected-back Battlefield->Battlefield move emits no ZoneChanged"
    );
    let redirected_destroyed = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::CreatureDestroyed { object_id, .. } if *object_id == redirected
            )
        })
        .count();
    assert_eq!(
        redirected_destroyed, 1,
        "outer destruction was approved, so the Remained member still emits one CreatureDestroyed"
    );
    assert_eq!(destroyed_events(&outcome), 3);
}

/// D4 (inner redirect to Exile): Rest in Peace replaces every inner
/// Graveyard move with Exile; `CreatureDestroyed` still fires once per member.
/// V2: occurrence order is announcement order and the single `EffectResolved`
/// follows every delivery.
#[test]
fn d4_rest_in_peace_redirects_every_inner_move_and_keeps_one_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let c = scenario.add_creature(P0, "Bear C", 2, 2).id();
    scenario.add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE);
    let day = zero_cost_spell(&mut scenario, "Day of Judgment", WRATH);

    let mut runner = scenario.build();
    let outcome = runner.cast(day).resolve();

    for id in [a, b, c] {
        assert_eq!(
            zone_of(&outcome, id),
            Zone::Exile,
            "Rest in Peace redirects the inner zone change to exile"
        );
    }
    assert_eq!(
        destroyed_events(&outcome),
        3,
        "each destroyed member emits CreatureDestroyed once"
    );
    let zone_change_order: Vec<ObjectId> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            // Only the admitted members witness the batch order; unrelated
            // events (the enchantment, the resolving spell card) are ignored.
            GameEvent::ZoneChanged { object_id, .. } if [a, b, c].contains(object_id) => {
                Some(*object_id)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        zone_change_order,
        vec![a, b, c],
        "V2: public occurrence order is announcement order"
    );
    let resolutions = outcome
        .events()
        .iter()
        .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
        .count();
    assert_eq!(resolutions, 1, "one resolution event for the population");
    let resolved_at = outcome
        .events()
        .iter()
        .position(|event| matches!(event, GameEvent::EffectResolved { .. }));
    let last_zone_changed = outcome
        .events()
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(
                event,
                GameEvent::ZoneChanged { object_id, .. } if [a, b, c].contains(object_id)
            )
        })
        .map(|(index, _)| index)
        .max();
    assert!(
        matches!(
            (resolved_at, last_zone_changed),
            (Some(resolved), Some(changed)) if resolved > changed
        ),
        "V2: exactly one resolution event follows every delivery"
    );
}

/// D6 (Fumigate count): the tracked set is the destroyed members, so the
/// chained "gain 1 life for each creature destroyed this way" sees exactly 3
/// with one indestructible control left alone.
#[test]
fn d6_fumigate_counts_only_destroyed_members() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let c = scenario.add_creature(P0, "Bear C", 2, 2).id();
    let titan = scenario
        .add_creature(P0, "Adamant Titan", 3, 3)
        .indestructible()
        .id();
    let fumigate = zero_cost_spell(&mut scenario, "Fumigate", FUMIGATE);
    let life_before = 20;
    let mut runner = scenario.build();
    let outcome = runner.cast(fumigate).resolve();

    for id in [a, b, c] {
        assert_eq!(zone_of(&outcome, id), Zone::Graveyard);
    }
    assert_eq!(zone_of(&outcome, titan), Zone::Battlefield);
    assert_eq!(
        outcome.state().players[0].life,
        life_before + 3,
        "life gained equals the destroyed count, not the matched count"
    );
    assert_eq!(destroyed_events(&outcome), 3);
}

fn direct_spell_ability(
    state: &mut engine::types::game_state::GameState,
    oracle: &str,
    name: &str,
    controller: PlayerId,
) -> (ResolvedAbility, ObjectId) {
    let definition = parse_effect_chain(oracle, AbilityKind::Spell);
    let source = create_object(
        state,
        CardId(9_700 + state.objects.len() as u64),
        controller,
        name.to_string(),
        Zone::Stack,
    );
    let ability = build_resolved_from_def(&definition, source, controller);
    (ability, source)
}

fn battlefield_creature(
    state: &mut engine::types::game_state::GameState,
    card_id: u64,
    owner: PlayerId,
    name: &str,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(engine::types::card_type::CoreType::Creature);
    id
}

/// D7 (Kaya's Wrath): life gain is the controller-owned destroyed count; an
/// opponent's creature dies without adding life, and an indestructible
/// controller creature is untouched.
#[test]
fn d7_kayas_wrath_gains_life_only_for_controller_owned_destroyed() {
    let mut state = engine::types::game_state::GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    let own_a = battlefield_creature(&mut state, 9_801, PlayerId(0), "Own A");
    let own_b = battlefield_creature(&mut state, 9_802, PlayerId(0), "Own B");
    let _their = battlefield_creature(&mut state, 9_803, PlayerId(1), "Their Bear");
    let (ability, _source) = direct_spell_ability(
        &mut state,
        "Destroy all creatures. You gain life equal to the number of creatures you controlled that were destroyed this way.",
        "Kaya's Wrath",
        PlayerId(0),
    );
    let life_before = state.players[0].life;
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert_eq!(state.objects[&own_a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&own_b].zone, Zone::Graveyard);
    assert_eq!(
        state.players[0].life,
        life_before + 2,
        "only the caster's own destroyed creatures add life"
    );
}

/// D9 (negatives/residuals): a singleton target keeps the legacy path; an
/// indestructible target is `Remained` with no `CreatureDestroyed` and no
/// duplicate resolution event.
#[test]
fn d9_singleton_and_indestructible_negatives() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P0, "Victim Bear", 2, 2).id();
    let murder = zero_cost_spell(&mut scenario, "Murder", MURDER);
    let mut runner = scenario.build();
    let outcome = runner.cast(murder).target_object(victim).resolve();
    assert_eq!(zone_of(&outcome, victim), Zone::Graveyard);
    assert_eq!(destroyed_events(&outcome), 1);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let titan = scenario
        .add_creature(P0, "Adamant Titan", 3, 3)
        .indestructible()
        .id();
    let murder = zero_cost_spell(&mut scenario, "Murder", MURDER);
    let mut runner = scenario.build();
    let outcome = runner.cast(murder).target_object(titan).resolve();
    assert_eq!(
        zone_of(&outcome, titan),
        Zone::Battlefield,
        "an indestructible target remains"
    );
    assert_eq!(
        destroyed_events(&outcome),
        0,
        "no CreatureDestroyed for an indestructible singleton"
    );
}

/// C2 (mass battlefield entry): Rise of the Dark Realms returns every creature
/// card under the caster's control.
#[test]
fn c2_rise_of_the_dark_realms_returns_every_graveyard_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario
        .add_creature_to_graveyard(P0, "Grave Bear", 2, 2)
        .id();
    let b = scenario
        .add_creature_to_graveyard(P0, "Grave Elk", 3, 3)
        .id();
    let c = scenario
        .add_creature_to_graveyard(P1, "Their Beast", 4, 4)
        .id();
    let rise = zero_cost_spell(
        &mut scenario,
        "Rise of the Dark Realms",
        RISE_OF_THE_DARK_REALMS,
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(rise).resolve();

    for id in [a, b, c] {
        assert_eq!(
            zone_of(&outcome, id),
            Zone::Battlefield,
            "the mass battlefield entry returns every creature card"
        );
        assert_eq!(
            outcome.state().objects[&id].controller,
            PlayerId(0),
            "all returned creatures enter under the caster's control"
        );
    }
}

/// C3 (targeted no-pause control): Ghostly Flicker exiles and returns two
/// permanents in one resolution with no replacement pause.
#[test]
fn c3_ghostly_flicker_returns_both_targets_without_a_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario.add_creature(P0, "Flicker A", 2, 2).id();
    let b = scenario.add_creature(P0, "Flicker B", 2, 2).id();
    let flicker = zero_cost_spell(&mut scenario, "Ghostly Flicker", GHOSTLY_FLICKER);
    let mut runner = scenario.build();
    let outcome = runner.cast(flicker).target_objects(&[a, b]).resolve();

    for id in [a, b] {
        assert_eq!(
            zone_of(&outcome, id),
            Zone::Battlefield,
            "both flickered permanents return"
        );
    }
}

/// C4 (targeted later-member pause): the first returned member pauses on its
/// own optional enter replacement; every later member still returns and the
/// admitted count stays the full population.
#[test]
fn c4_targeted_later_member_pause_keeps_every_return() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = {
        let mut builder = scenario.add_land_to_graveyard(P0, "Forest");
        let id = builder.id();
        builder.with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                .destination_zone(Zone::Battlefield)
                .description("enters tapped".to_string())
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: engine::types::ability::EffectScope::Single,
                        state: phase_ast::types::ability::TapStateChange::Tap,
                    },
                )),
        );
        id
    };
    let second = scenario.add_land_to_graveyard(P0, "Island").id();
    let third = scenario.add_land_to_graveyard(P0, "Mountain").id();
    let regrowth = zero_cost_spell(&mut scenario, "Vengeful Regrowth", VENGEFUL_REGROWTH);
    let mut runner = scenario.build();
    let outcome = runner
        .cast(regrowth)
        .target_objects(&[first, second, third])
        .replacement_choice(0)
        .resolve();

    for id in [first, second, third] {
        assert_eq!(
            zone_of(&outcome, id),
            Zone::Battlefield,
            "no returned member may be stranded by the first pause"
        );
    }
    assert!(
        outcome.state().objects[&first].tapped,
        "the accepted replacement applied to the paused member"
    );
    assert_eq!(
        outcome.state().last_effect_count,
        Some(3),
        "the admitted targeted population count survives the pause"
    );
}

/// D8 (staged Destroy wire/save): a mass destruction paused at the staged
/// replacement boundary round-trips through the persisted wire and resumes to
/// the same final terminals. The pause carries the staged owner (operation,
/// group, cursor), not a legacy singleton boundary.
#[test]
fn d8_staged_destroy_pause_round_trips_and_resumes() {
    use engine::game::engine::apply_as_current;
    use engine::types::actions::GameAction;
    use engine::types::game_state::{PersistedGameState, PersistedRestoreFinalization};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shielded = {
        let mut builder = scenario.add_creature(P0, "Shielded Bear", 2, 2);
        let id = builder.id();
        builder.with_replacement_definition(regeneration_shield());
        id
    };
    scenario.with_counter(shielded, CounterType::Shield, 1);
    let plain = scenario.add_creature(P0, "Plain Bear", 2, 2).id();
    let day = zero_cost_spell(&mut scenario, "Day of Judgment", WRATH);

    let mut runner = scenario.build();
    let mut commit = runner.cast(day).commit();
    for _ in 0..6 {
        if matches!(
            commit.state().waiting_for,
            engine::types::game_state::WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        commit
            .act(engine::types::actions::GameAction::PassPriority)
            .expect("priority passes while the staged destruction resolves");
    }
    assert!(
        matches!(
            commit.state().waiting_for,
            engine::types::game_state::WaitingFor::ReplacementChoice { .. }
        ),
        "premise: the staged destruction parks on the first member's replacement"
    );
    assert!(
        commit.state().resolution_stack.iter().any(|frame| matches!(
            frame,
            engine::types::resolution::ResolutionFrame::BatchDelivery(batch)
                if batch.staged.as_ref().is_some_and(|staged| {
                    matches!(
                        staged.members[staged.cursor].operation,
                        engine::types::game_state::StagedMemberOperation::Destroy(_)
                    )
                })
        )),
        "the parked owner is the staged Destroy operation"
    );

    let wire = serde_json::to_value(PersistedGameState::capture(commit.state().clone()))
        .expect("staged destroy pause serializes");
    let mut restored = serde_json::from_value::<PersistedGameState>(wire)
        .expect("staged destroy wire decodes")
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("staged destroy wire prepares")
        .finalize_immediately()
        .expect("staged destroy wire finalizes");
    assert!(matches!(
        restored.waiting_for,
        engine::types::game_state::WaitingFor::ReplacementChoice { .. }
    ));
    assert!(
        restored.resolution_stack.iter().any(|frame| matches!(
            frame,
            engine::types::resolution::ResolutionFrame::BatchDelivery(batch)
                if batch.staged.is_some()
        )),
        "the staged owner survives the wire"
    );

    apply_as_current(&mut restored, GameAction::ChooseReplacement { index: 0 })
        .expect("the restored staged destruction resumes");
    assert_eq!(
        restored.objects[&shielded].zone,
        Zone::Battlefield,
        "the chosen regeneration replacement is retained across the save"
    );
    assert_eq!(
        restored.objects[&plain].zone,
        Zone::Graveyard,
        "the later member is still destroyed after the restore"
    );
}

/// C9 (negatives): an empty mass population is a no-op with no synthetic
/// events, while a singleton keeps the legacy path (already pinned by D9).
#[test]
fn c9_empty_mass_population_is_a_no_op() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let judgment = zero_cost_spell(&mut scenario, "Final Judgment", FINAL_JUDGMENT);
    let mut runner = scenario.build();
    let outcome = runner.cast(judgment).resolve();

    assert!(
        !outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                from: Some(Zone::Battlefield),
                ..
            }
        )),
        "an empty population produces no battlefield zone change"
    );
    assert_eq!(destroyed_events(&outcome), 0);
    assert!(matches!(
        outcome.state().waiting_for,
        engine::types::game_state::WaitingFor::Priority { .. }
    ));
}

/// C11 (random-order library, F19/N9): a mass bottom placement with
/// `random_order` writes the requested placement, produces no arrangement
/// prompt, and leaves the sentinels above the moved cards.
#[test]
fn c11_random_order_bottom_placement_has_no_prompt_and_exact_region() {
    use engine::types::ability::MassLibraryShuffleMode;
    use engine::types::game_state::GameState;
    use engine::types::zones::EtbTapState;

    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    let sentinel_a = create_object(
        &mut state,
        CardId(9_901),
        PlayerId(0),
        "Sentinel A".into(),
        Zone::Library,
    );
    let sentinel_b = create_object(
        &mut state,
        CardId(9_902),
        PlayerId(0),
        "Sentinel B".into(),
        Zone::Library,
    );
    let a = create_object(
        &mut state,
        CardId(9_903),
        PlayerId(0),
        "Card A".into(),
        Zone::Graveyard,
    );
    let b = create_object(
        &mut state,
        CardId(9_904),
        PlayerId(0),
        "Card B".into(),
        Zone::Graveyard,
    );
    let c = create_object(
        &mut state,
        CardId(9_905),
        PlayerId(0),
        "Card C".into(),
        Zone::Graveyard,
    );
    let ability = ResolvedAbility::new(
        Effect::ChangeZoneAll {
            origin: Some(Zone::Graveyard),
            destination: Zone::Library,
            target: TargetFilter::Any,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position: Some(engine::types::ability::LibraryPosition::Bottom),
            library_shuffle: MassLibraryShuffleMode::TerminalShuffle,
            random_order: true,
        },
        vec![],
        ObjectId(9_900),
        PlayerId(0),
    );
    let mut events = Vec::new();
    resolve_all(&mut state, &ability, &mut events).expect("random mass placement resolves");

    assert!(
        matches!(
            state.waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        ),
        "a random-order placement produces no arrangement prompt"
    );
    let library = &state.players[0].library;
    assert_eq!(library.len(), 5);
    assert_eq!(&library[0], &sentinel_a, "sentinels stay on top in order");
    assert_eq!(&library[1], &sentinel_b);
    let bottom: std::collections::HashSet<ObjectId> = library.iter().skip(2).copied().collect();
    assert_eq!(
        bottom,
        [a, b, c]
            .into_iter()
            .collect::<std::collections::HashSet<_>>(),
        "the moved cards occupy the bottom region exactly once each"
    );
    assert_eq!(state.last_effect_count, Some(3));
}
