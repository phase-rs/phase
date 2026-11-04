//! R4c: staged destruction and ChangeZone populations.
//!
//! These rows drive the real card paths (CR 701.8a destruction, CR 608.2c
//! zone changes) through the common staged owner. The discriminating property
//! is that one announced simultaneous operation settles every member even when
//! a replacement choice pauses the first one; the base loop dropped the
//! unprocessed suffix.
//!
//! Carried matrix rows (existing coverage, no new row added here):
//! - C7 (Duplicity + Donate): `opponent_gains_control_from_you_trigger::`
//!   (`duplicity_*`) drives the five-card mass staging and the lose-control
//!   trigger; `control_change_evidence_never_reaches_a_viewer` stays green.
//! - C8 (ChangeZone saves): the migrated trigger rows serialize/deserialize a
//!   parked staged ChangeZone at consultation and delivery; the extended C6b
//!   fixture pins the pre-pause negative and completion counts, not the
//!   post-completion moved ledger (the chained consumer's per-effect publish
//!   overwrites it). D8 here covers a raw staged Destroy save. A direct
//!   marker+batch wire round-trip row remains a carried gap.
//! - V1 (privacy): `zone_pipeline::staged_arrangement_prompt_and_payload_are_redacted_for_every_viewer`
//!   (R4b batch-owned Library Top/Bottom arrangement) plus
//!   `a_public_concession_settles_a_staged_arrangement_prompt`.
//! - E1 (elimination): `leaving_owner_cancels_paused_change_zone_member_and_drains_surviving_tail`
//!   and `a_public_concession_settles_a_staged_arrangement_prompt`.

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

const DAY_OF_JUDGMENT: &str = "Destroy all creatures.";
const HEX: &str = "Destroy six target creatures.";
const FINAL_JUDGMENT: &str = "Exile all creatures.";
// Fixture: the return instruction only. The printed token continuation and
// flashback are card-level coverage, not part of this population row.
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

fn effect_resolved_events(outcome: &Outcome) -> usize {
    outcome
        .events()
        .iter()
        .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
        .count()
}

fn zone_changed_for(outcome: &Outcome, ids: &[ObjectId]) -> Vec<ObjectId> {
    outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged { object_id, .. } if ids.contains(object_id) => Some(*object_id),
            _ => None,
        })
        .collect()
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
        .add_spell_to_hand_from_oracle(P0, "Day of Judgment", false, DAY_OF_JUDGMENT)
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
    let mut mass_changes = zone_changed_for(&outcome, &[a, b, c]);
    mass_changes.sort_unstable();
    let mut expected_members = vec![a, b, c];
    expected_members.sort_unstable();
    assert_eq!(
        mass_changes, expected_members,
        "each member's occurrence is emitted exactly once"
    );
    assert_eq!(
        effect_resolved_events(&outcome),
        1,
        "one completion event for the whole population"
    );
    assert!(
        outcome
            .state()
            .resolution_stack
            .iter()
            .all(|frame| !matches!(
                frame,
                engine::types::resolution::ResolutionFrame::BatchDelivery(_)
            )),
        "the staged owner retires once the group is completed and latched"
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
        .add_spell_to_hand_from_oracle(P0, "Day of Judgment", false, DAY_OF_JUDGMENT)
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
    assert_eq!(
        zone_changed_for(&outcome, &[shielded, second]),
        vec![second]
    );
    assert_eq!(
        effect_resolved_events(&outcome),
        1,
        "the population completes once after the pause"
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
    let mut mass_changes = zone_changed_for(&outcome, &[a, b, c]);
    mass_changes.sort_unstable();
    let mut expected_members = vec![a, b, c];
    expected_members.sort_unstable();
    assert_eq!(
        mass_changes, expected_members,
        "each member changes zone exactly once"
    );
    assert_eq!(effect_resolved_events(&outcome), 1);
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
    let wrath = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);

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
    let other = scenario.add_creature(P0, "Shielded Other", 2, 2).id();
    scenario.with_counter(other, CounterType::Shield, 1);
    let damnation = zero_cost_spell(&mut scenario, "Damnation", DAMNATION);
    let mut runner = scenario.build();
    let outcome = runner.cast(damnation).replacement_choice(0).resolve();
    assert_eq!(
        zone_of(&outcome, regenerator),
        Zone::Graveyard,
        "cant_regenerate bypasses only the regeneration shield"
    );
    assert_eq!(
        zone_of(&outcome, other),
        Zone::Battlefield,
        "cant_regenerate does not bypass a shield counter"
    );
    assert_eq!(
        outcome.state().objects[&other]
            .counters
            .get(&CounterType::Shield)
            .copied()
            .unwrap_or_default(),
        0,
        "the shield counter is consumed"
    );
    assert_eq!(destroyed_events(&outcome), 1);
}

