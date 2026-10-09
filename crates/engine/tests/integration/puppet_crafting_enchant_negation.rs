//! Runtime coverage for leg-local negative-subtype Enchant restrictions.
//! Fixtures parse the actual Oracle text rather than exported card data.

use std::str::FromStr;

use engine::game::casting::legal_target_slots_for_castable_spell;
use engine::game::game_object::AttachTarget;
use engine::game::layers::mark_layers_full;
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const PUPPET_ORACLE: &str = "Enchant artifact or non-Aura enchantment\nEnchanted permanent is a Construct creature with base power and toughness 5/5 in addition to its other types.\n{4}{G}: Return this card from your graveyard to your hand.";

fn mana(scenario: &mut GameScenario, generic: usize, green: usize) {
    scenario.with_mana_pool(
        P0,
        std::iter::repeat_n(ManaType::Colorless, generic)
            .chain(std::iter::repeat_n(ManaType::Green, green))
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn puppet(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, "Puppet Crafting", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .from_oracle_text_with_keywords(&["enchant"], PUPPET_ORACLE)
        .id()
}

fn parsed_reach(state: &GameState, source: ObjectId) {
    let obj = state.objects.get(&source).unwrap();
    assert!(obj
        .keywords
        .iter()
        .any(|keyword| matches!(keyword, Keyword::Enchant(_))));
    assert_eq!(obj.abilities.len(), 1);
    assert!(!matches!(
        *obj.abilities[0].effect,
        Effect::Unimplemented { .. }
    ));
    assert!(!obj.static_definitions.is_empty());
    assert!(obj.parse_warnings.is_empty(), "{:?}", obj.parse_warnings);
}

fn assert_animated(outcome: &CastOutcome, source: ObjectId, host: ObjectId, retained: CoreType) {
    outcome.assert_zone(&[source, host], Zone::Battlefield);
    let state = outcome.state();
    // CR 303.4b: the selected object is enchanted by this particular source.
    assert_eq!(
        state.objects.get(&source).unwrap().attached_to,
        Some(AttachTarget::Object(host))
    );
    let obj = state.objects.get(&host).unwrap();
    assert!(obj.attachments.contains(&source));
    // CR 205.1b + CR 613.1d: "in addition" preserves types and adds Construct creature.
    assert!(obj.card_types.core_types.contains(&retained));
    assert!(obj.card_types.core_types.contains(&CoreType::Creature));
    assert!(obj
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Construct"));
    // CR 613.4b: the printed ability sets base power and toughness in layer 7b.
    assert_eq!((obj.power, obj.toughness), (Some(5), Some(5)));
}

fn persistent_board(controller: PlayerId) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario
        .add_enchantment_from_oracle(controller, "Enchantment host", "")
        .with_subtypes(vec!["Shrine"])
        .id();
    let other = scenario
        .add_artifact_from_oracle(P0, "Artifact host", "")
        .id();
    let source = puppet(&mut scenario);
    mana(&mut scenario, 1, 1);
    let runner = scenario.build();
    parsed_reach(runner.state(), source);
    (runner, source, host, other)
}

#[test]
fn puppet_crafting_cast_animates_enchantment_and_artifact() {
    for artifact in [false, true] {
        let (mut runner, source, enchantment, other) = persistent_board(P0);
        let host = if artifact { other } else { enchantment };
        let slots = legal_target_slots_for_castable_spell(runner.state(), source);
        assert_eq!(slots.len(), 1);
        assert!(slots[0].legal_targets.contains(&TargetRef::Object(host)));
        let outcome = runner.cast(source).target_object(host).resolve();
        assert_animated(
            &outcome,
            source,
            host,
            if artifact {
                CoreType::Artifact
            } else {
                CoreType::Enchantment
            },
        );
        if !artifact {
            assert!(outcome
                .state()
                .objects
                .get(&host)
                .unwrap()
                .card_types
                .subtypes
                .iter()
                .any(|s| s == "Shrine"));
        }
    }
}

fn attach_fixture(state: &mut GameState, aura: ObjectId, host: ObjectId) {
    state.objects.get_mut(&aura).unwrap().attached_to = Some(AttachTarget::Object(host));
    state.objects.get_mut(&host).unwrap().attachments.push(aura);
    mark_layers_full(state);
}

struct MixedBoard {
    runner: GameRunner,
    source: ObjectId,
    artifact_aura: ObjectId,
    ordinary_aura: ObjectId,
    enchantment: ObjectId,
    artifact: ObjectId,
    stable: ObjectId,
}

fn mixed_board() -> MixedBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stable = scenario.add_creature(P0, "Stable creature", 2, 3).id();
    let artifact_aura = scenario
        .add_enchantment_from_oracle(P0, "Artifact Aura", "")
        .as_artifact()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::from_str("Enchant:creature").unwrap())
        .id();
    let ordinary_aura = scenario
        .add_enchantment_from_oracle(P0, "Ordinary Aura", "")
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::from_str("Enchant:creature").unwrap())
        .id();
    let enchantment = scenario
        .add_enchantment_from_oracle(P0, "Enchantment host", "")
        .id();
    let artifact = scenario
        .add_artifact_from_oracle(P0, "Artifact host", "")
        .id();
    let source = puppet(&mut scenario);
    mana(&mut scenario, 1, 1);
    let mut runner = scenario.build();
    attach_fixture(runner.state_mut(), artifact_aura, stable);
    attach_fixture(runner.state_mut(), ordinary_aura, stable);
    parsed_reach(runner.state(), source);
    MixedBoard {
        runner,
        source,
        artifact_aura,
        ordinary_aura,
        enchantment,
        artifact,
        stable,
    }
}

