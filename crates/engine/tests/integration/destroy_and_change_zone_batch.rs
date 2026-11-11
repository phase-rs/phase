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
use engine::game::engine::apply_as_current;
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, Outcome, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode,
    ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::ability::{
    ControllerRef, EffectKind, FaceDownProfile, LibraryShuffleMode, QuantityExpr,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    GameState, PersistedGameState, PersistedRestoreFinalization, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::{AppliedReplacementKey, ProposedEvent};
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::EtbTapState;
use engine::types::zones::Zone;

const DAY_OF_JUDGMENT: &str = "Destroy all creatures.";
const HEX: &str = "Destroy six target creatures.";
const FINAL_JUDGMENT: &str = "Exile all creatures.";
// Full printed Oracle, including the alternate casting mode.
const VENGEFUL_REGROWTH: &str = "Return up to three target land cards from your graveyard to the battlefield tapped. Create that many 4/2 green Plant Warrior creature tokens with reach.\nFlashback {6}{G}{G} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
// Infrastructure exception: Vengeful Regrowth is the printed analogue. C10
// isolates its return instruction to observe the LAST instruction's zone-change
// ledger; the full printed token consumer is exercised by C5 below.
const VENGEFUL_REGROWTH_RETURN_ONLY: &str =
    "Return up to three target land cards from your graveyard to the battlefield tapped.";

// Infrastructure exception: Drudge Skeletons is the printed analogue. Install
// the shield its regeneration ability would create, without activating it.
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
    scenario.add_artifact_from_oracle(
        P0,
        "Grafdigger's Cage",
        "Creature cards in graveyards and libraries can't enter the battlefield.\nPlayers can't cast spells from graveyards or libraries.",
    );
    let a = scenario.add_land_to_graveyard(P0, "Forest").id();
    let b = scenario.add_land_to_graveyard(P0, "Island").id();
    let c = scenario
        .add_creature_to_graveyard(P0, "Dryad Arbor", 1, 1)
        .as_land()
        .as_creature()
        .with_subtypes(vec!["Forest", "Dryad"])
        .from_oracle_text("(This land isn't a spell, it's affected by summoning sickness, and it has \"{T}: Add {G}.\")")
        .id();
    let regrowth = scenario
        .add_spell_to_hand_from_oracle(P0, "Vengeful Regrowth", false, VENGEFUL_REGROWTH)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(regrowth).target_objects(&[a, b, c]).resolve();

    for id in [a, b] {
        assert_eq!(
            zone_of(&outcome, id),
            Zone::Battlefield,
            "each targeted card arrives on the battlefield"
        );
        assert!(
            outcome.state().objects[&id].tapped,
            "Regrowth returns the land tapped"
        );
    }
    assert_eq!(zone_of(&outcome, c), Zone::Graveyard);
    assert!(
        zone_changed_for(&outcome, &[c]).is_empty(),
        "the Cage-blocked attempted return must not emit an arrival"
    );
    assert_eq!(
        outcome.state().last_effect_count,
        Some(2),
        "the targeted policy counts actual arrivals"
    );

    // CR 608.2c: only the two actual arrivals feed the printed token consumer;
    // Grafdigger's Cage prevents the land creature from entering.
    let tokens: Vec<ObjectId> = outcome
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            outcome.state().objects[id]
                .card_types
                .core_types
                .contains(&CoreType::Creature)
        })
        .collect();
    assert_eq!(
        tokens.len(),
        2,
        "\"Create that many ...\" creates one token per returned land"
    );
    for token in tokens {
        let object = &outcome.state().objects[&token];
        assert!(
            object.is_token,
            "the printed continuation creates real tokens"
        );
        // CR 111.2: the player creating the token owns and controls it.
        assert_eq!(object.owner, P0);
        assert_eq!(object.controller, P0);
        assert_eq!(
            object.color.as_slice(),
            &[engine::types::mana::ManaColor::Green]
        );
        assert_eq!(
            (object.power, object.toughness),
            (Some(4), Some(2)),
            "the tokens are 4/2"
        );
        for subtype in ["Plant", "Warrior"] {
            assert!(
                object
                    .card_types
                    .subtypes
                    .iter()
                    .any(|value| value == subtype),
                "the token is a {subtype}, got {:?}",
                object.card_types.subtypes
            );
        }
        assert!(
            object.keywords.contains(&Keyword::Reach),
            "the token has reach, got {:?}",
            object.keywords
        );
    }
    assert!(matches!(
        outcome.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    assert!(outcome.state().resolution_stack.is_empty());
}

/// Oracle texts used by the rows below (raw, as a player would read them).
const DAMNATION: &str = "Destroy all creatures. They can't be regenerated.";
const FUMIGATE: &str =
    "Destroy all creatures. You gain 1 life for each creature destroyed this way.";
// Infrastructure exception: only Rest in Peace's replacement line is needed;
// the preinstalled permanent never enters here, so its graveyard-exile trigger
// is intentionally omitted.
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
    fn umbra_board(
        oracle: &str,
        name: &str,
    ) -> (GameState, Vec<GameEvent>, ObjectId, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let host = battlefield_creature(&mut state, 9_821, PlayerId(0), "Umbra Host");
        let plain = battlefield_creature(&mut state, 9_822, PlayerId(0), "Plain Bear");
        // Infrastructure exception: Hyena Umbra is the printed analogue. This
        // constructed Aura isolates umbra armor; its P/T and first strike grant
        // are irrelevant to the destruction boundary.
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
        // Infrastructure exception: no printed card performs this exact
        // Battlefield->Graveyard -> Battlefield substitution. Rest in Peace
        // is the nearest printed analogue: it redirects a graveyard-bound move
        // to exile instead; this synthetic destination exercises Remained.
        builder.with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_card(TargetFilter::SelfRef)
                .description("redirect back".to_string())
                .destination_zone(Zone::Graveyard)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        library_shuffle: Default::default(),
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
    let their = battlefield_creature(&mut state, 9_803, PlayerId(1), "Their Bear");
    // A controller-owned indestructible creature: it dies for nobody's count.
    let control = battlefield_creature(&mut state, 9_804, PlayerId(0), "Adamant Titan");
    state
        .objects
        .get_mut(&control)
        .unwrap()
        .keywords
        .push(Keyword::Indestructible);
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
        state.objects[&their].zone,
        Zone::Graveyard,
        "the opponent's creature really died, so the count is discriminated"
    );
    assert_eq!(
        state.objects[&control].zone,
        Zone::Battlefield,
        "the indestructible control is a Remained member"
    );
    assert_eq!(
        state.players[0].life,
        life_before + 2,
        "only the caster's own DESTROYED creatures add life"
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

    // LIVE discriminator: the same singleton destroy parked on its own
    // replacement choice must still own no staged BatchDelivery. Asserting
    // only after completion cannot see a carrier that settled and popped.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = {
        let mut builder = scenario.add_creature(P0, "Pausing Bear", 2, 2);
        let id = builder.id();
        // Infrastructure exception: Drudge Skeletons' regeneration shield is
        // the nearest printed destroy replacement. This optional NoOp only
        // opens the singleton replacement boundary; it models no printed card.
        builder.with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Destroy)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_card(TargetFilter::SelfRef)
                .description("pause the destroy".to_string())
                .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp)),
        );
        id
    };
    let murder = zero_cost_spell(&mut scenario, "Murder", MURDER);
    let mut runner = scenario.build();
    let pending = runner
        .cast(murder)
        .target_object(victim)
        .commit()
        .try_resolve()
        .expect("a singleton destroy pause is not an error");
    assert!(
        matches!(
            pending.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "premise: the singleton destroy is parked on its own replacement, got {:?}",
        pending.final_waiting_for()
    );
    assert!(
        !pending
            .state()
            .resolution_stack
            .iter()
            .any(|frame| matches!(
                frame,
                engine::types::resolution::ResolutionFrame::BatchDelivery(_)
            )),
        "a one-member destroy population parks no staged owner while it is live"
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
        // Infrastructure exception: Llanowar Reborn is the nearest printed
        // counter-on-entry land. This optional counter write isolates a pause
        // and an observable effect that Regrowth's printed tapped entry cannot produce.
        builder.with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                // CR 113.6h + CR 614.12: only the entrant's own replacement
                // functions while this source is still in the graveyard.
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield)
                .description("enters with a +1/+1 counter".to_string())
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PutCounter {
                        target: TargetFilter::SelfRef,
                        counter_type: CounterType::Plus1Plus1,
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                )),
        );
        id
    };
    let second = scenario.add_land_to_graveyard(P0, "Island").id();
    let third = scenario.add_land_to_graveyard(P0, "Mountain").id();
    let regrowth = zero_cost_spell(&mut scenario, "Vengeful Regrowth", VENGEFUL_REGROWTH);
    let mut runner = scenario.build();
    let pending = runner
        .cast(regrowth)
        .target_objects(&[first, second, third])
        .commit()
        .try_resolve()
        .expect("Regrowth reaches its first entry replacement");
    assert!(matches!(
        pending.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    assert!(pending.state().active_batch_delivery().is_some());
    for id in [first, second, third] {
        assert_eq!(pending.state().objects[&id].zone, Zone::Graveyard);
    }
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept the observed counter-on-entry replacement");
    let state = runner.state();

    for id in [first, second, third] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Battlefield,
            "no returned member may be stranded by the first pause"
        );
    }
    assert_eq!(
        state.objects[&first].counters.get(&CounterType::Plus1Plus1),
        Some(&1),
        "the accepted replacement, not Regrowth's tapped entry, added the counter"
    );
    assert!(
        state.objects[&first].tapped,
        "Regrowth still returns it tapped"
    );
    assert_eq!(
        state.last_effect_count,
        Some(3),
        "the admitted targeted population count survives the pause"
    );
    let tokens: Vec<_> = state
        .battlefield
        .iter()
        .map(|id| &state.objects[id])
        .filter(|object| object.is_token)
        .collect();
    assert_eq!(
        tokens.len(),
        3,
        "the printed that-many tail creates three tokens"
    );
    for token in tokens {
        assert_eq!(token.controller, PlayerId(0));
        assert_eq!((token.power, token.toughness), (Some(4), Some(2)));
        assert!(token.keywords.contains(&Keyword::Reach));
    }
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

