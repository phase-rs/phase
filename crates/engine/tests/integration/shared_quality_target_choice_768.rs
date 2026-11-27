//! Announcement of multi-target declarations with a shared-quality restriction.

use std::collections::BTreeSet;

use engine::ai_support::{candidate_actions, legal_actions};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
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