#[test]
fn puppet_crafting_artifact_aura_leg_is_legal_then_creature_aura_cleanup_runs() {
    let MixedBoard {
        mut runner,
        source,
        artifact_aura,
        ordinary_aura,
        enchantment,
        artifact,
        stable,
    } = mixed_board();
    let slots = legal_target_slots_for_castable_spell(runner.state(), source);
    assert_eq!(slots.len(), 1);
    // CR 702.5a: the artifact union leg is independent of the Aura exclusion.
    for host in [artifact_aura, enchantment, artifact] {
        assert!(slots[0].legal_targets.contains(&TargetRef::Object(host)));
    }
    assert!(!slots[0]
        .legal_targets
        .contains(&TargetRef::Object(ordinary_aura)));
    let committed = runner.cast(source).target_object(artifact_aura).commit();
    assert_committed(committed.state(), source, artifact_aura);
    assert_attached_alive(committed.state(), artifact_aura, stable);
    let outcome = committed.resolve();
    // CR 303.4d + CR 704.5p + CR 704.5m: animating the attached Aura removes
    // its attachment, then both that Aura and Puppet lose their legal hosts.
    outcome.assert_zone(&[artifact_aura, source], Zone::Graveyard);
    outcome.assert_zone(
        &[ordinary_aura, stable, enchantment, artifact],
        Zone::Battlefield,
    );
    assert_attached_alive(outcome.state(), ordinary_aura, stable);
}

#[test]
fn puppet_crafting_illegal_aura_selection_rejected_by_apply() {
    let MixedBoard {
        mut runner,
        source,
        ordinary_aura,
        enchantment,
        artifact,
        ..
    } = mixed_board();
    let card_id = runner.state().objects.get(&source).unwrap().card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: source,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .unwrap();
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!("must reach the actual target-selection prompt");
    };
    assert_eq!(target_slots.len(), 1);
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(enchantment)));
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(artifact)));
    let error = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(ordinary_aura)),
        })
        .unwrap_err();
    assert!(
        error.to_string().contains("Illegal target selected"),
        "{error}"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(enchantment)),
        })
        .unwrap();
    assert_committed(runner.state(), source, enchantment);
    runner.resolve_top();
    // CR 303.4b + CR 613.4b: a legal selection completes the same cast.
    assert_eq!(
        runner.state().objects.get(&source).unwrap().attached_to,
        Some(AttachTarget::Object(enchantment))
    );
    assert_eq!(
        runner.state().objects.get(&enchantment).unwrap().power,
        Some(5)
    );
}

