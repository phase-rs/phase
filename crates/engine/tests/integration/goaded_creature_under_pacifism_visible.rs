//! Combat-requirement visibility (B1): a goaded creature under a Pacifism-class
//! restriction ("Enchanted creature can't attack or block.") must surface as
//! `CantAttack`, NOT `MustAttack`, and must NOT be force-attacked.
//!
//! CR 508.1c beats CR 508.1d: a "can't attack" restriction overrides an "attacks
//! if able" requirement. Before B1, `creature_must_attack` returned `true` for a
//! goaded creature even under Pacifism, so the declare-attackers enforcement
//! rejected an empty declaration AND the display would have shown `MustAttack`.
//!
//! Drives the REAL layer → combat pipeline: the constraints come from
//! `attacker_constraints_for_active_player` (the production authority `turns.rs`
//! uses to populate the `DeclareAttackers` waiting payload) and the enforcement
//! comes from `declare_attackers`. The Pacifism restriction is installed as a
//! functioning `CantAttackOrBlock` static (the same static the parser lowers
//! "can't attack or block" to) and resolved through `evaluate_layers`.

use std::sync::Arc;

use engine::game::combat::{
    attacker_constraints_for_active_player, creature_cant_attack, declare_attackers,
    get_valid_attacker_ids, CombatRequirement,
};
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::PhaseOutCause;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityKind, ContinuousModification, Duration, Effect, EffectKind, FilterProp,
    StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const SOUND_OF_DRUMS_ORACLE: &str = "Enchant creature\nEnchanted creature is goaded.\nIf enchanted creature would deal combat damage to a permanent or player, it deals double that damage instead.\n{2}{R}: Return this card from your graveyard to your hand.";
const PSYCHIC_IMPETUS_ORACLE: &str = "Enchant creature\nEnchanted creature gets +2/+2 and is goaded. (It attacks each combat if able and attacks a player other than you if able.)\nWhenever enchanted creature attacks, you scry 2.";
const WAR_GAMES_ORACLE: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after IV.)\nI — Each player creates three tapped 1/1 white Warrior creature tokens. The tokens are goaded for as long as this Saga remains on the battlefield.\nII, III — Put a +1/+1 counter on each Warrior creature.\nIV — You may exile a nontoken creature you control. When you do, exile all Warriors.";
const LASER_SCREWDRIVER_ORACLE: &str = "{T}: Add one mana of any color.\n{1}, {T}: Tap target artifact.\n{2}, {T}: Surveil 1. (Look at the top card of your library. You may put that card into your graveyard.)\n{3}, {T}: Goad target creature. (Until your next turn, it attacks each combat if able and attacks a player other than you if able.)";
const KARDUR_ORACLE: &str = "When Kardur enters, until your next turn, creatures your opponents control attack each combat if able and attack a player other than you if able.\nWhenever an attacking creature dies, each opponent loses 1 life and you gain 1 life.";

/// Mark a creature goaded by `goader` (P0 is the active player, so its own
/// creatures are subject to CR 508.1d during its combat).
fn goad(runner: &mut engine::game::scenario::GameRunner, creature: ObjectId, goader: PlayerId) {
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .goaded_by
        .insert(goader);
}

/// Install a functioning intrinsic `CantAttackOrBlock` static on `creature`
/// (the Pacifism restriction: "Enchanted creature can't attack or block."),
/// mirroring the proven pattern in `willie_lumpkin_cant_attack.rs`.
fn pacify(runner: &mut engine::game::scenario::GameRunner, creature: ObjectId) {
    let def = StaticDefinition::new(StaticMode::CantAttackOrBlock)
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::CantAttackOrBlock,
        }]);
    let obj = runner.state_mut().objects.get_mut(&creature).unwrap();
    obj.static_definitions = vec![def.clone()].into();
    obj.base_static_definitions = Arc::new(vec![def]);
}