/// D2 (F9) Umbra control: a functioning Umbra armor Aura replaces the
/// destruction (CR 702.89a) without being regeneration, so it still applies to
/// a `cant_regenerate` destruction. The host survives, the Aura is destroyed,
/// and the host emits no `CreatureDestroyed`.
#[test]
fn d2_umbra_armor_prevents_destruction_even_with_cant_regenerate() {
    use engine::types::card_type::CoreType;
    use engine::types::game_state::GameState;
    use engine::types::keywords::Keyword;

    fn umbra_board(
        oracle: &str,
        name: &str,
    ) -> (GameState, Vec<GameEvent>, ObjectId, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let host = battlefield_creature(&mut state, 9_821, PlayerId(0), "Umbra Host");
        let plain = battlefield_creature(&mut state, 9_822, PlayerId(0), "Plain Bear");
        let umbra = create_object(
            &mut state,
            CardId(9_823),
            PlayerId(0),
            "Hyena Umbra".to_string(),
            Zone::Battlefield,
        );
        {
            let aura = state.objects.get_mut(&umbra).unwrap();
            aura.card_types.core_types.push(CoreType::Enchantment);
            aura.card_types.subtypes.push("Aura".to_string());
            aura.keywords.push(Keyword::TotemArmor);
            aura.attached_to = Some(host.into());
        }
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .attachments
            .push(umbra);
        let (ability, _source) = direct_spell_ability(&mut state, oracle, name, PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("umbra control destruction resolves");
        (state, events, host, plain, umbra)
    }

    fn destroyed_for(events: &[GameEvent], id: ObjectId) -> usize {
        events
            .iter()
            .filter(|event| match event {
                GameEvent::CreatureDestroyed { object_id, .. } => *object_id == id,
                _ => false,
            })
            .count()
    }

    // Day of Judgment (no cant_regenerate): the Umbra replaces the destruction.
    let (state, events, host, plain, umbra) = umbra_board(DAY_OF_JUDGMENT, "Day of Judgment");
    assert_eq!(
        state.objects[&host].zone,
        Zone::Battlefield,
        "the Umbra armor Aura prevents the host's destruction"
    );
    assert_eq!(
        state.objects[&umbra].zone,
        Zone::Graveyard,
        "the Aura is destroyed instead"
    );
    assert_eq!(state.objects[&plain].zone, Zone::Graveyard);
    assert_eq!(
        destroyed_for(&events, host),
        0,
        "an outer-prevented member emits no CreatureDestroyed"
    );

    // Damnation (`cant_regenerate`): Umbra armor is not regeneration, so it
    // still replaces the destruction.
    let (state, events, host, plain, umbra) = umbra_board(DAMNATION, "Damnation");
    assert_eq!(
        state.objects[&host].zone,
        Zone::Battlefield,
        "cant_regenerate does not bypass Umbra armor"
    );
    assert_eq!(state.objects[&umbra].zone, Zone::Graveyard);
    assert_eq!(state.objects[&plain].zone, Zone::Graveyard);
    assert_eq!(destroyed_for(&events, host), 0);
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
    let day = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);

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
    let day = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);

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