#[test]
fn puppet_crafting_no_legal_host_rejected_with_successful_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stable = scenario.add_creature(P0, "Stable creature", 2, 3).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Ordinary Aura", "")
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::from_str("Enchant:creature").unwrap())
        .id();
    let source = puppet(&mut scenario);
    mana(&mut scenario, 1, 1);
    let mut runner = scenario.build();
    attach_fixture(runner.state_mut(), aura, stable);
    parsed_reach(runner.state(), source);
    let error = runner
        .cast(source)
        .try_resolve()
        .err()
        .expect("no legal host must fail");
    // CR 303.4a + CR 702.5a: a typed restriction with an empty legal set
    // cannot fall through to an untargeted permanent cast.
    assert!(
        error.to_string().contains("No legal targets for Aura"),
        "{error}"
    );
    let (mut control, source, host, _) = persistent_board(P0);
    let outcome = control.cast(source).target_object(host).resolve();
    assert_animated(&outcome, source, host, CoreType::Enchantment);
}

#[test]
fn puppet_crafting_sources_bind_separate_hosts_and_preserve_controllers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opponent_host = scenario
        .add_enchantment_from_oracle(P1, "Opponent host", "")
        .id();
    let own_host = scenario
        .add_enchantment_from_oracle(P0, "Own host", "")
        .id();
    let first = puppet(&mut scenario);
    let second = puppet(&mut scenario);
    mana(&mut scenario, 2, 2);
    let mut runner = scenario.build();
    parsed_reach(runner.state(), first);
    parsed_reach(runner.state(), second);
    let first_outcome = runner.cast(first).target_object(opponent_host).resolve();
    assert_animated(&first_outcome, first, opponent_host, CoreType::Enchantment);
    assert!(!first_outcome
        .state()
        .objects
        .get(&own_host)
        .unwrap()
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    let outcome = runner.cast(second).target_object(own_host).resolve();
    assert_animated(&outcome, first, opponent_host, CoreType::Enchantment);
    assert_animated(&outcome, second, own_host, CoreType::Enchantment);
    // CR 303.4e: Aura and host controllers need not be the same.
    assert_eq!(
        outcome
            .state()
            .objects
            .get(&opponent_host)
            .unwrap()
            .controller,
        P1
    );
    assert_eq!(
        outcome.state().objects.get(&own_host).unwrap().controller,
        P0
    );
    assert_eq!(outcome.state().objects.get(&first).unwrap().controller, P0);
}

fn assert_committed(state: &GameState, source: ObjectId, host: ObjectId) {
    assert_eq!(state.objects.get(&source).unwrap().zone, Zone::Stack);
    let entry = state
        .stack
        .iter()
        .find(|entry| entry.source_id == source)
        .expect("committed stack entry");
    // CR 601.2c: the chosen ObjectId is carried by the production cast.
    assert!(entry
        .ability()
        .unwrap()
        .targets
        .contains(&TargetRef::Object(host)));
}

fn assert_attached_alive(state: &GameState, aura: ObjectId, stable: ObjectId) {
    let obj = state.objects.get(&aura).unwrap();
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.attached_to, Some(AttachTarget::Object(stable)));
    assert!(obj.card_types.subtypes.iter().any(|s| s == "Aura"));
    assert!(obj
        .keywords
        .contains(&Keyword::from_str("Enchant:creature").unwrap()));
    let host = state.objects.get(&stable).unwrap();
    assert_eq!(host.zone, Zone::Battlefield);
    assert!(host.attachments.contains(&aura));
}