fn refresh(runner: &mut engine::game::scenario::GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

#[test]
fn goaded_creature_under_pacifism_is_visible_as_cant_attack_not_must_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    // A goaded creature P0 controls (P0 is the active player).
    let pacified = scenario.add_creature(P0, "Goaded Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    goad(&mut runner, pacified, P1);
    pacify(&mut runner, pacified);
    refresh(&mut runner);

    // Reach-guard: the Pacifism static is functioning (else the assertions below
    // would be vacuous — the creature would just be a plain goaded creature).
    assert!(
        creature_cant_attack(runner.state(), pacified),
        "sanity: Pacifism's CantAttackOrBlock static must be functioning"
    );

    // (a) DISPLAY: the pacified goaded creature surfaces as CantAttack.
    // REVERT-FAIL: before B1 the must-attack predicate returned true (goad), so
    // the helper emitted `MustAttack` here instead of `CantAttack`.
    let constraints = attacker_constraints_for_active_player(
        runner.state(),
        &get_valid_attacker_ids(runner.state()),
    );
    assert_eq!(
        constraints.get(&pacified),
        // CR 508.1c: intrinsic SelfRef Pacifism → carrier is the creature itself.
        Some(&CombatRequirement::CantAttack {
            sources: vec![pacified]
        }),
        "a goaded creature under Pacifism is CantAttack, not MustAttack"
    );

    // (b) ENFORCEMENT: declaring no attackers is legal — the pacified creature is
    // not force-attacked. REVERT-FAIL: before B1 this returned Err (goad forced).
    {
        let mut s = runner.state().clone();
        let mut events = Vec::new();
        assert!(
            declare_attackers(&mut s, &[], &mut events).is_ok(),
            "CR 508.1c: a goaded creature that can't attack is not force-attacked"
        );
    }

    // (c) DIFFERENTIAL: an unencumbered goaded creature in the SAME state is
    // MustAttack{players:[]}, proving it is Pacifism specifically that neutralizes
    // the requirement — not a blanket suppression of goad display.
    let unencumbered = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(9999),
        P0,
        "Unencumbered Goaded Bear".to_string(),
        engine::types::zones::Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&unencumbered).unwrap();
        obj.card_types.core_types = vec![engine::types::card_type::CoreType::Creature];
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        // CR 302.6: not summoning sick, so it is a valid attacker (must-attack
        // only surfaces for eligible attackers).
        obj.summoning_sick = false;
    }
    goad(&mut runner, unencumbered, P1);
    refresh(&mut runner);

    let constraints = attacker_constraints_for_active_player(
        runner.state(),
        &get_valid_attacker_ids(runner.state()),
    );
    assert_eq!(
        constraints.get(&unencumbered),
        // CR 701.15b: direct player-goad (`goaded_by`) carries no object source →
        // EMPTY sources. This is the documented player-level goad row.
        Some(&CombatRequirement::MustAttack {
            defenders: vec![],
            sources: vec![]
        }),
        "an unencumbered goaded creature must surface as MustAttack with no specific-player constraint"
    );
    assert_eq!(
        constraints.get(&pacified),
        // CR 508.1c: intrinsic SelfRef Pacifism → carrier is the creature itself.
        Some(&CombatRequirement::CantAttack {
            sources: vec![pacified]
        }),
        "the pacified creature stays CantAttack even with a sibling must-attacker present"
    );
}