/// D8 (staged Destroy wire/save): a mass destruction paused at the staged
/// replacement boundary round-trips through the persisted wire and resumes to
/// the same final terminals. The pause carries the staged owner (operation,
/// group, cursor), not a legacy singleton boundary.
#[test]
fn d8_staged_destroy_pause_round_trips_and_resumes() {
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
    assert!(matches!(restored.waiting_for, WaitingFor::Priority { .. }));
    assert!(restored.stack.is_empty());
    assert!(restored.resolution_stack.is_empty());
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
    assert_eq!(effect_resolved_events(&outcome), 1);
    assert_eq!(zone_of(&outcome, judgment), Zone::Graveyard);
    assert!(matches!(
        outcome.state().waiting_for,
        engine::types::game_state::WaitingFor::Priority { .. }
    ));
}

/// C11 (random-order library, F19/N9): a mass bottom placement with
/// `random_order` writes the requested placement, produces no arrangement
/// prompt, and leaves the sentinels above the moved cards.
/// Infrastructure exception: Junktroller is the nearest printed
/// graveyard-to-library-bottom move. This constructed mass/random-order
/// combination isolates placement; it is not Junktroller's full ability.
#[test]
fn c11_random_order_bottom_placement_has_no_prompt_and_exact_region() {
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
            library_shuffle: LibraryShuffleMode::TerminalShuffle,
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
            library_shuffle: LibraryShuffleMode::TerminalShuffle,
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
/// Infrastructure exception: Memory Jar is the nearest printed mass hand-exile
/// instruction. These direct resolver fixtures isolate its moved-member ledger,
/// omitting face-down delivery, drawing and the delayed return.
#[test]
fn c13a_mass_hand_exile_counts_only_moved_members() {
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
                library_shuffle: LibraryShuffleMode::TerminalShuffle,
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
            library_shuffle: Default::default(),
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
/// admitted count and the exact moved ledger in announcement order (the defect
/// was an empty ledger from the battlefield-only occurrence projection).
/// Infrastructure exception: Vengeful Regrowth is the printed analogue; this
/// return-only fixture isolates the ledger before its printed token consumer.
#[test]
fn c10_paused_targeted_population_pins_count_and_moved_ledger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = {
        let mut builder = scenario.add_land_to_graveyard(P0, "Forest");
        let id = builder.id();
        // Infrastructure exception: Temple Garden is the nearest printed
        // as-enters choice; this optional tap write isolates the pause.
        builder.with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                // CR 113.6h + CR 614.12: the graveyard source modifies only its own entry.
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
        id
    };
    let second = scenario.add_land_to_graveyard(P0, "Island").id();
    let third = scenario.add_land_to_graveyard(P0, "Mountain").id();
    let regrowth = zero_cost_spell(
        &mut scenario,
        "Vengeful Regrowth",
        VENGEFUL_REGROWTH_RETURN_ONLY,
    );
    let mut runner = scenario.build();
    let pending = runner
        .cast(regrowth)
        .target_objects(&[first, second, third])
        .commit()
        .try_resolve()
        .expect("the return-only population reaches its entry replacement");
    assert!(matches!(
        pending.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    assert!(pending.state().active_batch_delivery().is_some());
    let completion = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answer the observed targeted entry replacement");
    let state = runner.state();

    assert_eq!(state.last_effect_count, Some(3));
    assert_eq!(
        state.last_zone_changed_ids,
        vec![first, second, third],
        "the moved ledger is the actual moved members in announcement order"
    );
    assert_eq!(
        pending
            .events()
            .iter()
            .chain(&completion.events)
            .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
            .count(),
        1
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

/// C10 (mass paused pins): a paused mass population uses the non-abandoned
/// count (not the arrival subset) and writes the full moved ledger.
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
        // Infrastructure exception: Temple Garden is the nearest printed
        // as-enters choice; this optional tap write isolates the pause.
        builder.with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                // CR 113.6h + CR 614.12: the graveyard source modifies only its own entry.
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
        id
    };
    let rise = zero_cost_spell(
        &mut scenario,
        "Rise of the Dark Realms",
        RISE_OF_THE_DARK_REALMS,
    );
    let mut runner = scenario.build();
    let pending = runner
        .cast(rise)
        .commit()
        .try_resolve()
        .expect("the mass return population reaches its entry replacement");
    assert!(matches!(
        pending.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P0, .. }
    ));
    assert!(pending.state().active_batch_delivery().is_some());
    let completion = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answer the observed mass entry replacement");
    let state = runner.state();

    assert_eq!(
        state.last_effect_count,
        Some(4),
        "the paused mass policy counts every non-abandoned member"
    );
    let mut ledger = state.last_zone_changed_ids.clone();
    ledger.sort_unstable();
    let mut expected = vec![a, b, c, pauser];
    expected.sort_unstable();
    assert_eq!(
        ledger, expected,
        "the paused mass ledger holds every moved member"
    );
    assert_eq!(
        pending
            .events()
            .iter()
            .chain(&completion.events)
            .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
            .count(),
        1
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
}

/// C12 (F6): a `forward_result` ChangeZone over two members pauses on the first
/// member's entry replacement. The marker records the staged group; after
/// settlement the chained referent is bound to the exact battlefield arrivals.
/// Infrastructure exception: Necrotic Plague is the nearest printed
/// `forward_result` producer, lowered by
/// `assembly.rs::stamp_forward_result_on_battlefield_attach_return`.
/// It returns one Aura attached to the chosen creature; no printed card in the
/// reviewed corpus forwards this two-member population. This row constructs a
/// `ResolvedAbility` and drives `resolve_ability_chain` to isolate the real
/// `settle_forwarded_zone_result` consumer and marker retirement, not to claim
/// a public full-card witness. Public ChangeZone completion producers are
/// covered by C1, C5 and C10.
#[test]
fn c12_forwarded_zone_result_binds_staged_arrivals_after_pause() {
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
            // Infrastructure exception: Temple Garden is the nearest printed
            // as-enters choice; this optional tap write isolates the pause.
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
            library_shuffle: Default::default(),
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

/// CR 401.4 + CR 730.3a: the owner chooses both the numbered
/// position and the private order of the departing merged permanent's cards.
#[test]
fn deem_inferior_mutated_owner_orders_components_second_from_top() {
    const DEEM_INFERIOR: &str = "This spell costs {1} less to cast for each card you've drawn this turn.\nThe owner of target nonland permanent puts it into their library second from the top or on the bottom.";
    for reverse in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P1, &["L0", "L1", "L2"]);
        let host = scenario
            .add_creature(P1, "Host", 2, 2)
            .controlled_by(P0)
            .id();
        let rider = scenario.add_creature_to_hand(P1, "Rider", 3, 3).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Deem Inferior", false, DEEM_INFERIOR)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        // Infrastructure exception: this direct merge stands in for Gemrazer's
        // public Mutate cast; this row isolates the departing components' order.
        engine::game::merge::merge_object_onto(
            runner.state_mut(),
            rider,
            host,
            engine::game::merge::MergeSide::Bottom,
            &mut Vec::new(),
        );
        let sentinels: Vec<_> = runner.state().library_of(P1).iter().copied().collect();
        runner.cast(spell).target_objects(&[host]).commit();
        for _ in 0..16 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::TopOrBottomChoice { .. }
            ) {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            runner
                .act(GameAction::PassPriority)
                .expect("resolve Deem Inferior");
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::TopOrBottomChoice {
                player: P1,
                object_id,
                top_position: engine::types::ability::LibraryPosition::NthFromTop { n: 2 },
            } if object_id == host
        ));
        runner
            .act(GameAction::ChooseTopOrBottom { top: true })
            .expect("P1 chooses second from top");
        match &runner.state().waiting_for {
            WaitingFor::DigRestSplitChoice {
                player,
                library_owner,
                cards,
                top_count,
                scope,
                ..
            } => {
                assert_eq!(
                    (*player, *library_owner),
                    (P1, P1),
                    "P0 never arranges P1's cards"
                );
                assert_eq!(*scope, engine::types::ability::DigRestSplitScope::OrderOnly);
                assert_eq!(*top_count, 2, "the UI presents the numbered block top-down");
                let mut offered = cards.clone();
                offered.sort_unstable();
                let mut expected = vec![host, rider];
                expected.sort_unstable();
                assert_eq!(offered, expected);
            }
            other => panic!("expected P1's component order, got {other:?}"),
        }
        let order = if reverse {
            [rider, host]
        } else {
            [host, rider]
        };
        runner
            .act(GameAction::SelectCards {
                cards: order.to_vec(),
            })
            .expect("P1 submits the component permutation");
        assert_eq!(
            runner
                .state()
                .library_of(P1)
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [sentinels[0], order[0], order[1], sentinels[1], sentinels[2]]
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
    }
}

/// CR 401.7: a nonmerged tuck has no ordering prompt and uses the exact
/// numbered slot, including the short-library clamp.
#[test]
fn tricksters_stratagem_nonmerged_exact_nth_slot() {
    const TRICKSTERS_STRATAGEM: &str = "The owner of target creature an opponent controls puts it into their library second from the top or on the bottom. Then up to one target creature you control connives. (Draw a card, then discard a card. If you discarded a nonland card, put a +1/+1 counter on that creature.)";
    for fillers in [&["L0", "L1"][..], &["L0"][..]] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P1, fillers);
        scenario.with_library_top(P0, &["Caster Filler"]);
        let victim = scenario.add_creature(P1, "Opposing Creature", 2, 2).id();
        let conniver = scenario.add_creature(P0, "Own Creature", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Trickster's Stratagem", false, TRICKSTERS_STRATAGEM)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        let sentinels: Vec<_> = runner.state().library_of(P1).iter().copied().collect();
        // FU-3: declining this optional slot has a separate existing defect.
        // Selecting P0's legal creature keeps its connive out of P1's library.
        runner
            .cast(spell)
            .target_objects(&[victim, conniver])
            .commit();
        for _ in 0..16 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::TopOrBottomChoice { .. }
            ) {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            runner
                .act(GameAction::PassPriority)
                .expect("resolve Trickster's Stratagem");
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::TopOrBottomChoice { player: P1, .. }
        ));
        let result = runner
            .act(GameAction::ChooseTopOrBottom { top: true })
            .expect("P1 chooses second from top");
        let mut expected = sentinels.clone();
        expected.insert(1, victim);
        assert_eq!(
            runner
                .state()
                .library_of(P1)
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            expected,
            "L0 is never drawn by P0's connive"
        );
        assert!(
            result.events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved { kind: EffectKind::Connive, source_id, .. }
                    if *source_id == conniver
            )),
            "the printed continuation connived P0's creature"
        );
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "no arrangement or connive choice remains: {:?}",
            runner.state().waiting_for
        );
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
    }
}

