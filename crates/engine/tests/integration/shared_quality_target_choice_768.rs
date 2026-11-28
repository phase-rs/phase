//! Announcement of multi-target declarations with a shared-quality restriction.

use std::collections::BTreeSet;

use engine::ai_support::{candidate_actions, legal_actions};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    Effect, FilterProp, SharedQuality, SharedQualityRelation, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::TargetRef;

const UNBURY: &str = "Choose one —\n• Return target creature card from your graveyard to your hand.\n• Return two target creature cards that share a creature type from your graveyard to your hand.";
const SECRET_TUNNEL: &str = "This land can't be blocked.\n{T}: Add {C}.\n{4}, {T}: Two target creatures you control that share a creature type can't be blocked this turn.";
const RIVALS_DUEL: &str = "Choose two target creatures that share no creature types. Those creatures fight each other. (Each deals damage equal to its power to the other.)";
const UNIVERSAL_AUTOMATON: &str = "Changeling (This card is every creature type.)";

fn build(scenario: GameScenario, mana: &[ManaType]) -> GameRunner {
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.all_creature_types = ["Goblin", "Warrior", "Elf", "Druid", "Shapeshifter"]
        .map(str::to_owned)
        .to_vec();
    state.layers_dirty.mark_full();
    evaluate_layers(state);
    for &color in mana {
        state.players[0]
            .mana_pool
            .add(ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
    runner
}

fn cast(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
}

fn choose(runner: &mut GameRunner, target: ObjectId) {
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .unwrap();
}

fn offers(runner: &GameRunner, index: usize, expected: &[ObjectId]) {
    let WaitingFor::TargetSelection {
        player,
        selection: progress,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(progress.current_slot, index);
    let expected = expected
        .iter()
        .copied()
        .map(TargetRef::Object)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        progress
            .current_legal_targets
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        expected
    );
}

fn chosen_objects(actions: impl IntoIterator<Item = GameAction>) -> BTreeSet<ObjectId> {
    actions
        .into_iter()
        .filter_map(|action| match action {
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(id)),
            } => Some(id),
            _ => None,
        })
        .collect()
}

fn unbury(second_goblin: bool) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Unbury", true, UNBURY)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black],
        })
        .id();
    let goblin = scenario
        .add_creature_to_graveyard(P0, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin", "Warrior"])
        .id();
    let other = second_goblin.then(|| {
        scenario
            .add_creature_to_graveyard(P0, "Raging Goblin", 1, 1)
            .with_subtypes(vec!["Goblin"])
            .id()
    });
    let elf = scenario
        .add_creature_to_graveyard(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf", "Druid"])
        .id();
    (
        build(scenario, &[ManaType::Black; 2]),
        spell,
        goblin,
        other,
        elf,
    )
}

fn select_mode(runner: &mut GameRunner, index: usize) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ModeChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::SelectModes {
            indices: vec![index],
        })
        .unwrap();
}

fn resolve_unbury(runner: &mut GameRunner, returned: &[ObjectId], remaining: &[ObjectId]) {
    assert_eq!(runner.state().stack.len(), 1);
    runner.pass_both_players();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    for id in returned {
        assert_eq!(runner.state().objects[id].zone, Zone::Hand);
    }
    for id in remaining {
        assert_eq!(runner.state().objects[id].zone, Zone::Graveyard);
    }
}

#[test]
fn unbury_announces_only_completable_picks_and_ai_candidates() {
    let (mut runner, spell, goblin, other, elf) = unbury(true);
    let other = other.unwrap();
    cast(&mut runner, spell);
    select_mode(&mut runner, 1);
    // CR 601.2c: both targets must form a legal pair.
    offers(&runner, 0, &[goblin, other]);
    assert_eq!(
        chosen_objects(legal_actions(runner.state())),
        BTreeSet::from([goblin, other])
    );
    assert_eq!(
        chosen_objects(
            candidate_actions(runner.state())
                .into_iter()
                .map(|candidate| candidate.action)
        ),
        BTreeSet::from([goblin, other])
    );
    choose(&mut runner, goblin);
    offers(&runner, 1, &[other]);
    assert_eq!(
        chosen_objects(legal_actions(runner.state())),
        BTreeSet::from([other])
    );
    assert_eq!(
        chosen_objects(
            candidate_actions(runner.state())
                .into_iter()
                .map(|candidate| candidate.action)
        ),
        BTreeSet::from([other])
    );
    choose(&mut runner, other);
    resolve_unbury(&mut runner, &[goblin, other], &[elf]);
}