fn graveyard_creature(
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
        Zone::Graveyard,
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
    assert!(
        !outcome
            .state()
            .resolution_stack
            .iter()
            .any(|frame| matches!(
                frame,
                engine::types::resolution::ResolutionFrame::BatchDelivery(_)
            )),
        "a one-member destroy population is never admitted to the staged owner"
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
    // Flicker is two zone changes per permanent (exile, then return), so the
    // exact per-member occurrence count is the invariant, not a single event.
    let changes = zone_changed_for(&outcome, &[a, b]);
    assert_eq!(changes.len(), 4, "each member changes zone exactly twice");
    assert_eq!(changes.iter().filter(|id| **id == a).count(), 2);
    assert_eq!(changes.iter().filter(|id| **id == b).count(), 2);
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
    let day = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);

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

/// C2 (structural counter/profile row, labelled infrastructure): a mass
/// graveyard→battlefield return applies the effect's entry counters and
/// face-down profile to every member exactly once, under the caster, and the
/// pre-entry Devour/ChangeZone snapshot is cleared after the synchronous
/// population.
#[test]
fn c2_structural_mass_entry_applies_counters_and_profile_once() {
    use engine::types::ability::{
        ControllerRef, FaceDownProfile, MassLibraryShuffleMode, QuantityExpr,
    };
    use engine::types::game_state::GameState;
    use engine::types::zones::EtbTapState;

    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    let a = graveyard_creature(&mut state, 9_831, PlayerId(0), "Grave A");
    let b = graveyard_creature(&mut state, 9_832, PlayerId(0), "Grave B");
    let source = create_object(
        &mut state,
        CardId(9_830),
        PlayerId(0),
        "Rise".to_string(),
        Zone::Battlefield,
    );
    let ability = ResolvedAbility::new(
        Effect::ChangeZoneAll {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target: TargetFilter::Any,
            enters_under: Some(ControllerRef::You),
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            enter_with_counters: vec![(CounterType::Plus1Plus1, QuantityExpr::Fixed { value: 1 })],
            face_down_profile: Some(FaceDownProfile::vanilla_2_2()),
            library_position: None,
            library_shuffle: MassLibraryShuffleMode::TerminalShuffle,
            random_order: false,
        },
        vec![],
        source,
        PlayerId(0),
    );
    let mut events = Vec::new();
    resolve_all(&mut state, &ability, &mut events).expect("the mass entry resolves");

    for id in [a, b] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Battlefield,
            "every member enters"
        );
        assert_eq!(
            state.objects[&id].controller,
            PlayerId(0),
            "the effect's controller override applies to every member"
        );
        assert_eq!(
            state.objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default(),
            1,
            "the entry counter applies exactly once"
        );
        assert!(
            state.objects[&id].face_down,
            "the effect's face-down profile applies to every member"
        );
    }
    assert_eq!(state.last_effect_count, Some(2));
    assert!(
        state.active_change_zone_frame().is_none(),
        "the pre-entry Devour/ChangeZone snapshot is cleared once"
    );
}