fn destroy_with_surviving_owner_and_opposing_chooser(
    name: &str,
    oracle: &str,
    spell_type: CoreType,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(p2, "Grizzly Bears", 2, 2).id();
    scenario.with_counter(host, CounterType::Shield, 1);
    let sibling = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let control = scenario
        .add_enchantment_from_oracle(
            P1,
            "Mind Control",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let umbra = scenario
        .add_enchantment_from_oracle(
            p2,
            "Hyena Umbra",
            "Enchant creature\nEnchanted creature gets +1/+1 and has first strike.\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, spell_type == CoreType::Instant, oracle)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    // Infrastructure exception: preattach the full-Oracle Auras in place of
    // casting them, isolating the public destruction/concession handoff.
    engine::game::effects::attach::attach_to(runner.state_mut(), control, host);
    engine::game::effects::attach::attach_to(runner.state_mut(), umbra, host);
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&host].controller, P1);
    (runner, host, sibling, spell, umbra)
}

/// CR 800.4a + CR 616.1: after Mind Control leaves, the surviving controller
/// must answer the interrupted Destroy consultation. CR 122.1c: choosing the
/// shield spends it; merely skipping the member would leave both protections.
#[test]
fn staged_destroy_reasks_the_surviving_controller_and_resolves_the_rest() {
    let p2 = PlayerId(2);
    let (mut runner, host, sibling, wrath, umbra) =
        destroy_with_surviving_owner_and_opposing_chooser(
            "Day of Judgment",
            DAY_OF_JUDGMENT,
            CoreType::Sorcery,
        );
    let pending = runner.cast(wrath).commit().try_resolve().unwrap();
    assert!(matches!(
        pending.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    assert!(runner.state().active_batch_delivery().is_some());
    runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    let wire = serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
        .expect("the surviving controller's unanswered consultation serializes");
    let restored = serde_json::from_value::<PersistedGameState>(wire)
        .expect("the reasked consultation decodes")
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("the reasked consultation prepares")
        .finalize_immediately()
        .expect("the reasked consultation finalizes");
    for mut runner in [runner, GameRunner::from_state(restored)] {
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "the surviving controller must choose: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, p2);
        assert_eq!(candidates.len(), 2);
        assert!(candidates
            .iter()
            .any(|candidate| candidate.source_id == umbra));
        let shield = candidates
            .iter()
            .position(|candidate| candidate.source_id == host)
            .expect("the host's shield is offered separately from the surviving Aura");
        assert_eq!(runner.state().objects[&host].controller, p2);
        assert_eq!(runner.state().objects[&sibling].zone, Zone::Battlefield);
        assert!(runner.state().pending_replacement.is_some());
        runner
            .state()
            .resolution_stack
            .validate_at_rest(&runner.state().waiting_for)
            .expect("the new controller's unanswered choice is a valid boundary");
        let result = runner
            .act(GameAction::ChooseReplacement { index: shield })
            .expect("the surviving controller chooses its shield");
        let state = runner.state();
        assert_eq!(state.objects[&host].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&host]
                .counters
                .get(&CounterType::Shield)
                .copied()
                .unwrap_or(0),
            0,
            "the actual replacement consumes the shield rather than skipping destruction"
        );
        assert_eq!(state.objects[&umbra].zone, Zone::Battlefield);
        assert_eq!(state.objects[&sibling].zone, Zone::Graveyard);
        assert_eq!(state.objects[&wrath].zone, Zone::Graveyard);
        assert_eq!(
            result
                .events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::CreatureDestroyed { object_id, .. } if *object_id == sibling
                ))
                .count(),
            1
        );
        assert!(!result.events.iter().any(|event| matches!(
            event,
            GameEvent::CreatureDestroyed { object_id, .. } if *object_id == host
        )));
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.pending_replacement.is_none());
        assert!(state.stack.is_empty());
        assert!(state.resolution_stack.is_empty());
        state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .expect("answering the replacement completes the whole population");
    }
}