#[test]
fn puppet_crafting_live_subtype_recheck_preserves_now_illegal_target() {
    let (mut runner, source, host, _) = persistent_board(P0);
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stable = scenario.add_creature(P0, "Stable creature", 2, 3).id();
    let target = scenario
        .add_enchantment_from_oracle(P0, "Live target", "")
        .id();
    scenario.add_artifact_from_oracle(P0, "Second legal host", "");
    let live_source = puppet(&mut scenario);
    mana(&mut scenario, 1, 1);
    let mut live_runner = scenario.build();
    parsed_reach(live_runner.state(), live_source);
    let initial = live_runner.state().objects.get(&target).unwrap();
    assert!(!initial.card_types.subtypes.iter().any(|s| s == "Aura"));
    assert!(!initial.card_types.core_types.contains(&CoreType::Artifact));
    assert!(!initial.card_types.core_types.contains(&CoreType::Creature));
    let mut committed = live_runner.cast(live_source).target_object(target).commit();
    assert_committed(committed.state(), live_source, target);
    assert!(committed.state().priority_passes.is_empty());
    {
        let state = committed.state_mut();
        let obj = state.objects.get_mut(&target).unwrap();
        obj.card_types.subtypes.push("Aura".into());
        obj.base_card_types = obj.card_types.clone();
        let enchant = Keyword::from_str("Enchant:creature").unwrap();
        obj.keywords.push(enchant.clone());
        obj.base_keywords.push(enchant);
        attach_fixture(state, target, stable);
    }
    committed.act(GameAction::PassPriority).unwrap();
    assert_committed(committed.state(), live_source, target);
    assert_attached_alive(committed.state(), target, stable);
    let target_obj = committed.state().objects.get(&target).unwrap();
    assert!(!target_obj
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    assert!(!target_obj
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    let outcome = committed.resolve();
    // CR 608.2b + CR 303.4c: the live Aura exclusion rejects Puppet while
    // the target's own independent legal attachment survives, unanimated.
    outcome.assert_zone(&[live_source], Zone::Graveyard);
    outcome.assert_zone(&[target, stable], Zone::Battlefield);
    assert_attached_alive(outcome.state(), target, stable);
    let target_obj = outcome.state().objects.get(&target).unwrap();
    assert!(target_obj
        .card_types
        .core_types
        .contains(&CoreType::Enchantment));
    assert!(!target_obj
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    assert!(!target_obj
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert!(!target_obj
        .card_types
        .subtypes
        .iter()
        .any(|s| s == "Construct"));
    // Paired unchanged-host control drives the same commit/resolution path.
    let control = runner.cast(source).target_object(host).commit().resolve();
    assert_animated(&control, source, host, CoreType::Enchantment);
}

#[test]
fn puppet_crafting_graveyard_activation_returns_only_its_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut sources = Vec::new();
    for _ in 0..2 {
        sources.push(
            scenario
                .add_spell_to_graveyard(P0, "Puppet Crafting", false)
                .as_enchantment()
                .with_subtypes(vec!["Aura"])
                .from_oracle_text_with_keywords(&["enchant"], PUPPET_ORACLE)
                .id(),
        );
    }
    mana(&mut scenario, 4, 1);
    let mut runner = scenario.build();
    parsed_reach(runner.state(), sources[0]);
    parsed_reach(runner.state(), sources[1]);
    // CR 113.6m + CR 108.4a + CR 602.2: the actual parsed graveyard-only
    // ability is activated by its owner and moves that source to its hand.
    let outcome = runner.activate(sources[0], 0).resolve();
    outcome.assert_zone(&[sources[0]], Zone::Hand);
    outcome.assert_zone(&[sources[1]], Zone::Graveyard);
}

#[test]
fn non_wall_controller_suffix_reaches_real_target_preview() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Own non-Wall", 2, 3).id();
    let other_own = scenario.add_creature(P0, "Second own non-Wall", 2, 3).id();
    let wall = scenario
        .add_creature(P0, "Own Wall", 0, 4)
        .with_subtypes(vec!["Wall"])
        .id();
    let opponent = scenario.add_creature(P1, "Opponent non-Wall", 2, 3).id();
    let source = scenario.add_spell_to_hand(P0, "Krovikan Plague", false)
        .as_enchantment().with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["enchant"], "Enchant non-Wall creature you control\nWhen this Aura enters, draw a card at the beginning of the next turn's upkeep.\nTap enchanted creature: This Aura deals 1 damage to any target. Put a -0/-1 counter on enchanted creature. Activate only if enchanted creature is untapped.")
        .id();
    let mut runner = scenario.build();
    assert!(runner
        .state()
        .objects
        .get(&source)
        .unwrap()
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Enchant(_))));
    let slots = legal_target_slots_for_castable_spell(runner.state(), source);
    assert_eq!(slots.len(), 1);
    // CR 702.5a: the controller suffix and negative subtype both restrict
    // the actual selection set; unrelated Krovikan abilities remain unclaimed.
    assert!(slots[0].legal_targets.contains(&TargetRef::Object(own)));
    assert!(slots[0]
        .legal_targets
        .contains(&TargetRef::Object(other_own)));
    assert!(!slots[0].legal_targets.contains(&TargetRef::Object(wall)));
    assert!(!slots[0]
        .legal_targets
        .contains(&TargetRef::Object(opponent)));
    // Exercise the actual announcement/selection route without claiming
    // support for the sibling's unrelated resolution abilities.
    let committed = runner.cast(source).target_object(own).commit();
    assert_committed(committed.state(), source, own);
}