#[test]
fn unbury_checks_bulk_targets() {
    let (mut runner, spell, goblin, other, elf) = unbury(true);
    let other = other.unwrap();
    cast(&mut runner, spell);
    select_mode(&mut runner, 1);
    offers(&runner, 0, &[goblin, other]);

    // CR 601.2c: submitting all targets together obeys the same declaration.
    let submit = |second| GameAction::SelectTargets {
        targets: vec![TargetRef::Object(goblin), TargetRef::Object(second)],
    };
    assert!(runner.act(submit(elf)).is_err());
    offers(&runner, 0, &[goblin, other]);
    runner.act(submit(other)).unwrap();
    resolve_unbury(&mut runner, &[goblin, other], &[elf]);
}

#[test]
fn unbury_excludes_mode_without_a_pair_and_keeps_single_target_mode() {
    let (mut runner, spell, goblin, _, elf) = unbury(false);
    cast(&mut runner, spell);
    let WaitingFor::ModeChoice {
        player,
        unavailable_modes,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Unbury mode choice");
    };
    assert_eq!(*player, P0);
    // CR 601.2c: the mode with no legal pair is unavailable.
    assert_eq!(unavailable_modes, &[1]);
    select_mode(&mut runner, 0);
    offers(&runner, 0, &[goblin, elf]);
    choose(&mut runner, elf);
    resolve_unbury(&mut runner, &[elf], &[goblin]);
}

fn battlefield(scenario: &mut GameScenario) -> [ObjectId; 4] {
    let goblin = scenario
        .add_creature(P0, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin", "Warrior"])
        .id();
    let other = scenario
        .add_creature(P0, "Raging Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let elf = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf", "Druid"])
        .id();
    let changeling = scenario
        .add_creature(P0, "Universal Automaton", 1, 1)
        .with_subtypes(vec!["Shapeshifter"])
        .from_oracle_text_with_keywords(&["Changeling"], UNIVERSAL_AUTOMATON)
        .id();
    [goblin, other, elf, changeling]
}

#[test]
fn secret_tunnel_accepts_shared_types_and_changeling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tunnel = scenario
        .add_land_from_oracle(P0, "Secret Tunnel", SECRET_TUNNEL)
        .id();
    let [goblin, other, elf, changeling] = battlefield(&mut scenario);
    let mut runner = build(scenario, &[ManaType::Colorless; 4]);
    let ability_index = runner.state().objects[&tunnel]
        .abilities
        .iter()
        .position(|ability| ability.multi_target.is_some())
        .expect("Secret Tunnel's two-target activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: tunnel,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    // CR 602.2b + CR 601.2c + CR 702.73a: Changeling can partner with either creature type.
    offers(&runner, 0, &[goblin, other, elf, changeling]);
    choose(&mut runner, goblin);
    offers(&runner, 1, &[other, changeling]);
    choose(&mut runner, changeling);
    assert_eq!(runner.state().stack.len(), 1);
}

#[test]
fn rivals_duel_accepts_disjoint_types_and_excludes_changeling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Rivals' Duel", false, RIVALS_DUEL)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let [goblin, other, elf, _changeling] = battlefield(&mut scenario);
    let mut runner = build(scenario, &[ManaType::Red; 4]);
    cast(&mut runner, spell);
    // CR 601.2c + CR 702.73a: the Changeling shares a type with every other candidate.
    offers(&runner, 0, &[goblin, other, elf]);
    choose(&mut runner, goblin);
    offers(&runner, 1, &[elf]);
    choose(&mut runner, elf);
    assert_eq!(runner.state().stack.len(), 1);
}

const PRISMATIC_WARDROBE: &str = "Destroy target nonartifact, nonland permanent that doesn't share a color with clothing worn by its controller.";
const DEFLECTING_SWAT: &str = "If you control a commander, you may cast this spell without paying its mana cost.\nYou may choose new targets for target spell or ability.";