/// CR 616.1: the same population must retain its live controller's choice,
/// not skip the protected member before the chooser answers.
#[test]
fn staged_destroy_keeps_the_choice_when_its_chooser_does_not_concede() {
    let (mut runner, host, sibling, wrath, umbra) =
        destroy_with_surviving_owner_and_opposing_chooser(
            "Day of Judgment",
            DAY_OF_JUDGMENT,
            CoreType::Sorcery,
        );
    let pending = runner.cast(wrath).commit().try_resolve().unwrap();
    assert!(matches!(
        pending.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    assert!(runner.state().pending_replacement.is_some());
    assert_eq!(runner.state().objects[&host].controller, P1);
    assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&sibling].zone, Zone::Battlefield);
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        unreachable!("the required choice was asserted above");
    };
    let armor = candidates
        .iter()
        .position(|candidate| candidate.source_id == umbra)
        .expect("the living controller is offered its host's Hyena Umbra");
    runner
        .act(GameAction::ChooseReplacement { index: armor })
        .expect("the living controller chooses the printed umbra armor replacement");
    assert_eq!(runner.state().objects[&host].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&umbra].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&host]
            .counters
            .get(&CounterType::Shield),
        Some(&1)
    );
    assert_eq!(runner.state().objects[&sibling].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&wrath].zone, Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    runner
        .state()
        .resolution_stack
        .validate_at_rest(&runner.state().waiting_for)
        .expect("the living chooser completes at a valid public boundary");
}

/// CR 800.4a + CR 616.1: the same reconsultation belongs to an ordinary
/// singleton Destroy, not only to the staged-population cursor.
/// CR 702.89a: choosing armor destroys that Aura and leaves the shield intact.
#[test]
fn singleton_destroy_reasks_the_surviving_controller_and_applies_armor() {
    let p2 = PlayerId(2);
    let (mut runner, host, sibling, murder, umbra) =
        destroy_with_surviving_owner_and_opposing_chooser("Murder", MURDER, CoreType::Instant);
    let pending = runner
        .cast(murder)
        .target_object(host)
        .commit()
        .try_resolve()
        .unwrap();
    assert!(matches!(
        pending.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the singleton event must be reasked: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, p2);
    assert_eq!(candidates.len(), 2);
    let armor = candidates
        .iter()
        .position(|candidate| candidate.source_id == umbra)
        .expect("the surviving controller can choose the printed armor replacement");
    runner
        .act(GameAction::ChooseReplacement { index: armor })
        .unwrap();
    let state = runner.state();
    assert_eq!(state.objects[&host].controller, p2);
    assert_eq!(state.objects[&host].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&host].counters.get(&CounterType::Shield),
        Some(&1)
    );
    assert_eq!(state.objects[&umbra].zone, Zone::Graveyard);
    assert_eq!(state.objects[&sibling].zone, Zone::Battlefield);
    assert_eq!(state.objects[&murder].zone, Zone::Graveyard);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.pending_replacement.is_none());
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
    state
        .resolution_stack
        .validate_at_rest(&state.waiting_for)
        .expect("the singleton reconsultation completes at a valid boundary");
}

/// CR 800.4a + CR 616.1 + CR 702.89a: removed sources are not stale candidates.
/// With zero surviving Auras, destroy the host; with one, apply its mandatory
/// armor without inventing a new ordering choice.
#[test]
fn staged_destroy_refreshes_zero_or_one_surviving_umbra_after_chooser_departure() {
    let p2 = PlayerId(2);
    for second_owner in [P1, p2] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(p2, "Grizzly Bears", 2, 2).id();
        let sibling = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let control = scenario
            .add_enchantment_from_oracle(
                P1,
                "Mind Control",
                "Enchant creature\nYou control enchanted creature.",
            )
            .with_subtypes(vec!["Aura"])
            .id();
        let umbras: Vec<_> = [P1, second_owner].into_iter().map(|owner|
            scenario.add_enchantment_from_oracle(
                owner,
                "Hyena Umbra",
                "Enchant creature\nEnchanted creature gets +1/+1 and has first strike.\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)",
            ).with_subtypes(vec!["Aura"]).id()
        ).collect();
        let wrath = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);
        let mut runner = scenario.build();
        // Infrastructure exception: preattach the full-Oracle Auras instead
        // of casting them; the pending consultation is produced by the public cast.
        for aura in [control, umbras[0], umbras[1]] {
            engine::game::effects::attach::attach_to(runner.state_mut(), aura, host);
        }
        engine::game::layers::flush_layers(runner.state_mut());
        assert_eq!(runner.state().objects[&host].controller, P1);
        let pending = runner.cast(wrath).commit().try_resolve().unwrap();
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = pending.final_waiting_for()
        else {
            panic!(
                "two printed Auras must offer ordering: {:?}",
                pending.final_waiting_for()
            );
        };
        assert_eq!(*player, P1);
        assert_eq!(candidates.len(), 2);
        for aura in &umbras {
            assert!(candidates
                .iter()
                .any(|candidate| candidate.source_id == *aura));
        }
        let result = runner.act(GameAction::Concede { player_id: P1 }).unwrap();
        let state = runner.state();
        assert_eq!(state.objects[&host].controller, p2);
        assert!(!state.battlefield.contains(&control));
        assert!(!state.battlefield.contains(&umbras[0]));
        if second_owner == P1 {
            assert!(!state.battlefield.contains(&umbras[1]));
            assert_eq!(state.objects[&host].zone, Zone::Graveyard);
            assert_eq!(
                result
                    .events
                    .iter()
                    .filter(|event| matches!(
                        event,
                        GameEvent::CreatureDestroyed { object_id, .. } if *object_id == host
                    ))
                    .count(),
                1,
                "zero remaining replacements must not silently skip the host"
            );
        } else {
            assert_eq!(state.objects[&host].zone, Zone::Battlefield);
            assert_eq!(
                state.objects[&umbras[1]].zone,
                Zone::Graveyard,
                "the one remaining mandatory armor replacement actually destroys its Aura"
            );
            assert!(!result.events.iter().any(|event| matches!(
                event,
                GameEvent::CreatureDestroyed { object_id, .. } if *object_id == host
            )));
        }
        assert_eq!(state.objects[&sibling].zone, Zone::Graveyard);
        assert_eq!(state.objects[&wrath].zone, Zone::Graveyard);
        assert_eq!(
            result
                .events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::CreatureDestroyed { object_id, .. } if *object_id == sibling
                ))
                .count(),
            1
        );
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.pending_replacement.is_none());
        assert!(state.stack.is_empty());
        assert!(state.resolution_stack.is_empty());
        state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .expect("removing old candidates still completes the original population");
    }
}