/// C13a (N6/N11/N12): the mass-only hand-exile counter counts only members
/// whose terminal is `Moved` and whose applied event was Hand→Exile. A mass
/// member that remains (a token outside the battlefield, CR 111.8) does not
/// count; a targeted Hand→Exile twin never increments the mass counter.
#[test]
fn c13a_mass_hand_exile_counts_only_moved_members() {
    use engine::types::ability::MassLibraryShuffleMode;
    use engine::types::game_state::GameState;
    use engine::types::zones::EtbTapState;

    fn hand_card(state: &mut GameState, card: u64, name: &str) -> ObjectId {
        create_object(
            state,
            CardId(card),
            PlayerId(0),
            name.to_string(),
            Zone::Hand,
        )
    }

    fn mass_hand_exile(source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Hand),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                library_shuffle: MassLibraryShuffleMode::TerminalShuffle,
                random_order: false,
            },
            vec![],
            source,
            PlayerId(0),
        )
    }

    // One moved card and one token that cannot change zones.
    let mut state = GameState::new_two_player(42);
    let moved = hand_card(&mut state, 9_851, "Moved Hand Card");
    let stayed = hand_card(&mut state, 9_852, "Token In Hand");
    state.objects.get_mut(&stayed).unwrap().is_token = true;
    let source = create_object(
        &mut state,
        CardId(9_850),
        PlayerId(0),
        "Exile effect".to_string(),
        Zone::Battlefield,
    );
    state.exiled_from_hand_this_resolution = 0;
    let mut events = Vec::new();
    resolve_all(&mut state, &mass_hand_exile(source), &mut events)
        .expect("the mass hand exile resolves");
    assert_eq!(state.objects[&moved].zone, Zone::Exile);
    assert_eq!(
        state.objects[&stayed].zone,
        Zone::Hand,
        "the token outside the battlefield stays put (Remained)"
    );
    assert_eq!(
        state.exiled_from_hand_this_resolution, 1,
        "only the Moved Hand→Exile member increments the counter"
    );
    assert_eq!(
        state.last_effect_count,
        Some(2),
        "the mass policy counts every non-abandoned member"
    );

    // Both-moved control: two real hand cards count 2.
    let mut state = GameState::new_two_player(42);
    let a = hand_card(&mut state, 9_853, "Hand A");
    let b = hand_card(&mut state, 9_854, "Hand B");
    let source = create_object(
        &mut state,
        CardId(9_855),
        PlayerId(0),
        "Exile effect".to_string(),
        Zone::Battlefield,
    );
    state.exiled_from_hand_this_resolution = 0;
    let mut events = Vec::new();
    resolve_all(&mut state, &mass_hand_exile(source), &mut events)
        .expect("the both-moved mass hand exile resolves");
    assert_eq!(state.objects[&a].zone, Zone::Exile);
    assert_eq!(state.objects[&b].zone, Zone::Exile);
    assert_eq!(
        state.exiled_from_hand_this_resolution, 2,
        "the both-moved control counts both"
    );

    // Targeted twin: the mass-only gate keeps `count_hand_exile` false, so the
    // targeted Hand→Exile population never increments the counter.
    let mut state = GameState::new_two_player(42);
    let c = hand_card(&mut state, 9_856, "Targeted A");
    let d = hand_card(&mut state, 9_857, "Targeted B");
    let source = create_object(
        &mut state,
        CardId(9_858),
        PlayerId(0),
        "Targeted exile".to_string(),
        Zone::Battlefield,
    );
    state.exiled_from_hand_this_resolution = 0;
    let ability = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Hand),
            destination: Zone::Exile,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![TargetRef::Object(c), TargetRef::Object(d)],
        source,
        PlayerId(0),
    );
    let mut events = Vec::new();
    engine::game::effects::change_zone::resolve(&mut state, &ability, &mut events)
        .expect("the targeted hand exile resolves");
    assert_eq!(state.objects[&c].zone, Zone::Exile);
    assert_eq!(state.objects[&d].zone, Zone::Exile);
    assert_eq!(
        state.exiled_from_hand_this_resolution, 0,
        "a targeted Hand→Exile population is not the mass counter's producer"
    );
}

/// C10 (targeted paused pins): the paused targeted population settles with the
/// admitted count, the arrival this-way stamp, and the exact moved ledger in
/// announcement order (the defect was an empty ledger from the battlefield-only
/// occurrence projection).
#[test]
fn c10_paused_targeted_population_pins_count_stamp_and_moved_ledger() {
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

    assert_eq!(outcome.state().last_effect_count, Some(3));
    assert_eq!(
        outcome.state().last_zone_changed_ids,
        vec![first, second, third],
        "the moved ledger is the actual moved members in announcement order"
    );
    assert_eq!(effect_resolved_events(&outcome), 1);
}