#[test]
fn sound_of_drums_uses_auras_controller_and_survives_layer_refresh() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Bear", 2, 2).id();
    let sibling = scenario.add_creature(P1, "Bear", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "The Sound of Drums", "")
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(SOUND_OF_DRUMS_ORACLE)
        .id();
    let mut runner = scenario.build();
    engine::game::effects::attach::attach_to(runner.state_mut(), aura, host);
    assert_eq!(runner.state().objects[&aura].attached_to, Some(host.into()));
    assert!(runner.state().objects[&host].attachments.contains(&aura));
    assert_eq!(runner.state().objects[&aura].controller, P0);
    assert_eq!(runner.state().objects[&host].controller, P1);
    assert!(runner.state().objects[&host].goaded_by.is_empty());
    refresh(&mut runner);
    let goaded = TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded]));
    assert!(matches_target_filter(
        runner.state(),
        host,
        &goaded,
        &FilterContext::neutral()
    ));
    assert!(!matches_target_filter(
        runner.state(),
        sibling,
        &goaded,
        &FilterContext::neutral()
    ));

    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.advance_to_combat();
    assert_eq!(runner.waiting_for_kind(), "DeclareAttackers");
    let constraints = attacker_constraints_for_active_player(
        runner.state(),
        &get_valid_attacker_ids(runner.state()),
    );
    assert_eq!(
        constraints.get(&host),
        Some(&CombatRequirement::MustAttack {
            defenders: vec![],
            sources: vec![aura]
        })
    );

    let mut changed_controller = runner.state().clone();
    // CR 613.1b: change the Aura's control in layer 2 so it survives refresh.
    changed_controller.add_transient_continuous_effect(
        aura,
        p2,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: aura },
        vec![ContinuousModification::ChangeController],
        None,
    );
    evaluate_layers(&mut changed_controller);
    assert_eq!(changed_controller.objects[&aura].controller, p2);
    assert_eq!(changed_controller.objects[&host].controller, P1);
    assert!(matches_target_filter(
        &changed_controller,
        host,
        &goaded,
        &FilterContext::neutral()
    ));
    let mut against_new_goader = changed_controller.clone();
    assert!(declare_attackers(
        &mut against_new_goader,
        &[(host, AttackTarget::Player(p2))],
        &mut Vec::new()
    )
    .is_err());
    let mut away_from_new_goader = changed_controller.clone();
    assert!(declare_attackers(
        &mut away_from_new_goader,
        &[(host, AttackTarget::Player(P0))],
        &mut Vec::new()
    )
    .is_ok());

    let mut recipient_abilities_removed = runner.state().clone();
    recipient_abilities_removed.add_transient_continuous_effect(
        aura,
        P0,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: host },
        vec![ContinuousModification::RemoveAllAbilities],
        None,
    );
    evaluate_layers(&mut recipient_abilities_removed);
    assert!(matches_target_filter(
        &recipient_abilities_removed,
        host,
        &goaded,
        &FilterContext::neutral()
    ));

    let mut aura_abilities_removed = runner.state().clone();
    aura_abilities_removed.add_transient_continuous_effect(
        host,
        P1,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: aura },
        vec![ContinuousModification::RemoveAllAbilities],
        None,
    );
    evaluate_layers(&mut aura_abilities_removed);
    assert!(!matches_target_filter(
        &aura_abilities_removed,
        host,
        &goaded,
        &FilterContext::neutral()
    ));

    let mut departed = runner.state().clone();
    engine::game::zones::move_to_zone(&mut departed, aura, Zone::Graveyard, &mut Vec::new());
    evaluate_layers(&mut departed);
    assert!(!matches_target_filter(
        &departed,
        host,
        &goaded,
        &FilterContext::neutral()
    ));

    assert!(
        runner
            .declare_attackers(&[(host, AttackTarget::Player(P0))])
            .is_err(),
        "P2 is available, so the enchanted creature cannot attack its Aura controller"
    );
    runner
        .declare_attackers(&[(host, AttackTarget::Player(p2))])
        .expect("attacking a player other than the Aura controller obeys goad");
}

#[test]
fn psychic_impetus_keeps_power_toughness_bonus_with_goad_designation() {
    let mut scenario = GameScenario::new();
    let host = scenario.add_creature(P1, "Bear", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Psychic Impetus", "")
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(PSYCHIC_IMPETUS_ORACLE)
        .id();
    let mut runner = scenario.build();
    engine::game::effects::attach::attach_to(runner.state_mut(), aura, host);
    assert_eq!(runner.state().objects[&aura].attached_to, Some(host.into()));
    assert!(runner.state().objects[&host].attachments.contains(&aura));
    refresh(&mut runner);
    let creature = &runner.state().objects[&host];
    assert_eq!((creature.power, creature.toughness), (Some(4), Some(4)));
    let goaded = TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded]));
    assert!(matches_target_filter(
        runner.state(),
        host,
        &goaded,
        &FilterContext::neutral()
    ));
}