/// CR 616.1 + CR 800.4a: retain an already-approved Destroy's inner zone-change
/// event while its controller changes. The reasked source is a graveyard
/// redirect, not a replay of the outer destruction.
#[test]
fn singleton_destroy_inner_graveyard_choice_reasks_after_controller_departure() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(p2, "Grizzly Bears", 2, 2).id();
    let sibling = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let control = scenario
        .add_enchantment_from_oracle(
            P1,
            "Mind Control",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let rest = scenario.add_enchantment_from_oracle(
        P0,
        "Rest in Peace",
        "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.",
    ).id();
    let leyline = scenario.add_enchantment_from_oracle(
        P0,
        "Leyline of the Void",
        "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.",
    ).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    // Infrastructure exception: preattach Mind Control instead of casting it;
    // Murder publicly produces the inner consultation and its replacement context.
    engine::game::effects::attach::attach_to(runner.state_mut(), control, host);
    engine::game::layers::flush_layers(runner.state_mut());
    let pending = runner
        .cast(murder)
        .target_object(host)
        .commit()
        .try_resolve()
        .unwrap();
    assert!(matches!(
        pending.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    assert!(matches!(
        &runner.state().pending_replacement.as_ref().unwrap().proposed,
        ProposedEvent::ZoneChange { object_id, from: Zone::Battlefield, to: Zone::Graveyard, .. }
            if *object_id == host
    ));
    runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "the inner move must retain its live choice: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, p2);
    assert_eq!(candidates.len(), 2);
    assert!(candidates
        .iter()
        .any(|candidate| candidate.source_id == rest));
    let redirect = candidates
        .iter()
        .position(|candidate| candidate.source_id == leyline)
        .expect("the surviving Leyline is offered for the same inner graveyard move");
    assert!(matches!(
        &runner.state().pending_replacement.as_ref().unwrap().proposed,
        ProposedEvent::ZoneChange { object_id, from: Zone::Battlefield, to: Zone::Graveyard, .. }
            if *object_id == host
    ));
    let result = runner
        .act(GameAction::ChooseReplacement { index: redirect })
        .unwrap();
    let state = runner.state();
    assert_eq!(state.objects[&host].zone, Zone::Exile);
    assert_eq!(state.objects[&sibling].zone, Zone::Battlefield);
    assert_eq!(state.objects[&murder].zone, Zone::Exile);
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::ZoneChanged { object_id, .. } if *object_id == host
            ))
            .count(),
        1,
        "the retained inner event moves its subject exactly once"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.pending_replacement.is_none());
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
    state
        .resolution_stack
        .validate_at_rest(&state.waiting_for)
        .expect("the inner redirect completes the singleton Destroy");
}

/// CR 614.5 + CR 616.1 + CR 800.4a: reconsult the modified counter event,
/// preserving its applied replacement and depth while refreshing source control.
/// The printed effects must produce (1 + 1) * 2 + 1 = 5, not restart at one.
#[test]
fn reconsult_preserves_modified_counter_event_after_controller_departure() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(p2, "Grizzly Bears", 2, 2).id();
    let mind_control = scenario
        .add_enchantment_from_oracle(
            P1,
            "Mind Control",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let scales = scenario.add_enchantment_from_oracle(
        p2,
        "Hardened Scales",
        "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.",
    ).id();
    let branching = scenario.add_enchantment_from_oracle(
        p2,
        "Branching Evolution",
        "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.",
    ).id();
    let steal_scales = scenario
        .add_enchantment_from_oracle(
            P1,
            "Steal Enchantment",
            "Enchant enchantment\nYou control enchanted enchantment.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let steal_branching = scenario
        .add_enchantment_from_oracle(
            P1,
            "Steal Enchantment",
            "Enchant enchantment\nYou control enchanted enchantment.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let constrictors: Vec<_> = [P1, p2].into_iter().map(|owner|
        scenario.add_creature_from_oracle(
            owner,
            "Winding Constrictor",
            2,
            3,
            "If one or more counters would be put on an artifact or creature you control, that many plus one of each of those kinds of counters are put on that permanent instead.\nIf you would get one or more counters, you get that many plus one of each of those kinds of counters instead.",
        ).with_subtypes(vec!["Snake"]).id()
    ).collect();
    let travel = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Travel Preparations",
        false,
        "Put a +1/+1 counter on each of up to two target creatures.\nFlashback {1}{W} (You may cast this card from your graveyard for its flashback cost. Then exile it.)",
    ).with_mana_cost(ManaCost::zero()).id();
    let mut runner = scenario.build();
    // Infrastructure exception: preattach the full-Oracle control Auras instead
    // of casting them. Travel Preparations publicly creates every pending event.
    for (aura, enchanted) in [
        (mind_control, host),
        (steal_scales, scales),
        (steal_branching, branching),
    ] {
        engine::game::effects::attach::attach_to(runner.state_mut(), aura, enchanted);
    }
    engine::game::layers::flush_layers(runner.state_mut());
    for id in [host, scales, branching] {
        assert_eq!(runner.state().objects[&id].controller, P1);
    }
    let pending = runner
        .cast(travel)
        .target_object(host)
        .commit()
        .try_resolve()
        .unwrap();
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = pending.final_waiting_for()
    else {
        panic!(
            "the printed counter replacements must offer ordering: {:?}",
            pending.final_waiting_for()
        );
    };
    assert_eq!(*player, P1);
    assert_eq!(candidates.len(), 3);
    for source in [scales, branching, constrictors[0]] {
        assert!(candidates
            .iter()
            .any(|candidate| candidate.source_id == source));
    }
    let scales_index = candidates
        .iter()
        .position(|candidate| candidate.source_id == scales)
        .expect("select Hardened Scales by its actual source identity");
    runner
        .act(GameAction::ChooseReplacement {
            index: scales_index,
        })
        .unwrap();
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = &runner.state().waiting_for
    else {
        panic!("Branching and the first controller's Constrictor must still compete");
    };
    assert_eq!(*player, P1);
    assert_eq!(candidates.len(), 2);
    for source in [branching, constrictors[0]] {
        assert!(candidates
            .iter()
            .any(|candidate| candidate.source_id == source));
    }
    let modified = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the modified counter event is still suspended");
    assert_eq!(modified.proposed.affected_object_id(), Some(host));
    assert!(matches!(
        &modified.proposed,
        ProposedEvent::AddCounter { count: 2, .. }
    ));
    assert!(modified.depth > 0);
    assert!(modified.proposed.applied_set().iter().any(|key| matches!(
        key, AppliedReplacementKey::Object { source, .. } if *source == scales
    )));
    let retained_proposal = modified.proposed.clone();
    let retained_depth = modified.depth;
    runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    let wire = serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
        .expect("the modified reasked event serializes");
    let restored = serde_json::from_value::<PersistedGameState>(wire)
        .expect("the modified reasked event decodes")
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("the modified reasked event prepares")
        .finalize_immediately()
        .expect("the modified reasked event finalizes");
    for mut runner in [runner, GameRunner::from_state(restored)] {
        let state = runner.state();
        for id in [host, scales, branching] {
            assert_eq!(state.objects[&id].controller, p2);
        }
        assert!(!state.battlefield.contains(&constrictors[0]));
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &state.waiting_for
        else {
            panic!("the new controller must choose for the modified counter event");
        };
        assert_eq!(*player, p2);
        assert_eq!(candidates.len(), 2);
        for source in [branching, constrictors[1]] {
            assert!(candidates
                .iter()
                .any(|candidate| candidate.source_id == source));
        }
        assert!(
            !candidates
                .iter()
                .any(|candidate| candidate.source_id == scales),
            "the already-applied Scales must not be offered again after control reversion"
        );
        let modified = state.pending_replacement.as_ref().unwrap();
        assert_eq!(modified.proposed, retained_proposal);
        assert_eq!(modified.depth, retained_depth);
        let branching_index = candidates
            .iter()
            .position(|candidate| candidate.source_id == branching)
            .expect(
                "double the retained two counters before adding the surviving Constrictor's one",
            );
        runner
            .act(GameAction::ChooseReplacement {
                index: branching_index,
            })
            .unwrap();
        let state = runner.state();
        assert_eq!(state.objects[&host].counters.get(&CounterType::Plus1Plus1), Some(&5),
            "preserve Scales' earlier +1, then apply Branching and the current controller's Constrictor");
        assert_eq!(state.objects[&travel].zone, Zone::Graveyard);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.pending_replacement.is_none());
        assert!(state.stack.is_empty());
        assert!(state.resolution_stack.is_empty());
        state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .expect("the modified counter event finishes after its source-identified answer");
    }
}