/// C10 (mass paused pins): a paused mass population uses the non-abandoned
/// count (not the arrival subset), stamps arrivals, and writes the full moved
/// ledger.
#[test]
fn c10_paused_mass_population_pins_non_abandoned_count_and_ledger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let a = scenario
        .add_creature_to_graveyard(P0, "Grave Bear", 2, 2)
        .id();
    let b = scenario
        .add_creature_to_graveyard(P0, "Grave Elk", 2, 2)
        .id();
    let c = scenario
        .add_creature_to_graveyard(P0, "Grave Wolf", 2, 2)
        .id();
    let pauser = {
        let mut builder = scenario.add_creature_to_graveyard(P0, "Grave Pauser", 2, 2);
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
    let rise = zero_cost_spell(
        &mut scenario,
        "Rise of the Dark Realms",
        RISE_OF_THE_DARK_REALMS,
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(rise).replacement_choice(0).resolve();

    assert_eq!(
        outcome.state().last_effect_count,
        Some(4),
        "the paused mass policy counts every non-abandoned member"
    );
    let mut ledger = outcome.state().last_zone_changed_ids.clone();
    ledger.sort_unstable();
    let mut expected = vec![a, b, c, pauser];
    expected.sort_unstable();
    assert_eq!(
        ledger, expected,
        "the paused mass ledger holds every moved member"
    );
    assert_eq!(effect_resolved_events(&outcome), 1);
}

/// C12 (F6): a `forward_result` ChangeZone over two members pauses on the first
/// member's entry replacement. The marker records the staged group; after
/// settlement the chained referent is bound to the exact battlefield arrivals.
/// Exact reproduction fixture from the rows executor (see r4c-c12-fixture.md).
#[test]
fn c12_forwarded_zone_result_binds_staged_arrivals_after_pause() {
    use engine::game::engine::apply_as_current;
    use engine::types::ability::{EffectKind, QuantityExpr};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::zones::EtbTapState;

    fn graveyard_creature(
        state: &mut GameState,
        card_id: u64,
        owner: PlayerId,
        name: &str,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            owner,
            name.to_string(),
            Zone::Graveyard,
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
    fn install_replacement(state: &mut GameState, id: ObjectId, def: ReplacementDefinition) {
        let object = state.objects.get_mut(&id).unwrap();
        object.replacement_definitions.push(def.clone());
        std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(def);
    }

    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    let a = graveyard_creature(&mut state, 9_841, PlayerId(0), "Return A");
    let b = graveyard_creature(&mut state, 9_842, PlayerId(0), "Return B");
    install_replacement(
        &mut state,
        a,
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .mode(ReplacementMode::Optional { decline: None })
            .valid_card(TargetFilter::SelfRef)
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
    let source = create_object(
        &mut state,
        CardId(9_840),
        PlayerId(0),
        "Return spell".to_string(),
        Zone::Battlefield,
    );
    let mut ability = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![TargetRef::Object(a), TargetRef::Object(b)],
        source,
        PlayerId(0),
    );
    ability.forward_result = true;
    ability.sub_ability = Some(Box::new(ResolvedAbility::new(
        Effect::PutCounter {
            target: TargetFilter::ParentTarget,
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
        },
        vec![],
        source,
        PlayerId(0),
    )));

    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("the forwarded move pauses");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let group_id = state
        .active_batch_delivery()
        .expect("the staged owner is parked")
        .logical_zone_change_group
        .logical_group_id;
    let marker = state
        .ability_continuation_at_top_or_parent()
        .expect("the dependent continuation is parked beside the staged batch")
        .pending
        .chain
        .context
        .pending_forwarded_zone_result
        .clone()
        .expect("F6 records the pending forwarded result");
    assert_eq!(
        marker.group,
        Some(group_id),
        "the marker records the staged group"
    );
    assert_eq!(
        marker.producer, source,
        "the marker is bound to the resolving producer"
    );

    let mut resumed = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
        .expect("the entry replacement resolves");
    events.append(&mut resumed.events);

    for id in [a, b] {
        assert_eq!(state.objects[&id].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default(),
            1,
            "the chained referent sees the exact battlefield arrivals"
        );
    }
    assert!(
        state
            .ability_continuation_at_top_or_parent()
            .is_none_or(|frame| frame
                .pending
                .chain
                .context
                .pending_forwarded_zone_result
                .is_none()),
        "settlement clears the pending marker"
    );
    // The forwarded population emits exactly one `ChangeZone` resolution at
    // settlement. The chained `PutCounter` node then emits its own, separate
    // `EffectResolved`; counting all kinds double-counts the chain, which is
    // why this row scopes to the population's own kind.
    let change_zone_index = events.iter().position(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ChangeZone,
                source_id,
                ..
            } if *source_id == source
        )
    });
    assert!(
        change_zone_index.is_some(),
        "the forwarded population emits its ChangeZone resolution"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ChangeZone,
                    ..
                }
            ))
            .count(),
        1,
        "one ChangeZone resolution event for the whole forwarded population"
    );
    let counter_index = events.iter().position(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::PutCounter,
                ..
            }
        )
    });
    assert!(
        counter_index.is_some_and(|index| index > change_zone_index.unwrap()),
        "the chained counter node resolves after the population's single event"
    );
}