#[test]
fn war_games_registered_chapter_token_tracks_saga_presence() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let war = scenario
        .add_spell_to_hand(P0, "The War Games", false)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(WAR_GAMES_ORACLE)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(war).resolve();
    let state = outcome.state();
    assert!(state.stack.is_empty(), "chapter I must resolve");
    let saga = *state
        .battlefield
        .iter()
        .find(|id| state.objects[id].name == "The War Games")
        .expect("the Saga must resolve onto the battlefield");
    let warriors: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token
                && object
                    .card_types
                    .subtypes
                    .iter()
                    .any(|subtype| subtype == "Warrior")
        })
        .collect();
    assert_eq!(
        warriors.len(),
        9,
        "the full chapter I must create three Warriors per player"
    );
    assert!(warriors.iter().all(|id| state.objects[id].tapped));
    let recipient = state
        .transient_continuous_effects
        .iter()
        .find_map(|effect| {
            if effect.source_id != saga
                || effect.controller != P0
                || effect.duration != Duration::WhileHostOnBattlefield
                || !effect.modifications.iter().any(|modification| {
                    matches!(
                        modification,
                        ContinuousModification::AddStaticMode {
                            mode: StaticMode::Goaded
                        }
                    )
                })
            {
                return None;
            }
            match &effect.affected {
                TargetFilter::SpecificObject { id } => Some(*id),
                _ => None,
            }
        })
        .expect("chapter I must register at least one exact Warrior recipient");
    assert!(warriors.contains(&recipient));
    let goaded = TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded]));
    assert!(matches_target_filter(
        state,
        recipient,
        &goaded,
        &FilterContext::neutral()
    ));

    let mut ability_lost = state.clone();
    ability_lost.add_transient_continuous_effect(
        recipient,
        P1,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: saga },
        vec![ContinuousModification::RemoveAllAbilities],
        None,
    );
    evaluate_layers(&mut ability_lost);
    assert!(matches_target_filter(
        &ability_lost,
        recipient,
        &goaded,
        &FilterContext::neutral()
    ));

    let mut departed = state.clone();
    engine::game::zones::move_to_zone(&mut departed, saga, Zone::Graveyard, &mut Vec::new());
    evaluate_layers(&mut departed);
    assert!(!matches_target_filter(
        &departed,
        recipient,
        &goaded,
        &FilterContext::neutral()
    ));

    let mut phased = state.clone();
    let mut events = Vec::new();
    engine::game::phasing::phase_out_object(
        &mut phased,
        saga,
        PhaseOutCause::Directly,
        &mut events,
    );
    assert!(!phased.objects[&saga].is_phased_in());
    evaluate_layers(&mut phased);
    assert!(!matches_target_filter(
        &phased,
        recipient,
        &goaded,
        &FilterContext::neutral()
    ));
    engine::game::phasing::phase_in_object(&mut phased, saga, &mut events);
    evaluate_layers(&mut phased);
    assert!(!matches_target_filter(
        &phased,
        recipient,
        &goaded,
        &FilterContext::neutral()
    ));
}