/// CR 800.4a + CR 616.1: each departure consults the event's current authority,
/// not the chooser captured at its first pause.
#[test]
fn staged_destroy_can_reask_after_two_successive_controller_departures() {
    let p2 = PlayerId(2);
    let p3 = PlayerId(3);
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(p3, "Grizzly Bears", 2, 2).id();
    scenario.with_counter(host, CounterType::Shield, 1);
    let sibling = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let earlier_control = scenario
        .add_enchantment_from_oracle(
            p2,
            "Mind Control",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let later_control = scenario
        .add_enchantment_from_oracle(
            P1,
            "Mind Control",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let umbra = scenario.add_enchantment_from_oracle(
        p3,
        "Hyena Umbra",
        "Enchant creature\nEnchanted creature gets +1/+1 and has first strike.\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)",
    ).with_subtypes(vec!["Aura"]).id();
    let day = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);
    let mut runner = scenario.build();
    // Infrastructure exception: preattach both printed control Auras in
    // timestamp order and the Umbra, then publicly produce the Destroy event.
    for aura in [earlier_control, later_control, umbra] {
        engine::game::effects::attach::attach_to(runner.state_mut(), aura, host);
    }
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&host].controller, P1);
    let pending = runner.cast(day).commit().try_resolve().unwrap();
    assert!(matches!(
        pending.final_waiting_for(),
        WaitingFor::ReplacementChoice { player: P1, .. }
    ));
    let original = runner
        .state()
        .pending_replacement
        .as_ref()
        .unwrap()
        .proposed
        .clone();
    let original_depth = runner.state().pending_replacement.as_ref().unwrap().depth;
    for (leaver, next_controller) in [(P1, p2), (p2, p3)] {
        runner
            .act(GameAction::Concede { player_id: leaver })
            .unwrap();
        assert_eq!(runner.state().objects[&host].controller, next_controller);
        let WaitingFor::ReplacementChoice {
            player, candidates, ..
        } = &runner.state().waiting_for
        else {
            panic!("each surviving controller must receive the unanswered consultation");
        };
        assert_eq!(*player, next_controller);
        assert_eq!(candidates.len(), 2);
        for source in [host, umbra] {
            assert!(candidates
                .iter()
                .any(|candidate| candidate.source_id == source));
        }
        let pending = runner.state().pending_replacement.as_ref().unwrap();
        assert_eq!(pending.proposed, original);
        assert_eq!(pending.depth, original_depth);
        assert_eq!(runner.state().objects[&sibling].zone, Zone::Battlefield);
    }
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        unreachable!("the second reasked prompt was asserted above");
    };
    let shield = candidates
        .iter()
        .position(|candidate| candidate.source_id == host)
        .expect("P3 can choose its host's shield after both control Auras leave");
    runner
        .act(GameAction::ChooseReplacement { index: shield })
        .unwrap();
    let state = runner.state();
    assert_eq!(state.objects[&host].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&host]
            .counters
            .get(&CounterType::Shield)
            .copied()
            .unwrap_or(0),
        0
    );
    assert_eq!(state.objects[&umbra].zone, Zone::Battlefield);
    assert_eq!(state.objects[&sibling].zone, Zone::Graveyard);
    assert_eq!(state.objects[&day].zone, Zone::Graveyard);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.pending_replacement.is_none());
    assert!(state.stack.is_empty());
    assert!(state.resolution_stack.is_empty());
    state
        .resolution_stack
        .validate_at_rest(&state.waiting_for)
        .expect("the twice-reasked population finishes without an orphaned owner");
}

/// CR 104.1 + CR 104.2a: a two-player concession ends the game immediately;
/// replacement reconciliation must not publish a new living-player choice.
#[test]
fn two_player_replacement_chooser_concession_keeps_game_over() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.with_counter(host, CounterType::Shield, 1);
    let umbra = scenario.add_enchantment_from_oracle(
        P1,
        "Hyena Umbra",
        "Enchant creature\nEnchanted creature gets +1/+1 and has first strike.\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)",
    ).with_subtypes(vec!["Aura"]).id();
    let day = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);
    let mut runner = scenario.build();
    // Infrastructure exception: preattach the printed protection Aura instead
    // of casting it; Day publicly produces the genuine two-candidate prompt.
    engine::game::effects::attach::attach_to(runner.state_mut(), umbra, host);
    let pending = runner.cast(day).commit().try_resolve().unwrap();
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = pending.final_waiting_for()
    else {
        panic!("the game must first reach its real replacement choice");
    };
    assert_eq!(*player, P1);
    assert_eq!(candidates.len(), 2);
    for source in [host, umbra] {
        assert!(candidates
            .iter()
            .any(|candidate| candidate.source_id == source));
    }
    runner.act(GameAction::Concede { player_id: P1 }).unwrap();
    let state = runner.state();
    assert!(matches!(
        state.waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    ));
    assert!(state.pending_replacement.is_none());
    assert!(state.resolution_stack.is_empty());
}

/// CR 400.7 + CR 603.7c: the later member returns to the battlefield as a NEW
/// object, with a live replacement ready to ask for that new object. It is still
/// never offered: the staged population froze the old incarnation, so the guard
/// preempts the slot instead of consulting the replacement.
#[test]
fn later_member_returned_as_a_new_incarnation_is_never_offered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let watcher = scenario.add_creature(P0, "Watcher of P0", 1, 1).id();
    let first = scenario.add_creature(P1, "First Bear", 2, 2).id();
    let later = scenario.add_creature(P1, "Later Bear", 2, 2).id();
    let day = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);

    let mut runner = scenario.build();
    add_source_authority_optional_replacement(runner.state_mut(), watcher, "First Bear");
    runner.cast(day).commit();
    resolve_to_first_replacement_choice(&mut runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));

    // The later member leaves and returns as a new incarnation, carrying a live
    // replacement that WOULD prompt if it were consulted as itself.
    let mut moved = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), later, Zone::Hand, &mut moved);
    engine::game::zones::move_to_zone(runner.state_mut(), later, Zone::Battlefield, &mut moved);
    add_source_authority_optional_replacement(runner.state_mut(), watcher, "Later Bear");

    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("P0 declines the first member's replacement");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the new incarnation is never offered: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        zone_of_state(runner.state(), later),
        Zone::Battlefield,
        "the returned object is untouched by the frozen population"
    );
    assert_eq!(zone_of_state(runner.state(), first), Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

const HYENA_UMBRA: &str = "Enchant creature\nEnchanted creature gets +1/+1 and has first strike.\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)";

/// CR 616.1 + CR 800.4a + CR 701.8a: a Destroy member whose outer consultation
/// paused and was then approved (its chooser left with both Umbras, so nothing
/// replaces the destruction) still owns the approval cursor: its inner
/// Battlefield-to-Graveyard consultation runs before the next member is asked.
/// Day of Judgment says "Destroy all creatures." The first host is P2's creature
/// under P1's Mind Control wearing two of P1's Hyena Umbras (P1 orders them);
/// the second is P2's creature wearing two of P2's own Hyena Umbras (P2 orders
/// them). The batch is saved and reopened at both prompts, so a second member
/// parked while the first is still mid-consultation fails the staged validator.
#[test]
fn approved_outer_destroy_finishes_before_the_next_member_parks() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(p2, "Grizzly Bears", 2, 2).id();
    let second = scenario.add_creature(p2, "Runeclaw Bear", 2, 2).id();
    let control = scenario
        .add_enchantment_from_oracle(
            P1,
            "Mind Control",
            "Enchant creature\nYou control enchanted creature.",
        )
        .with_subtypes(vec!["Aura"])
        .id();
    let umbras: Vec<ObjectId> = [P1, P1, p2, p2]
        .into_iter()
        .map(|owner| {
            scenario
                .add_enchantment_from_oracle(owner, "Hyena Umbra", HYENA_UMBRA)
                .with_subtypes(vec!["Aura"])
                .id()
        })
        .collect();
    let day = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);
    let mut runner = scenario.build();
    // Infrastructure exception: preattach the full-Oracle Auras instead of
    // casting them; the consultations are produced by the public cast.
    for (aura, host) in [
        (control, first),
        (umbras[0], first),
        (umbras[1], first),
        (umbras[2], second),
        (umbras[3], second),
    ] {
        engine::game::effects::attach::attach_to(runner.state_mut(), aura, host);
    }
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&first].controller, P1);

    runner.cast(day).commit();
    resolve_to_first_replacement_choice(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player, candidates, ..
        } => {
            assert_eq!(
                *player, P1,
                "the first member's controller orders its Umbras"
            );
            assert_eq!(candidates.len(), 2);
            assert!(candidates
                .iter()
                .all(|candidate| candidate.source_id == umbras[0]
                    || candidate.source_id == umbras[1]));
        }
        other => panic!("expected P1's Umbra order for the first member, got {other:?}"),
    }
    round_trip_staged(&mut runner);

    // P1 leaves: Mind Control and both of P1's Umbras leave with it, so the
    // first member's destruction is approved with nothing left to replace it.
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("P1 concedes at its own prompt");
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player, candidates, ..
        } => {
            assert_eq!(
                *player, p2,
                "the second member's controller orders its Umbras"
            );
            assert_eq!(candidates.len(), 2);
            assert!(candidates
                .iter()
                .all(|candidate| candidate.source_id == umbras[2]
                    || candidate.source_id == umbras[3]));
        }
        other => panic!("expected P2's Umbra order for the second member, got {other:?}"),
    }
    let staged = runner
        .state()
        .active_batch_delivery()
        .and_then(|pending| pending.staged.as_ref())
        .expect("the staged owner is parked at the second prompt");
    assert_eq!(
        staged.members[staged.cursor].identity.object_id, second,
        "the parked member is the cursor member"
    );
    round_trip_staged(&mut runner);

    let chosen = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { candidates, .. } => candidates
            .iter()
            .position(|candidate| candidate.source_id == umbras[2])
            .expect("P2's first Umbra is offered"),
        other => panic!("expected the reopened second prompt, got {other:?}"),
    };
    let answered = runner
        .act(GameAction::ChooseReplacement { index: chosen })
        .expect("P2 applies its first Umbra");
    let state = runner.state();
    assert_eq!(state.objects[&first].zone, Zone::Graveyard);
    assert_eq!(state.objects[&second].zone, Zone::Battlefield);
    assert_eq!(state.objects[&umbras[2]].zone, Zone::Graveyard);
    assert_eq!(state.objects[&umbras[3]].zone, Zone::Battlefield);
    assert_eq!(
        answered
            .events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::CreatureDestroyed { object_id, .. } if *object_id == first
            ))
            .count(),
        1,
        "the approved first member is destroyed exactly once"
    );
    assert!(!answered.events.iter().any(|event| matches!(
        event,
        GameEvent::CreatureDestroyed { object_id, .. } if *object_id == second
    )));
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.pending_replacement.is_none());
    assert!(state.resolution_stack.is_empty());
}