#[test]
fn prismatic_wardrobe_rejects_a_target_with_no_clothing_reference() {
    use engine::types::mana::ManaColor;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Prismatic Wardrobe", false, PRISMATIC_WARDROBE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let victim = scenario
        .add_creature(P0, "Goblin Piker", 2, 1)
        .with_color(vec![ManaColor::Red])
        .id();
    let mut runner = build(scenario, &[ManaType::White]);

    // Reach guard: one target (no multi-target declaration) whose filter
    // carries the reference-free "doesn't share a color" relation.
    let ability = &runner.state().objects[&spell].abilities[0];
    assert!(ability.multi_target.is_none());
    let Effect::Destroy {
        target: TargetFilter::Typed(filter),
        ..
    } = ability.effect.as_ref()
    else {
        panic!(
            "Wardrobe destroys one typed target, got {:?}",
            ability.effect
        );
    };
    assert!(filter.properties.iter().any(|prop| matches!(
        prop,
        FilterProp::SharesQuality {
            quality: SharedQuality::Color,
            reference: None,
            relation: SharedQualityRelation::DoesNotShare,
        }
    )));

    // A single target has no group to compare, so the unresolved relation
    // admits nothing: the spell isn't offered, and naming the Goblin fails.
    assert!(!legal_actions(runner.state()).iter().any(|action| matches!(
        action,
        GameAction::CastSpell { object_id, .. } if *object_id == spell
    )));
    let card_id = runner.state().objects[&spell].card_id;
    assert!(runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![victim],
            payment_mode: CastPaymentMode::Auto,
        })
        .is_err());
    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn deflecting_swat_checks_the_final_rivals_duel_pair() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let duel = scenario
        .add_spell_to_hand_from_oracle(P0, "Rivals' Duel", false, RIVALS_DUEL)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let swat = scenario
        .add_spell_to_hand_from_oracle(P0, "Deflecting Swat", true, DEFLECTING_SWAT)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let [piker, goblin, elf, _changeling] = battlefield(&mut scenario);
    let mut runner = build(scenario, &[ManaType::Red; 7]);
    cast(&mut runner, duel);
    choose(&mut runner, piker);
    choose(&mut runner, elf);
    let duel_entry = runner.state().stack[0].id;
    runner.cast(swat).target_object(duel_entry).commit();
    runner.pass_both_players();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    let WaitingFor::RetargetChoice {
        player: P0,
        current_targets,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Swat's target-change prompt");
    };
    assert_eq!(
        current_targets,
        &[TargetRef::Object(piker), TargetRef::Object(elf)]
    );

    // CR 115.7d + CR 115.7e: changing one target must preserve the relation
    // between the new target and the target that stays.
    assert!(runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![None, Some(TargetRef::Object(goblin))],
        })
        .is_err());
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(goblin)), None],
        })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        runner.state().stack[0].ability().unwrap().targets,
        [TargetRef::Object(goblin), TargetRef::Object(elf)]
    );

    runner.pass_both_players();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&piker].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&elf].zone, Zone::Graveyard);
}

#[test]
fn deflecting_swat_checks_secret_tunnel_against_departed_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tunnel = scenario
        .add_land_from_oracle(P0, "Secret Tunnel", SECRET_TUNNEL)
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let swat = scenario
        .add_spell_to_hand_from_oracle(P0, "Deflecting Swat", true, DEFLECTING_SWAT)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let departed = scenario
        .add_creature(P0, "Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let survivor = scenario
        .add_creature(P0, "Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    let [replacement, _, elf, _] = battlefield(&mut scenario);
    let mut runner = build(
        scenario,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
        ],
    );
    for token in [departed, survivor] {
        runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;
    }
    let ability_index = runner.state().objects[&tunnel]
        .abilities
        .iter()
        .position(|ability| ability.multi_target.is_some())
        .expect("Secret Tunnel's two-target activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: tunnel,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    choose(&mut runner, departed);
    choose(&mut runner, survivor);
    let tunnel_entry = runner.state().stack[0].id;

    runner.cast(shock).target_object(departed).commit();
    runner.pass_both_players();
    assert_eq!(runner.state().stack.len(), 1);
    assert!(!runner.state().objects.contains_key(&departed));

    runner.cast(swat).target_object(tunnel_entry).commit();
    runner.pass_both_players();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    let WaitingFor::RetargetChoice {
        player: P0,
        current_targets,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("expected Swat's target-change prompt");
    };
    assert_eq!(
        current_targets,
        &[TargetRef::Object(departed), TargetRef::Object(survivor)]
    );

    // CR 115.7d + CR 608.2h: Keep the departed target and compare its
    // last known creature types with the proposed replacement's current types.
    let retarget = |candidate| GameAction::RetargetSpell {
        new_targets: vec![None, Some(TargetRef::Object(candidate))],
    };
    assert!(runner.act(retarget(elf)).is_err(), "Elf replacement");
    runner
        .act(retarget(replacement))
        .expect("Goblin replacement against departed Goblin");
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].id, tunnel_entry);
    assert_eq!(
        runner.state().stack[0].ability().unwrap().targets,
        [TargetRef::Object(departed), TargetRef::Object(replacement)]
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