#[test]
fn registered_warrior_direct_regoad_uses_each_goader_original_lifetime() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let war = scenario
        .add_spell_to_hand(P0, "The War Games", false)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(WAR_GAMES_ORACLE)
        .id();
    let first_source = scenario
        .add_artifact_from_oracle(P0, "Laser Screwdriver", LASER_SCREWDRIVER_ORACLE)
        .id();
    let second_source = scenario
        .add_artifact_from_oracle(P1, "Laser Screwdriver", LASER_SCREWDRIVER_ORACLE)
        .id();
    for player in [P0, P1] {
        scenario.with_mana_pool(
            player,
            (0..3)
                .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
                .collect(),
        );
    }
    let mut runner = scenario.build();
    runner.cast(war).resolve();
    assert!(runner.state().stack.is_empty(), "chapter I must resolve");
    let state = runner.state();
    let saga = *state
        .battlefield
        .iter()
        .find(|id| state.objects[id].name == "The War Games")
        .expect("the Saga must be on the battlefield");
    let recipient = state
        .transient_continuous_effects
        .iter()
        .find_map(|effect| {
            (effect.source_id == saga
                && effect.controller == P0
                && effect.duration == Duration::WhileHostOnBattlefield
                && effect
                    .modifications
                    .contains(&ContinuousModification::AddStaticMode {
                        mode: StaticMode::Goaded,
                    }))
            .then_some(match &effect.affected {
                TargetFilter::SpecificObject { id } => Some(*id),
                _ => None,
            })
            .flatten()
        })
        .expect("chapter I must register an exact live Warrior recipient");
    let creature = &state.objects[&recipient];
    assert!(creature.is_token);
    assert!(creature.card_types.core_types.contains(&CoreType::Creature));
    assert!(creature
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Warrior"));
    assert_ne!(
        creature.controller, P0,
        "the recipient and first goader differ"
    );
    assert!(creature.goaded_by.is_empty());
    let goaded = TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded]));
    assert!(matches_target_filter(
        state,
        recipient,
        &goaded,
        &FilterContext::neutral(),
    ));

    for (source, controller) in [(first_source, P0), (second_source, P1)] {
        let object = &runner.state().objects[&source];
        assert_eq!(object.controller, controller);
        assert!(object.abilities.iter().any(|ability| {
            ability.kind == AbilityKind::Activated
                && matches!(ability.effect.as_ref(), Effect::Goad { .. })
        }));
    }
    let first_goad = runner.state().objects[&first_source]
        .abilities
        .iter()
        .position(|ability| matches!(ability.effect.as_ref(), Effect::Goad { .. }))
        .expect("Laser Screwdriver's direct goad ability");
    let second_goad = runner.state().objects[&second_source]
        .abilities
        .iter()
        .position(|ability| matches!(ability.effect.as_ref(), Effect::Goad { .. }))
        .expect("the second Screwdriver's direct goad ability");

    // CR 701.15d: P0's real activation targets an already-designated creature.
    // It resolves, but cannot add a new direct next-turn deadline.
    let first_outcome = runner
        .activate(first_source, first_goad)
        .target_object(recipient)
        .resolve();
    assert!(first_outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Goad,
            source_id,
            ..
        } if *source_id == first_source
    )));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().objects[&first_source].tapped);
    assert!(
        runner.state().objects[&recipient].goaded_by.is_empty(),
        "same-goader activation must not add a direct deadline"
    );

    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    let second_outcome = runner
        .activate(second_source, second_goad)
        .target_object(recipient)
        .resolve();
    assert!(second_outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Goad,
            source_id,
            ..
        } if *source_id == second_source
    )));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().objects[&second_source].tapped);
    assert!(runner.state().battlefield.contains(&saga));
    assert!(runner
        .state()
        .transient_continuous_effects
        .iter()
        .any(|effect| {
            effect.source_id == saga
                && effect.controller == P0
                && effect.duration == Duration::WhileHostOnBattlefield
                && effect.affected == TargetFilter::SpecificObject { id: recipient }
        }));
    assert_eq!(
        runner.state().objects[&recipient].goaded_by,
        [P1].into_iter().collect(),
        "the different goader contributes one independent direct cause"
    );
    assert!(matches_target_filter(
        runner.state(),
        recipient,
        &goaded,
        &FilterContext::neutral(),
    ));

    engine::game::zones::move_to_zone(runner.state_mut(), saga, Zone::Graveyard, &mut Vec::new());
    evaluate_layers(runner.state_mut());
    assert!(!runner.state().battlefield.contains(&saga));
    assert_eq!(
        runner.state().objects[&recipient].goaded_by,
        [P1].into_iter().collect()
    );
    assert!(matches_target_filter(
        runner.state(),
        recipient,
        &goaded,
        &FilterContext::neutral(),
    ));

    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    // CR 701.15a: P1's direct designation ends at the start of P1's next turn.
    engine::game::turns::start_next_turn(runner.state_mut(), &mut Vec::new());
    engine::game::turns::execute_untap(runner.state_mut(), &mut Vec::new());
    assert_eq!(runner.state().active_player, P1);
    assert!(runner.state().objects[&recipient].goaded_by.is_empty());
    assert!(!matches_target_filter(
        runner.state(),
        recipient,
        &goaded,
        &FilterContext::neutral(),
    ));
}