/// CR 400.7 + CR 603.7c: a staged member that changed zone while an earlier
/// chooser was answering is refused before any replacement is offered for it.
/// The pause here is a real replacement choice with the second member still
/// unconsulted, not the commander graveyard SBA that runs after delivery.
#[test]
fn staged_member_that_left_during_a_pause_is_not_consulted() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let watcher = scenario.add_creature(P0, "Watcher of P0", 1, 1).id();
    let first = scenario.add_creature(P1, "First Bear", 2, 2).id();
    let later = scenario.add_creature(P1, "Later Bear", 2, 2).id();
    let day = zero_cost_spell(&mut scenario, "Day of Judgment", DAY_OF_JUDGMENT);

    let mut runner = scenario.build();
    add_source_authority_optional_replacement(runner.state_mut(), watcher, "First Bear");
    runner.cast(day).commit();
    resolve_to_first_replacement_choice(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player, candidates, ..
        } => {
            assert_eq!(*player, P0, "the first member's chooser is asked first");
            assert!(candidates
                .iter()
                .any(|candidate| candidate.source_id == watcher));
        }
        other => panic!("expected the first member's replacement choice, got {other:?}"),
    }
    let staged = runner
        .state()
        .active_batch_delivery()
        .and_then(|pending| pending.staged.as_ref())
        .expect("the staged owner is parked at the choice");
    assert!(
        staged.members.iter().any(|member| {
            member.identity.object_id == later
                && member.state == engine::types::game_state::StagedBatchMemberState::Unconsulted
        }),
        "the later member must still be unconsulted when the pause happens"
    );

    // Stand-in for another effect moving the not-yet-consulted member while the
    // chooser is still deciding.
    let mut moved = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), later, Zone::Hand, &mut moved);
    assert_eq!(zone_of_state(runner.state(), later), Zone::Hand);

    let answered = runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("P0 declines, so the first member is destroyed");
    assert_eq!(zone_of_state(runner.state(), first), Zone::Graveyard);
    assert_eq!(
        zone_of_state(runner.state(), later),
        Zone::Hand,
        "the departed member is never destroyed from its new zone"
    );
    assert!(
        !answered.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, .. } if *object_id == later
        )),
        "no delivery is attempted for the member that left"
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Test fixture only: no printed card has this exact shape (an optional
/// `Moved` replacement whose chooser is the granting source's controller).
/// Nearest printed analogue: Battletide Alchemist ("If a source an opponent
/// controls would deal damage to a player, you may prevent that damage"),
/// where the choice authority is the replacement source's controller under
/// CR 616.1. Used here to park a real replacement choice, not to test ordering.
fn add_source_authority_optional_replacement(state: &mut GameState, watcher: ObjectId, card: &str) {
    state
        .objects
        .get_mut(&watcher)
        .expect("watcher exists")
        .replacement_definitions
        .push(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .mode(ReplacementMode::Optional { decline: None })
                .valid_card(TargetFilter::Typed(engine::types::ability::TypedFilter {
                    type_filters: vec![engine::types::ability::TypeFilter::Creature],
                    controller: None,
                    properties: vec![engine::types::ability::FilterProp::Named {
                        name: card.to_string(),
                    }],
                }))
                .choice_authority(
                    engine::types::ability::ReplacementChoiceAuthority::SourceController,
                )
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        library_shuffle: Default::default(),
                        origin: None,
                        destination: Zone::Exile,
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
}

/// Serialize, decode through the checked restore boundary, and reopen.
fn round_trip_staged(runner: &mut GameRunner) {
    runner
        .state()
        .resolution_stack
        .validate_at_rest(&runner.state().waiting_for)
        .expect("the pause validates before the save");
    let saved = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("the pause serializes");
    let restored = serde_json::from_str::<PersistedGameState>(&saved)
        .expect("the pause restores")
        .into_game_state()
        .expect("the pause finalizes");
    restored
        .resolution_stack
        .validate_at_rest(&restored.waiting_for)
        .expect("the restored pause validates");
    *runner.state_mut() = restored;
}

fn zone_of_state(state: &GameState, id: ObjectId) -> Zone {
    state.objects[&id].zone
}

/// Pass priority until the spell resolves into its first replacement choice.
fn resolve_to_first_replacement_choice(runner: &mut GameRunner) {
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { .. } | WaitingFor::CommanderZoneChoice { .. } => return,
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes to resolve the spell");
            }
            other => panic!("expected a replacement choice, got {other:?}"),
        }
    }
    panic!("the destruction never reached a replacement choice");
}

const AKROMAS_VENGEANCE: &str =
    "Destroy all artifacts, creatures, and enchantments.\nCycling {3} ({3}, Discard this card: Draw a card.)";

const SPIDER_UMBRA: &str = "Enchant creature\nEnchanted creature gets +1/+1 and has reach. (It can block creatures with flying.)\nUmbra armor (If enchanted creature would be destroyed, instead remove all damage from it and destroy this Aura.)";

fn zone_changes_of(events: &[GameEvent], id: ObjectId) -> Vec<&GameEvent> {
    events
        .iter()
        .filter(
            |event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == id),
        )
        .collect()
}

fn destroyed_reports_of(events: &[GameEvent], id: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event, GameEvent::CreatureDestroyed { object_id, .. } if *object_id == id)
        })
        .count()
}