#[test]
fn kardurs_attack_requirement_does_not_designate_creatures_goaded() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let kardur = scenario
        .add_creature_to_hand(P0, "Kardur, Doomscourge", 4, 3)
        .from_oracle_text(KARDUR_ORACLE)
        .id();
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.cast(kardur).resolve();
    assert!(runner.state().stack.is_empty());
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.advance_to_combat();
    assert_eq!(runner.waiting_for_kind(), "DeclareAttackers");
    let constraints = attacker_constraints_for_active_player(
        runner.state(),
        &get_valid_attacker_ids(runner.state()),
    );
    assert!(matches!(
        constraints.get(&bear),
        Some(CombatRequirement::MustAttack { .. })
    ));
    let goaded = TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded]));
    assert!(!matches_target_filter(
        runner.state(),
        bear,
        &goaded,
        &FilterContext::neutral()
    ));
}

/// CR 701.15b + CR 508.1d: an Aura that goads, cast and resolved in the
/// precombat main phase, is seen by the next declare-attackers query.
#[test]
fn sound_of_drums_cast_mid_turn_goads_at_next_declare_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Bear", 2, 2).id();
    let aura = scenario
        .add_spell_to_hand(P0, "The Sound of Drums", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], SOUND_OF_DRUMS_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    let mut uncast = engine::game::scenario::GameRunner::from_state(runner.state().clone());
    uncast.advance_to_combat();
    let uncast_valid = get_valid_attacker_ids(uncast.state());
    assert!(uncast_valid.contains(&host), "the Bear can attack");
    assert_eq!(
        attacker_constraints_for_active_player(uncast.state(), &uncast_valid).get(&host),
        None,
        "without the Aura the Bear has no attack requirement"
    );

    runner.cast(aura).target_object(host).resolve();
    assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&aura].attached_to, Some(host.into()));

    runner.advance_to_combat();
    assert_eq!(runner.waiting_for_kind(), "DeclareAttackers");
    assert_eq!(
        attacker_constraints_for_active_player(
            runner.state(),
            &get_valid_attacker_ids(runner.state()),
        )
        .get(&host),
        Some(&CombatRequirement::MustAttack {
            defenders: vec![],
            sources: vec![aura]
        })
    );
    assert!(
        runner.declare_attackers(&[]).is_err(),
        "the goaded Bear must attack"
    );
}

const FEALTY_TO_THE_REALM_ORACLE: &str = "Enchant creature\nWhen this Aura enters, you become the monarch.\nThe monarch controls enchanted creature.\nEnchanted creature attacks each combat if able and can't attack you.";

/// CR 508.1c + CR 508.1d: a goad-like Aura that never says "goad" forces the
/// enchanted creature to attack and keeps it off the Aura's controller, and does
/// not designate it goaded (CR 701.15a).
#[test]
fn fealty_to_the_realm_forces_and_redirects_without_goading() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Bear", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Fealty to the Realm", "")
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(FEALTY_TO_THE_REALM_ORACLE)
        .id();
    let mut runner = scenario.build();
    engine::game::effects::attach::attach_to(runner.state_mut(), aura, host);
    refresh(&mut runner);
    assert_eq!(runner.state().objects[&host].controller, P1);
    let goaded = TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::Goaded]));
    assert!(!matches_target_filter(
        runner.state(),
        host,
        &goaded,
        &FilterContext::neutral()
    ));

    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.advance_to_combat();
    assert_eq!(runner.waiting_for_kind(), "DeclareAttackers");
    assert!(
        get_valid_attacker_ids(runner.state()).contains(&host),
        "the enchanted creature can attack"
    );

    let mut no_attack = runner.state().clone();
    assert!(
        declare_attackers(&mut no_attack, &[], &mut Vec::new()).is_err(),
        "the enchanted creature attacks each combat if able"
    );
    let mut at_aura_controller = runner.state().clone();
    assert!(
        declare_attackers(
            &mut at_aura_controller,
            &[(host, AttackTarget::Player(P0))],
            &mut Vec::new()
        )
        .is_err(),
        "the enchanted creature can't attack the Aura's controller"
    );
    runner
        .declare_attackers(&[(host, AttackTarget::Player(p2))])
        .expect("attacking another player obeys both clauses");
}