/// CR 702.89a + CR 701.8a + CR 603.10a: Akroma's Vengeance destroys the
/// creature and its Hyena Umbra at once. The Umbra's armor replaces the
/// creature's destruction by destroying the Aura, so the Aura's own slot has
/// exactly one occurrence (it is put into the graveyard once) whether the Aura
/// is consulted before or after the creature it protects. In the third case a
/// later host wears Hyena Umbra and Spider Umbra, so its owner's ordering
/// choice (CR 616.1) parks the batch after the first Aura already left during
/// approval; that departure must survive the pause, a save and reopen, and the
/// answer, with one occurrence for the Aura's own incarnation and a completed
/// batch.
#[test]
fn akromas_vengeance_umbra_member_gets_one_terminal_in_either_order() {
    for (aura_first, later_choice) in [(true, false), (false, false), (true, true)] {
        let case = format!("aura_first={aura_first} later_choice={later_choice}");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let add_aura = |scenario: &mut GameScenario, name: &str, oracle: &str| {
            scenario
                .add_enchantment_from_oracle(P1, name, oracle)
                .with_subtypes(vec!["Aura"])
                .id()
        };
        let (umbra, host) = if aura_first {
            let umbra = add_aura(&mut scenario, "Hyena Umbra", HYENA_UMBRA);
            (umbra, scenario.add_creature(P1, "Grizzly Bears", 2, 2).id())
        } else {
            let host = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
            (add_aura(&mut scenario, "Hyena Umbra", HYENA_UMBRA), host)
        };
        let bystander = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
        let later = later_choice.then(|| {
            let hyena = add_aura(&mut scenario, "Hyena Umbra", HYENA_UMBRA);
            let spider = add_aura(&mut scenario, "Spider Umbra", SPIDER_UMBRA);
            let host = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
            (hyena, spider, host)
        });
        let vengeance = zero_cost_spell(&mut scenario, "Akroma's Vengeance", AKROMAS_VENGEANCE);
        let mut runner = scenario.build();
        // Infrastructure exception: preattach the full-Oracle Auras instead of
        // casting them; the destruction population is produced by the public cast.
        engine::game::effects::attach::attach_to(runner.state_mut(), umbra, host);
        if let Some((hyena, spider, later_host)) = later {
            engine::game::effects::attach::attach_to(runner.state_mut(), hyena, later_host);
            engine::game::effects::attach::attach_to(runner.state_mut(), spider, later_host);
        }
        engine::game::layers::flush_layers(runner.state_mut());
        let umbra_identity = ObjectIncarnationRef::from_object(&runner.state().objects[&umbra]);

        let mut events = Vec::new();
        if let Some((hyena, spider, later_host)) = later {
            events.extend(runner.cast(vengeance).commit().events().to_vec());
            for _ in 0..8 {
                if matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { .. }
                ) {
                    break;
                }
                events.extend(runner.act(GameAction::PassPriority).unwrap().events);
            }
            let WaitingFor::ReplacementChoice {
                player, candidates, ..
            } = runner.state().waiting_for.clone()
            else {
                panic!(
                    "{case}: expected the later host's Umbra order, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(player, P1, "{case}");
            assert_eq!(candidates.len(), 2, "{case}");
            assert_eq!(
                runner.state().objects[&umbra].zone,
                Zone::Graveyard,
                "{case}: the first Aura left during approval, before the pause"
            );
            round_trip_staged(&mut runner);
            let spider_index = candidates
                .iter()
                .position(|candidate| candidate.source_id == spider)
                .expect("Spider Umbra is offered");
            events.extend(
                runner
                    .act(GameAction::ChooseReplacement {
                        index: spider_index,
                    })
                    .expect("P1 applies Spider Umbra")
                    .events,
            );
            let state = runner.state();
            assert_eq!(state.objects[&later_host].zone, Zone::Battlefield, "{case}");
            assert_eq!(state.objects[&spider].zone, Zone::Graveyard, "{case}");
            // The unchosen Hyena Umbra is itself an enchantment in the
            // population, so Akroma's Vengeance destroys it directly.
            assert_eq!(state.objects[&hyena].zone, Zone::Graveyard, "{case}");
            for aura in [spider, hyena] {
                assert_eq!(zone_changes_of(&events, aura).len(), 1, "{case}");
                assert_eq!(destroyed_reports_of(&events, aura), 1, "{case}");
            }
            assert_eq!(destroyed_reports_of(&events, later_host), 0, "{case}");
        } else {
            events.extend(runner.cast(vengeance).resolve().events().to_vec());
        }
        let state = runner.state();
        assert_eq!(state.objects[&host].zone, Zone::Battlefield, "{case}");
        assert_eq!(state.objects[&umbra].zone, Zone::Graveyard, "{case}");
        assert_eq!(state.objects[&bystander].zone, Zone::Graveyard, "{case}");
        assert_eq!(state.objects[&vengeance].zone, Zone::Graveyard, "{case}");
        let umbra_moves = zone_changes_of(&events, umbra);
        assert_eq!(
            umbra_moves.len(),
            1,
            "{case}: the Aura is put into the graveyard once"
        );
        let GameEvent::ZoneChanged { record, .. } = umbra_moves[0] else {
            unreachable!()
        };
        assert_eq!(
            record
                .trigger_source_context()
                .map(|context| context.identity.reference),
            Some(umbra_identity),
            "{case}: the one occurrence is the battlefield Aura's own incarnation"
        );
        assert_eq!(destroyed_reports_of(&events, host), 0, "{case}");
        assert_eq!(
            destroyed_reports_of(&events, umbra),
            1,
            "{case}: the Aura is reported destroyed once, by its armor"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "{case}"
        );
        assert!(state.pending_replacement.is_none(), "{case}");
        assert!(state.resolution_stack.is_empty(), "{case}");
        state
            .resolution_stack
            .validate_at_rest(&state.waiting_for)
            .expect("the population settles");
    }
}

const DAUTHI_VOIDWALKER: &str = "Shadow (This creature can block or be blocked by only creatures with shadow.)\nIf a card would be put into an opponent's graveyard from anywhere, instead exile it with a void counter on it.\n{T}, Sacrifice this creature: Choose an exiled card an opponent owns with a void counter on it. You may play it this turn without paying its mana cost.";
const REST_IN_PEACE_ORACLE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";

/// Cast Akroma's Vengeance on a three-player board where P0 controls Dauthi
/// Voidwalker and Rest in Peace and each opponent controls one creature, in
/// the given seat order, before or after P0's permanents. Answer every
/// replacement choice with its first option; return who was asked, in order.
fn vengeance_over_dauthi_and_rest_in_peace(
    opponents_in_battlefield_order: [PlayerId; 2],
    creatures_first: bool,
) -> (Vec<PlayerId>, GameRunner, [ObjectId; 2], [ObjectId; 2]) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let add_permanents = |scenario: &mut GameScenario| {
        [
            scenario
                .add_creature_from_oracle(P0, "Dauthi Voidwalker", 3, 2, DAUTHI_VOIDWALKER)
                .id(),
            scenario
                .add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE_ORACLE)
                .id(),
        ]
    };
    let mut permanents = None;
    if !creatures_first {
        permanents = Some(add_permanents(&mut scenario));
    }
    let creatures = opponents_in_battlefield_order
        .map(|owner| scenario.add_creature(owner, "Grizzly Bears", 2, 2).id());
    let permanents = permanents.unwrap_or_else(|| add_permanents(&mut scenario));
    let vengeance = zero_cost_spell(&mut scenario, "Akroma's Vengeance", AKROMAS_VENGEANCE);
    let mut runner = scenario.build();
    runner.cast(vengeance).commit();
    let mut asked = Vec::new();
    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { player, .. } => {
                asked.push(player);
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("the asked player answers");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    (asked, runner, creatures, permanents)
}

/// CR 616.1: "If two or more players have to make these choices at the same
/// time, choices are made in APNAP order." Dauthi Voidwalker ("If a card would
/// be put into an opponent's graveyard from anywhere, instead exile it with a
/// void counter on it") and Rest in Peace both apply to each opponent's
/// creature while the whole Vengeance population is consulted before delivery
/// (CR 701.8a: one simultaneous destruction), so P1 and P2 each choose. With
/// P0 active, P1 is asked before P2 whatever the battlefield order of their
/// creatures or of P0's permanents, and every member is destroyed.
#[test]
fn vengeance_never_asks_two_players_out_of_apnap_order() {
    let p2 = PlayerId(2);
    for (seats, creatures_first) in [([p2, P1], false), ([P1, p2], false), ([p2, P1], true)] {
        let case = format!("seats={seats:?} creatures_first={creatures_first}");
        let (asked, runner, creatures, permanents) =
            vengeance_over_dauthi_and_rest_in_peace(seats, creatures_first);
        assert_eq!(asked, vec![P1, p2], "{case}: choices are asked APNAP");
        for creature in creatures {
            assert_eq!(
                runner.state().objects[&creature].zone,
                Zone::Exile,
                "{case}: both replacements apply to the simultaneous destruction"
            );
        }
        for permanent in permanents {
            assert!(
                !runner.state().battlefield.contains(&permanent),
                "{case}: Dauthi and Rest in Peace are destroyed too"
            );
        }
        assert!(runner.state().resolution_stack.is_empty(), "{case}");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}
