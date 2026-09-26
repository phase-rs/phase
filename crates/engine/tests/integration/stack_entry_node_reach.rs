//! `effects::stack_reach::stack_entry_node_reach`: each node of a pending
//! stack entry is answered with the objects and players its resolver acts on.

use engine::game::combat::AttackTarget;
use engine::game::effects::stack_reach::stack_entry_node_reach;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, Effect, QuantityExpr, ResolvedAbility, TargetChoiceTiming, TargetFilter,
    TargetRef, TriggerCondition,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SWALLOWED_BY_LEVIATHAN: &str = "Choose target spell. Surveil 2, then counter the chosen spell unless its controller pays {1} for each card in your graveyard. (To surveil 2, look at the top two cards of your library, then put any number of them into your graveyard and the rest on top of your library in any order.)";
const AETHER_SPIKE: &str = "Choose target spell. You get {E}{E} (two energy counters), then you may pay any amount of {E}. Counter that spell unless its controller pays {1} for each {E} paid this way.";
const TAIL_SWIPE: &str = "Choose target creature you control and target creature you don't control. If you cast this spell during your main phase, the creature you control gets +1/+1 until end of turn. Then those creatures fight each other. (Each deals damage equal to its power to the other.)";
const BOON_OF_EREBOS: &str =
    "Target creature gets +2/+0 until end of turn. Regenerate it. You lose 2 life.";
const SELF_DESTRUCT: &str = "Target creature you control deals X damage to any other target and X damage to itself, where X is its power.";
const ARC_TRAIL: &str = "Arc Trail deals 2 damage to any target and 1 damage to any other target.";

/// Every node's `acted_on`, in chain order, for the entry with `id`.
fn reach(state: &GameState, id: ObjectId) -> Vec<(Effect, Vec<TargetRef>, Vec<TargetRef>)> {
    let entry = state.stack.iter().find(|e| e.id == id).expect("entry");
    stack_entry_node_reach(state, entry)
        .into_iter()
        .map(|r| (r.node.effect.clone(), r.node.targets.clone(), r.acted_on))
        .collect()
}

/// A copy of `state` in which each of the first `depth` instructions of
/// `entry`'s chain only holds what it declares (a `TargetOnly` over its own
/// filter, or a `NoOp` where it declares nothing), keeping every node's
/// targets and flags: the nodes below them are read on the board as it stood
/// before the entry resolved.
fn below_inert_instructions(state: &GameState, entry: ObjectId, depth: usize) -> GameState {
    let mut copy = state.clone();
    let mut node = copy
        .stack
        .iter_mut()
        .find(|e| e.id == entry)
        .and_then(StackEntry::ability_mut)
        .expect("entry");
    for _ in 0..depth {
        node.effect = match node.effect.target_filter() {
            Some(target) if !node.targets.is_empty() => Effect::TargetOnly {
                target: target.clone(),
            },
            _ => Effect::NoOp,
        };
        node = node.sub_ability.as_deref_mut().expect("a node below");
    }
    copy
}

fn main_phase(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    let s = runner.state_mut();
    s.turn_number = 3;
    s.active_player = P0;
    s.phase = Phase::PreCombatMain;
    s.waiting_for = WaitingFor::Priority { player: P0 };
    runner
}

/// P0 casts a 2/2 creature spell and passes; P1 casts `text` at it.
fn counter_board(name: &str, text: &str, cost: ManaCost) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario
        .add_creature_to_hand(P0, "Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, name, true, text)
        .with_mana_cost(cost)
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    for _ in 0..3 {
        scenario.add_creature_to_graveyard(P1, "Dead", 1, 1);
    }
    let mut runner = main_phase(scenario);
    runner.cast(bear).commit();
    let bear_spell = runner.state().stack.back().expect("bear spell").id;
    engine::game::engine::apply_as_current_for_simulation(
        runner.state_mut(),
        GameAction::PassPriority,
    )
    .expect("P0 passes");
    runner.cast(spell).target_object(bear_spell).commit();
    let entry = runner.state().stack.back().expect("counter entry").id;
    (runner, bear_spell, entry)
}

fn swallowed_board() -> (GameRunner, ObjectId, ObjectId) {
    counter_board(
        "Swallowed by Leviathan",
        SWALLOWED_BY_LEVIATHAN,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        },
    )
}

/// The index of `entry`'s `Counter` node, after the reach guards: it declares
/// nothing, names no parent anaphor, and follows the surveil.
fn swallowed_counter_index(runner: &GameRunner, entry: ObjectId) -> usize {
    let nodes = reach(runner.state(), entry);
    let index = nodes
        .iter()
        .position(|(e, _, _)| matches!(e, Effect::Counter { .. }))
        .expect("reach guard: the chain counters");
    assert!(
        nodes[index].1.is_empty()
            && matches!(
                nodes[index].0,
                Effect::Counter {
                    target: TargetFilter::Any,
                    ..
                }
            ),
        "reach guard: the counter declares nothing and names no parent anaphor"
    );
    assert!(
        matches!(nodes[index - 1].0, Effect::Surveil { .. }),
        "reach guard: the surveil runs before the counter"
    );
    index
}

#[test]
fn swallowed_by_leviathan_counter_acts_on_the_chosen_spell() {
    let (mut runner, bear_spell, entry) = swallowed_board();
    for _ in 0..2 {
        let state = runner.state_mut();
        let card = CardId(state.next_object_id);
        create_object(state, card, P1, "Top".to_string(), Zone::Library);
    }
    let graveyard = |runner: &GameRunner| runner.state().players[1].graveyard.len();
    assert_eq!(
        graveyard(&runner),
        3,
        "reach guard: before the entry resolves the unless cost counts three cards"
    );
    let counter = swallowed_counter_index(&runner, entry);
    assert_eq!(
        reach(runner.state(), entry)[counter].2,
        vec![TargetRef::Object(bear_spell)]
    );
    runner.resolve_top();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::SurveilChoice { .. }),
        "engine agreement: the surveil runs before the counter"
    );
    runner
        .act(GameAction::SelectCards { cards: Vec::new() })
        .expect("put both cards into the graveyard");
    let now = graveyard(&runner);
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment {
            cost,
            pending_effect,
            ..
        } => assert_eq!(
            (&pending_effect.targets, cost, now > 3),
            (
                &vec![TargetRef::Object(bear_spell)],
                &AbilityCost::Mana {
                    cost: ManaCost::generic(now as u32)
                },
                true
            ),
            "engine agreement: the pending counter names the spell, and its unless cost counts the graveyard the surveil changed"
        ),
        other => panic!("expected the unless-payment counter, got {other:?}"),
    }
}

const JESKAI_REVELATION: &str = "Return target spell or permanent to its owner's hand. Jeskai Revelation deals 4 damage to any target. Create two 1/1 white Monk creature tokens with prowess. Draw two cards. You gain 4 life.";

#[test]
fn a_damage_node_whose_target_an_earlier_instruction_returns_to_hand_acts_on_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let theirs = scenario.add_creature(P1, "Theirs", 5, 5).id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Jeskai Revelation", true, JESKAI_REVELATION)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    for _ in 0..2 {
        let state = runner.state_mut();
        let library_card = CardId(state.next_object_id);
        create_object(state, library_card, P0, "Top".to_string(), Zone::Library);
    }
    runner.cast(card).target_objects(&[theirs, theirs]).commit();
    let entry = runner.state().stack.back().expect("Jeskai Revelation").id;
    let nodes = reach(runner.state(), entry);
    assert!(
        matches!(nodes[0].0, Effect::Bounce { .. })
            && nodes[0].2 == vec![TargetRef::Object(theirs)],
        "reach guard: the first instruction returns the creature"
    );
    let damage = nodes
        .iter()
        .position(|(e, _, _)| matches!(e, Effect::DealDamage { .. }))
        .expect("reach guard: the chain deals damage");
    assert_eq!(
        nodes[damage].1,
        vec![TargetRef::Object(theirs)],
        "reach guard: the damage declares the same creature"
    );
    assert!(nodes[damage].2.is_empty());
    assert_eq!(
        reach(
            &below_inert_instructions(runner.state(), entry, damage),
            entry
        )[damage]
            .2,
        vec![TargetRef::Object(theirs)],
        "reach guard: below an instruction that changes nothing, the damage acts on the creature"
    );
    runner.resolve_top();
    settle_prompts(&mut runner);
    assert_eq!(
        (
            runner.state().objects.get(&theirs).map(|o| o.zone),
            runner.life(P1)
        ),
        (Some(Zone::Hand), 20),
        "engine agreement: the creature is in its owner's hand and its controller loses no life"
    );
}

#[test]
fn aether_spike_counter_acts_on_nothing_while_no_energy_is_paid() {
    let (mut runner, bear_spell, entry) = counter_board(
        "Aether Spike",
        AETHER_SPIKE,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        },
    );
    let nodes = reach(runner.state(), entry);
    let index = nodes
        .iter()
        .position(|(e, _, _)| matches!(e, Effect::Counter { .. }))
        .expect("reach guard: the chain counters");
    assert!(
        index >= 2 && nodes[1..index].iter().all(|(_, own, _)| own.is_empty()),
        "reach guard: the counter sits below untargeted nodes"
    );
    assert!(nodes[index].2.is_empty());
    runner.resolve_top();
    settle_prompts(&mut runner);
    assert_eq!(
        runner.state().objects.get(&bear_spell).map(|o| o.zone),
        Some(Zone::Stack),
        "engine agreement: the spell is not countered"
    );
}

#[test]
fn tail_swipe_fight_acts_on_both_declared_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Own", 3, 3).id();
    let theirs = scenario.add_creature(P1, "Theirs", 4, 4).id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Tail Swipe", true, TAIL_SWIPE)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    runner.cast(card).target_objects(&[own, theirs]).commit();
    let entry = runner.state().stack.back().expect("Tail Swipe").id;
    let nodes = reach(runner.state(), entry);
    let fight = nodes
        .iter()
        .position(|(e, _, _)| matches!(e, Effect::Fight { .. }))
        .expect("reach guard: the chain fights");
    assert!(
        nodes[fight].1.is_empty(),
        "reach guard: the fight declares nothing"
    );
    assert!(
        matches!(nodes[fight - 1].0, Effect::Pump { .. })
            && nodes[fight - 1].2 == vec![TargetRef::Object(own)],
        "reach guard: the pump before the fight acts on the creature you control"
    );
    assert!(nodes[fight].2.contains(&TargetRef::Object(own)));
    assert!(nodes[fight].2.contains(&TargetRef::Object(theirs)));
    runner.resolve_top();
    let zone = |id| runner.state().objects.get(&id).map(|o| o.zone);
    assert_eq!(
        (zone(own), zone(theirs)),
        (Some(Zone::Graveyard), Some(Zone::Graveyard)),
        "engine agreement: both creatures fought"
    );
}

#[test]
fn boon_of_erebos_life_loss_acts_on_its_controller_not_the_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Boon of Erebos", true, BOON_OF_EREBOS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.add_basic_land(P0, ManaColor::Black);
    let mut runner = scenario.build();
    runner.cast(card).target_object(bear).commit();
    let entry = runner.state().stack.back().expect("Boon").id;
    let nodes = reach(runner.state(), entry);
    assert_eq!(
        nodes[0].2,
        vec![TargetRef::Object(bear)],
        "reach guard: the pump acts on the creature"
    );
    let life_loss = nodes
        .iter()
        .position(|(e, _, _)| matches!(e, Effect::LoseLife { .. }))
        .expect("reach guard: the chain loses life");
    assert!(
        nodes[life_loss].1.is_empty()
            && matches!(nodes[life_loss - 1].0, Effect::Regenerate { .. }),
        "reach guard: the life loss declares nothing and follows the regeneration"
    );
    assert_eq!(nodes[life_loss].2, vec![TargetRef::Player(P0)]);
    runner.resolve_top();
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (18, 20),
        "engine agreement: the controller loses the life"
    );
}

#[test]
fn self_destruct_later_node_and_slot_anaphor_each_act_on_their_own_object() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Own", 3, 3).id();
    let other = scenario.add_creature(P1, "Other", 3, 3).id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Self-Destruct", true, SELF_DESTRUCT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let mut runner = scenario.build();
    runner.cast(card).target_objects(&[own, other]).commit();
    let entry = runner.state().stack.back().expect("Self-Destruct").id;
    let nodes = reach(runner.state(), entry);
    assert_eq!(
        nodes[0].1,
        vec![TargetRef::Object(own)],
        "reach guard: root declares own"
    );
    let declared = nodes
        .iter()
        .position(|(_, own_targets, _)| own_targets == &vec![TargetRef::Object(other)])
        .expect("reach guard: a later node declares the other target");
    let slot = nodes
        .iter()
        .position(|(e, _, _)| {
            e.target_filter() == Some(&TargetFilter::ParentTargetSlot { index: 0 })
        })
        .expect("reach guard: the damage to itself names slot 0");
    assert!(
        slot > declared,
        "reach guard: the damage to itself follows the damage to the other target"
    );
    assert_eq!(nodes[declared].2, vec![TargetRef::Object(other)]);
    assert_eq!(nodes[slot].2, vec![TargetRef::Object(own)]);
    runner.resolve_top();
    let zone = |id| runner.state().objects.get(&id).map(|o| o.zone);
    assert_eq!(
        (zone(own), zone(other)),
        (Some(Zone::Graveyard), Some(Zone::Graveyard)),
        "engine agreement: each 3/3 took its 3 damage"
    );
}

/// Arc Trail cast by P0 at two creatures P1 controls, a 3/3 and a 1/1.
fn arc_trail_board(scenario: &mut GameScenario) -> (ObjectId, ObjectId, ObjectId) {
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "First", 3, 3).id();
    let second = scenario.add_creature(P1, "Second", 1, 1).id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Arc Trail", true, ARC_TRAIL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    (first, second, card)
}

#[test]
fn arc_trail_each_damage_node_acts_on_its_own_target() {
    let mut scenario = GameScenario::new();
    let (first, second, card) = arc_trail_board(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(card).target_objects(&[first, second]).commit();
    let entry = runner.state().stack.back().expect("Arc Trail").id;
    let nodes = reach(runner.state(), entry);
    assert_eq!(nodes.len(), 2, "reach guard: two damage nodes");
    assert_eq!(nodes[0].2, vec![TargetRef::Object(first)]);
    assert_eq!(nodes[1].2, vec![TargetRef::Object(second)]);
    runner.resolve_top();
    assert_eq!(
        (
            runner.state().objects[&first].damage_marked,
            runner.state().objects.get(&second).map(|o| o.zone)
        ),
        (2, Some(Zone::Graveyard)),
        "engine agreement: 2 damage to the first creature and 1 to the second"
    );
}

const FOG: &str = "Prevent all combat damage that would be dealt this turn.";

#[test]
fn a_damage_node_below_a_change_acts_on_nothing_while_a_damage_replacement_is_on_the_board() {
    for replacement in ["Pariah", "Fog"] {
        let mut scenario = GameScenario::new();
        let (first, second, card) = arc_trail_board(&mut scenario);
        let host = scenario.add_creature(P0, "Host", 2, 2).id();
        let pariah = (replacement == "Pariah").then(|| {
            scenario
                .add_enchantment_from_oracle(P0, "Pariah", PARIAH)
                .id()
        });
        let fog = scenario
            .add_spell_to_hand_from_oracle(P0, "Fog", true, FOG)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        match pariah {
            Some(pariah) => runner.attach_as_bestowed_aura(pariah, host),
            None => {
                runner.cast(fog).commit();
                runner.resolve_top();
                assert!(
                    !runner.state().pending_damage_replacements.is_empty(),
                    "reach guard: Fog's shield waits for combat damage"
                );
            }
        }
        runner.cast(card).target_objects(&[first, second]).commit();
        let entry = runner.state().stack.back().expect("Arc Trail").id;
        let nodes = reach(runner.state(), entry);
        assert_eq!(
            nodes[0].2,
            vec![TargetRef::Object(first)],
            "reach guard, {replacement}: no replacement applies to the first damage now"
        );
        assert!(nodes[1].2.is_empty(), "{replacement}");
    }
}

fn creature(state: &mut GameState, owner: PlayerId) -> ObjectId {
    let card = CardId(state.next_object_id);
    let id = create_object(
        state,
        card,
        owner,
        "Creature".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(2);
    obj.toughness = Some(2);
    id
}

fn push_spell(state: &mut GameState, controller: PlayerId, root: ResolvedAbility) -> ObjectId {
    let id = root.source_id;
    state.stack.push_back(StackEntry {
        id,
        source_id: id,
        controller,
        kind: StackEntryKind::Spell {
            ability: Some(Box::new(root)),
            card_id: CardId(id.0),
            casting_variant: Default::default(),
            actual_mana_spent: 0,
        },
    });
    id
}

fn spell_object(state: &mut GameState, controller: PlayerId) -> ObjectId {
    let card = CardId(state.next_object_id);
    create_object(state, card, controller, "Spell".to_string(), Zone::Stack)
}

fn node(effect: Effect, targets: Vec<TargetRef>, source: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(effect, targets, source, PlayerId(1))
}

fn destroy(target: TargetFilter) -> Effect {
    Effect::Destroy {
        target,
        cant_regenerate: false,
    }
}

#[test]
fn a_node_that_declares_its_own_target_acts_only_on_it() {
    let mut state = GameState::new_two_player(42);
    let aimed = creature(&mut state, PlayerId(0));
    let harmed = creature(&mut state, PlayerId(0));
    let source = spell_object(&mut state, PlayerId(1));
    let mut root = node(
        Effect::TargetOnly {
            target: TargetFilter::Any,
        },
        vec![TargetRef::Object(aimed)],
        source,
    );
    root.sub_ability = Some(Box::new(node(
        destroy(TargetFilter::Any),
        vec![TargetRef::Object(harmed)],
        source,
    )));
    let entry = push_spell(&mut state, PlayerId(1), root);
    let nodes = reach(&state, entry);
    assert_eq!(
        nodes[0].1,
        vec![TargetRef::Object(aimed)],
        "reach guard: root aims at one"
    );
    assert!(nodes[0].2.is_empty());
    assert_eq!(nodes[1].2, vec![TargetRef::Object(harmed)]);
}

#[test]
fn an_undeclared_child_acts_on_the_target_the_engine_hands_it() {
    for filter in [TargetFilter::ParentTarget, TargetFilter::Any] {
        let mut state = GameState::new_two_player(42);
        let aimed = creature(&mut state, PlayerId(0));
        let source = spell_object(&mut state, PlayerId(1));
        let mut root = node(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(aimed)],
            source,
        );
        root.sub_ability = Some(Box::new(node(destroy(filter.clone()), Vec::new(), source)));
        let entry = push_spell(&mut state, PlayerId(1), root);
        let nodes = reach(&state, entry);
        assert!(
            nodes[1].1.is_empty(),
            "reach guard: the destroy declares nothing"
        );
        assert_eq!(
            nodes[1].2,
            vec![TargetRef::Object(aimed)],
            "filter {filter:?}"
        );
    }
}

/// A `TargetOnly` root aimed at `creature`, then a `NoOp` whose target choice
/// happens at `timing`, then a destroy naming the parent target.
fn choice_then_destroy(
    creature: ObjectId,
    source: ObjectId,
    timing: TargetChoiceTiming,
) -> ResolvedAbility {
    let mut middle = node(Effect::NoOp, Vec::new(), source);
    middle.target_choice_timing = timing;
    middle.sub_ability = Some(Box::new(node(
        destroy(TargetFilter::ParentTarget),
        Vec::new(),
        source,
    )));
    let mut root = node(
        Effect::TargetOnly {
            target: TargetFilter::Any,
        },
        vec![TargetRef::Object(creature)],
        source,
    );
    root.sub_ability = Some(Box::new(middle));
    root
}

#[test]
fn an_undeclared_child_acts_on_nothing_where_the_two_hand_offs_differ() {
    for (timing, handed) in [
        (TargetChoiceTiming::Stack, true),
        (TargetChoiceTiming::Resolution, false),
    ] {
        let mut state = GameState::new_two_player(42);
        let target = creature(&mut state, PlayerId(0));
        let source = spell_object(&mut state, PlayerId(1));
        let entry = push_spell(
            &mut state,
            PlayerId(1),
            choice_then_destroy(target, source, timing),
        );
        let nodes = reach(&state, entry);
        assert_eq!(
            nodes[2].2.contains(&TargetRef::Object(target)),
            handed,
            "timing {timing:?}"
        );
        let runner = resolve_raw(state);
        assert_eq!(
            runner.state().objects[&target].zone,
            Zone::Graveyard,
            "engine agreement, timing {timing:?}: the destroy is handed the creature"
        );
    }
}

#[test]
fn an_entry_below_a_stack_object_does_not_act_on_it() {
    let mut state = GameState::new_two_player(42);
    let spell = spell_object(&mut state, PlayerId(0));
    push_spell(
        &mut state,
        PlayerId(0),
        node(Effect::NoOp, Vec::new(), spell),
    );
    let counter_source = spell_object(&mut state, PlayerId(1));
    let counter = push_spell(
        &mut state,
        PlayerId(1),
        node(
            counter(TargetFilter::StackSpell),
            vec![TargetRef::Object(spell)],
            counter_source,
        ),
    );
    assert_eq!(
        reach(&state, counter)[0].2,
        vec![TargetRef::Object(spell)],
        "control: above the spell, the counter acts on it"
    );
    state.stack.swap(0, 1);
    assert_eq!(
        state.stack.back().map(|e| e.id),
        Some(spell),
        "reach guard: the spell now resolves first"
    );
    assert!(reach(&state, counter)[0].2.is_empty());
}

#[test]
fn a_node_naming_a_set_its_own_resolution_produces_names_nothing_yet() {
    use engine::types::identifiers::TrackedSetId;
    let tap: Effect = serde_json::from_str(r#"{"type":"SetTapState","target":{"type":"TrackedSet","id":0},"scope":{"type":"Single"},"state":{"type":"Tap"}}"#).unwrap();
    let mut state = GameState::new_two_player(42);
    let aimed = creature(&mut state, PlayerId(0));
    let bystander = creature(&mut state, PlayerId(0));
    state
        .tracked_object_sets
        .insert(TrackedSetId(7), vec![bystander]);
    let source = spell_object(&mut state, PlayerId(1));
    let tap_parent_target: Effect = serde_json::from_str(r#"{"type":"SetTapState","target":{"type":"ParentTarget"},"scope":{"type":"Single"},"state":{"type":"Tap"}}"#).unwrap();
    let mut answers = Vec::new();
    for child in [tap_parent_target, tap] {
        let mut state = state.clone();
        let mut root = node(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(aimed)],
            source,
        );
        root.sub_ability = Some(Box::new(node(child, Vec::new(), source)));
        let entry = push_spell(&mut state, PlayerId(1), root);
        answers.push(reach(&state, entry)[1].2.clone());
    }
    assert_eq!(
        answers[0],
        vec![TargetRef::Object(aimed)],
        "reach guard: a tap below the choice is answered"
    );
    assert!(
        !state.tracked_object_sets.is_empty(),
        "reach guard: an earlier resolution's set is still published"
    );
    assert!(answers[1].is_empty());
}

const OFF_BALANCE: &str = "Target creature can't attack or block this turn.";

#[test]
fn off_balance_acts_on_the_creature_it_declares() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P1, "Creature", 2, 2).id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Off Balance", true, OFF_BALANCE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();
    runner.cast(card).target_object(creature).commit();
    let entry = runner.state().stack.back().expect("Off Balance").id;
    let nodes = reach(runner.state(), entry);
    assert!(
        matches!(nodes[0].0, Effect::GenericEffect { .. }),
        "reach guard: a single granted-restriction node"
    );
    assert_eq!(nodes[0].2, vec![TargetRef::Object(creature)]);
}

/// One parsed single-target effect per routed kind, as `data/card-data.json`
/// stores it for the named card.
const DECLARED_TARGET_EFFECTS: &[(&str, &str)] = &[
    (
        "Shock",
        r#"{"type":"DealDamage","amount":{"type":"Fixed","value":2},"target":{"type":"Any"}}"#,
    ),
    (
        "Murder",
        r#"{"type":"Destroy","target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]},"cant_regenerate":false}"#,
    ),
    (
        "Unsummon",
        r#"{"type":"Bounce","target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]},"destination":null}"#,
    ),
    (
        "Giant Growth",
        r#"{"type":"Pump","power":{"type":"Fixed","value":3},"toughness":{"type":"Fixed","value":3},"target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]}}"#,
    ),
    (
        "Bulk Up",
        r#"{"type":"DoublePT","mode":"Power","target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]},"factor":2}"#,
    ),
    (
        "Swords to Plowshares",
        r#"{"type":"ChangeZone","origin":null,"destination":"Exile","target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]},"owner_library":false,"enter_transformed":false,"enter_tapped":false,"enters_attacking":false}"#,
    ),
    (
        "A Good Day to Pie",
        r#"{"type":"SetTapState","target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]},"scope":{"type":"Single"},"state":{"type":"Tap"}}"#,
    ),
    (
        "Battlegrowth",
        r#"{"type":"PutCounter","counter_type":"P1P1","count":{"type":"Fixed","value":1},"target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]}}"#,
    ),
    (
        "Heartless Act",
        r#"{"type":"RemoveCounter","counter_type":null,"count":{"type":"Fixed","value":3},"target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]}}"#,
    ),
    (
        "The Five Stages of Grief",
        r#"{"type":"Goad","target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]}}"#,
    ),
    (
        "Culling Mark",
        r#"{"type":"ForceBlock","target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]},"duration":"UntilEndOfTurn"}"#,
    ),
    (
        "March of Swirling Mist",
        r#"{"type":"PhaseOut","target":{"type":"Typed","type_filters":["Creature"],"controller":null,"properties":[]}}"#,
    ),
];

#[test]
fn every_routed_single_target_effect_acts_on_its_declared_target() {
    for (card, json) in DECLARED_TARGET_EFFECTS {
        let effect: Effect = serde_json::from_str(json).expect("parsed effect");
        let mut state = GameState::new_two_player(42);
        let aimed = creature(&mut state, PlayerId(0));
        let bystander = creature(&mut state, PlayerId(0));
        let source = spell_object(&mut state, PlayerId(1));
        let entry = push_spell(
            &mut state,
            PlayerId(1),
            node(effect, vec![TargetRef::Object(aimed)], source),
        );
        assert!(
            state.objects.contains_key(&bystander),
            "hostile fixture: a second creature a population read would also name"
        );
        assert_eq!(
            reach(&state, entry)[0].2,
            vec![TargetRef::Object(aimed)],
            "{card}"
        );
    }
}

#[test]
fn brain_freeze_mills_the_player_it_declares() {
    let mill: Effect = serde_json::from_str(
        r#"{"type":"Mill","count":{"type":"Fixed","value":3},"target":{"type":"Player"},"destination":"Graveyard"}"#,
    )
    .expect("parsed effect");
    let mut state = GameState::new_two_player(42);
    let source = spell_object(&mut state, PlayerId(1));
    let entry = push_spell(
        &mut state,
        PlayerId(1),
        node(mill, vec![TargetRef::Player(PlayerId(0))], source),
    );
    assert_eq!(
        reach(&state, entry)[0].2,
        vec![TargetRef::Player(PlayerId(0))]
    );
}

const TEFERIS_PROTECTION: &str = "Until your next turn, your life total can't change and you gain protection from everything. All permanents you control phase out. (While they're phased out, they're treated as though they don't exist. They phase in before you untap during your untap step.)\nExile Teferi's Protection.";

#[test]
fn teferis_protection_later_nodes_act_on_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Theirs", 2, 2).id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Teferi's Protection", true, TEFERIS_PROTECTION)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let land = scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();
    runner.cast(card).commit();
    let entry = runner.state().stack.back().expect("Teferi's Protection").id;
    let nodes = reach(runner.state(), entry);
    let exile = nodes
        .iter()
        .position(|(e, _, _)| {
            matches!(
                e,
                Effect::ChangeZone {
                    target: TargetFilter::SelfRef,
                    destination: Zone::Exile,
                    ..
                }
            )
        })
        .expect("reach guard: the chain exiles itself");
    assert!(
        nodes[exile].1.is_empty(),
        "reach guard: the exile node declares nothing"
    );
    let phase_out = nodes
        .iter()
        .position(|(e, _, _)| matches!(e, Effect::PhaseOut { .. }))
        .expect("reach guard: the chain phases out");
    assert!(
        phase_out > 0 && nodes[phase_out].2.is_empty() && nodes[exile].2.is_empty(),
        "both nodes follow an instruction whose changes this authority does not bound"
    );
    let inert = |index: usize| {
        reach(
            &below_inert_instructions(runner.state(), entry, index),
            entry,
        )[index]
            .2
            .clone()
    };
    let phased = inert(phase_out);
    assert!(
        phased.contains(&TargetRef::Object(mine))
            && phased.contains(&TargetRef::Object(land))
            && !phased.contains(&TargetRef::Object(theirs)),
        "reach guard: below instructions that change nothing, the phase-out acts on your permanents"
    );
    assert_eq!(
        inert(exile),
        vec![TargetRef::Object(entry)],
        "reach guard: below instructions that change nothing, the exile acts on its own spell"
    );
    runner.resolve_top();
    assert_eq!(
        runner.state().objects.get(&entry).map(|o| o.zone),
        Some(Zone::Exile),
        "engine agreement: the spell exiled itself (an under-report)"
    );
}

const SHEOLDRED: &str = "Deathtouch\nWhenever you draw a card, you gain 2 life.\nWhenever an opponent draws a card, they lose 2 life.";
const UNDERWORLD_DREAMS: &str =
    "Whenever an opponent draws a card, this enchantment deals 1 damage to that player.";
const CHANCELLOR_OF_THE_ANNEX: &str = "You may reveal this card from your opening hand. If you do, when each opponent casts their first spell of the game, counter that spell unless that player pays {1}.\nFlying\nWhenever an opponent casts a spell, counter it unless that player pays {1}.";
const COUNTERBALANCE: &str = "Whenever an opponent casts a spell, you may reveal the top card of your library. If you do, counter that spell if it has the same mana value as the revealed card.";
const MEMORY_EROSION: &str = "Whenever an opponent casts a spell, that player mills two cards.";
const LEECHING_SLIVER: &str =
    "Whenever a Sliver you control attacks, defending player loses 1 life.";

/// Hands priority to P1 in its own main phase.
fn p1_main_phase(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    let s = runner.state_mut();
    s.turn_number = 3;
    s.active_player = P1;
    s.phase = Phase::PreCombatMain;
    s.waiting_for = WaitingFor::Priority { player: P1 };
    s.priority_player = P1;
    runner
}

/// Passes priority until a triggered ability of `source` is the top entry.
fn trigger_of(runner: &mut GameRunner, source: ObjectId) -> ObjectId {
    for _ in 0..8 {
        if let Some(top) = runner.state().stack.back() {
            if top.source_id == source
                && matches!(top.kind, StackEntryKind::TriggeredAbility { .. })
            {
                return top.id;
            }
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("no trigger of {source:?} reached the top of the stack");
}

/// P0 controls `source`; P1 casts "Draw a card." and `source` triggers.
fn opponent_draw_trigger(source_text: &str, enchantment: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = if enchantment {
        scenario.add_enchantment_from_oracle(P0, "Source", source_text)
    } else {
        scenario.add_creature_from_oracle(P0, "Source", 4, 5, source_text)
    }
    .id();
    let cantrip = scenario
        .add_spell_to_hand_from_oracle(P1, "Cantrip", true, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.add_basic_land(P1, ManaColor::Blue);
    scenario.add_card_to_library_top(P1, "Drawn");
    let mut runner = p1_main_phase(scenario);
    runner.cast(cantrip).commit();
    let entry = trigger_of(&mut runner, source);
    (runner, entry)
}

/// The one node of a trigger that names the player who drew, after the reach
/// guards: it declares nothing, P0 controls it, and no trigger event is bound
/// while it waits on the stack.
fn drawing_player_node(runner: &GameRunner, entry: ObjectId) -> Vec<TargetRef> {
    let state = runner.state();
    let on_stack = state.stack.iter().find(|e| e.id == entry).expect("entry");
    assert_eq!(
        on_stack.controller, P0,
        "reach guard: P0 controls the trigger"
    );
    assert!(
        state.current_trigger_event.is_none(),
        "reach guard: no trigger event is bound while the trigger waits"
    );
    let nodes = reach(state, entry);
    assert_eq!(nodes.len(), 1, "reach guard: a single node");
    assert!(
        nodes[0].1.is_empty(),
        "reach guard: the node declares nothing"
    );
    nodes[0].2.clone()
}

#[test]
fn sheoldred_life_loss_acts_on_the_player_who_drew() {
    let (mut runner, entry) = opponent_draw_trigger(SHEOLDRED, false);
    assert_eq!(
        drawing_player_node(&runner, entry),
        vec![TargetRef::Player(P1)]
    );
    runner.resolve_top();
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (20, 18),
        "engine agreement: the player who drew loses 2 life"
    );
}

#[test]
fn underworld_dreams_damage_acts_on_the_player_who_drew() {
    let (mut runner, entry) = opponent_draw_trigger(UNDERWORLD_DREAMS, true);
    assert_eq!(
        drawing_player_node(&runner, entry),
        vec![TargetRef::Player(P1)]
    );
    runner.resolve_top();
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (20, 19),
        "engine agreement: the player who drew is dealt 1 damage"
    );
}

/// P0 controls `source`; P1 casts a 2/2 creature spell with mana value 2 and
/// `source` triggers. P0's library top has mana value 2.
fn opponent_cast_trigger(
    source_text: &str,
    enchantment: bool,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = if enchantment {
        scenario.add_enchantment_from_oracle(P0, "Source", source_text)
    } else {
        scenario.add_creature_from_oracle(P0, "Source", 5, 6, source_text)
    }
    .id();
    let bear = scenario
        .add_creature_to_hand(P1, "Bear", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    scenario.add_card_to_library_top(P1, "Milled A");
    scenario.add_card_to_library_top(P1, "Milled B");
    scenario
        .add_spell_to_library_top(P0, "Revealed", true)
        .with_mana_cost(ManaCost::generic(2));
    let mut runner = p1_main_phase(scenario);
    runner.cast(bear).commit();
    let entry = trigger_of(&mut runner, source);
    (runner, source, bear, entry)
}

/// Accepts each "may" and declines each "unless" payment the resolution asks for.
fn settle_prompts(runner: &mut GameRunner) {
    for _ in 0..6 {
        let action = match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::UnlessPayment { .. } => GameAction::PayUnlessCost { pay: false },
            _ => return,
        };
        runner.act(action).expect("answer the prompt");
    }
}

/// The trigger's `Counter` node, after the reach guards: it declares nothing
/// and names the parent anaphor, whose fallback without an event is the source.
fn counter_node(runner: &GameRunner, entry: ObjectId) -> Vec<TargetRef> {
    let nodes = reach(runner.state(), entry);
    let (_, own, acted) = nodes
        .iter()
        .find(|(e, _, _)| {
            matches!(
                e,
                Effect::Counter {
                    target: TargetFilter::ParentTarget,
                    ..
                }
            )
        })
        .expect("reach guard: the trigger counters its parent referent");
    assert!(own.is_empty(), "reach guard: the counter declares nothing");
    acted.clone()
}

#[test]
fn chancellor_of_the_annex_counter_acts_on_the_spell_that_triggered_it() {
    let (mut runner, source, bear, entry) = opponent_cast_trigger(CHANCELLOR_OF_THE_ANNEX, false);
    assert_eq!(counter_node(&runner, entry), vec![TargetRef::Object(bear)]);
    runner.resolve_top();
    settle_prompts(&mut runner);
    let zone = |id| runner.state().objects.get(&id).map(|o| o.zone);
    assert_eq!(
        (zone(bear), zone(source)),
        (Some(Zone::Graveyard), Some(Zone::Battlefield)),
        "engine agreement: the unpaid spell is countered and the source stays"
    );
}

#[test]
fn counterbalance_counter_below_its_reveal_acts_on_nothing() {
    let (mut runner, source, bear, entry) = opponent_cast_trigger(COUNTERBALANCE, true);
    assert!(counter_node(&runner, entry).is_empty());
    runner.resolve_top();
    settle_prompts(&mut runner);
    let zone = |id| runner.state().objects.get(&id).map(|o| o.zone);
    assert_eq!(
        (zone(bear), zone(source)),
        (Some(Zone::Graveyard), Some(Zone::Battlefield)),
        "engine agreement: the spell is countered, which the authority under-reports"
    );
}

#[test]
fn memory_erosion_mill_acts_on_the_player_who_cast() {
    let (mut runner, _, _, entry) = opponent_cast_trigger(MEMORY_EROSION, true);
    let nodes = reach(runner.state(), entry);
    assert_eq!(nodes.len(), 1, "reach guard: a single node");
    assert!(
        matches!(nodes[0].0, Effect::Mill { .. }) && nodes[0].1.is_empty(),
        "reach guard: an undeclared mill"
    );
    assert_eq!(nodes[0].2, vec![TargetRef::Player(P1)]);
    let library = runner.state().players[1].library.len();
    runner.resolve_top();
    assert_eq!(
        runner.state().players[1].library.len() + 2,
        library,
        "engine agreement: the player who cast mills two"
    );
}

#[test]
fn leeching_sliver_life_loss_acts_on_the_player_another_sliver_attacks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler"]);
    scenario.with_library_top(P1, &["Filler"]);
    let attacker = scenario
        .add_creature(P0, "Other Sliver", 2, 2)
        .with_subtypes(vec!["Sliver"])
        .id();
    let leeching = scenario
        .add_creature_from_oracle(P0, "Leeching Sliver", 1, 1, LEECHING_SLIVER)
        .with_subtypes(vec!["Sliver"])
        .id();
    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    let entry = runner
        .state()
        .stack
        .back()
        .filter(|e| e.source_id == leeching)
        .expect("reach guard: the Leeching Sliver trigger is on top")
        .id;
    let nodes = reach(runner.state(), entry);
    assert!(
        matches!(
            nodes[0].0,
            Effect::LoseLife {
                target: Some(TargetFilter::DefendingPlayer),
                ..
            }
        ),
        "reach guard: the defending-player anaphor"
    );
    assert_eq!(nodes[0].2, vec![TargetRef::Player(P1)]);
    runner.resolve_top();
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (20, 19),
        "engine agreement: the defending player loses 1 life"
    );
}

/// A triggered entry from `source` with `effect` and the given trigger event.
fn push_trigger(
    state: &mut GameState,
    id: u64,
    source: ObjectId,
    effect: Effect,
    trigger_event: Option<GameEvent>,
) -> ObjectId {
    state.stack.push_back(StackEntry {
        id: ObjectId(id),
        source_id: source,
        controller: PlayerId(1),
        kind: StackEntryKind::TriggeredAbility {
            source_id: source,
            ability: Box::new(node(effect, Vec::new(), source)),
            condition: None,
            trigger_event,
            description: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
            provenance: None,
        },
    });
    ObjectId(id)
}

fn spell_cast(spell: ObjectId) -> GameEvent {
    GameEvent::SpellCast {
        card_id: CardId(spell.0),
        controller: PlayerId(0),
        object_id: spell,
        cast_mana_value: None,
    }
}

fn counter(target: TargetFilter) -> Effect {
    Effect::Counter {
        target,
        source_rider: None,
        countered_spell_zone: None,
    }
}

#[test]
fn a_pending_trigger_reads_its_own_trigger_event_not_the_one_resolving() {
    let mut state = GameState::new_two_player(42);
    let own_spell = spell_object(&mut state, PlayerId(0));
    push_spell(
        &mut state,
        PlayerId(0),
        node(Effect::NoOp, Vec::new(), own_spell),
    );
    let other_spell = spell_object(&mut state, PlayerId(0));
    push_spell(
        &mut state,
        PlayerId(0),
        node(Effect::NoOp, Vec::new(), other_spell),
    );
    let source = creature(&mut state, PlayerId(1));
    let parent = push_trigger(
        &mut state,
        960,
        source,
        counter(TargetFilter::ParentTarget),
        Some(spell_cast(own_spell)),
    );
    let subject = push_trigger(
        &mut state,
        961,
        source,
        counter(TargetFilter::TriggeringSource),
        Some(spell_cast(own_spell)),
    );
    let eventless = push_trigger(
        &mut state,
        962,
        source,
        counter(TargetFilter::TriggeringSource),
        None,
    );
    state.current_trigger_event = Some(spell_cast(other_spell));
    assert_eq!(
        reach(&state, parent)[0].2,
        vec![TargetRef::Object(own_spell)]
    );
    assert_eq!(
        reach(&state, subject)[0].2,
        vec![TargetRef::Object(own_spell)]
    );
    assert!(
        reach(&state, eventless)[0].2.is_empty(),
        "a trigger without an event of its own does not read the one resolving"
    );
}

#[test]
fn a_trigger_whose_intervening_if_is_false_acts_on_nothing() {
    // The drawing player needs the trigger event; the controller does not, so
    // it is answered even when no event is bound.
    for (target, acted) in [
        ("TriggeringPlayer", PlayerId(0)),
        ("Controller", PlayerId(1)),
    ] {
        let mut state = GameState::new_two_player(42);
        let source = creature(&mut state, PlayerId(0));
        let drew = GameEvent::CardDrawn {
            player_id: PlayerId(0),
            object_id: source,
            nth_in_turn: 1,
            nth_in_step: 1,
        };
        let lose_life = effect(serde_json::json!({
            "type": "LoseLife",
            "amount": {"type": "Fixed", "value": 2},
            "target": {"type": target},
        }));
        let entry = push_trigger(&mut state, 970, source, lose_life, Some(drew));
        assert_eq!(
            reach(&state, entry)[0].2,
            vec![TargetRef::Player(acted)],
            "control: with no condition the {target} node acts on {acted:?}"
        );
        if let Some(StackEntryKind::TriggeredAbility { condition, .. }) =
            state.stack.back_mut().map(|e| &mut e.kind)
        {
            *condition = Some(TriggerCondition::SourceAttackedThisCombat);
        }
        assert!(reach(&state, entry)[0].2.is_empty(), "{target}");
    }
}

const ASSASSINATE: &str = "Destroy target tapped creature.";

#[test]
fn a_declared_target_that_became_illegal_is_not_acted_on() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Assassinate", false, ASSASSINATE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.add_basic_land(P0, ManaColor::Black);
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&bear).unwrap().tapped = true;
    runner.cast(card).target_object(bear).commit();
    let entry = runner.state().stack.back().expect("Assassinate").id;
    assert_eq!(
        reach(runner.state(), entry)[0].2,
        vec![TargetRef::Object(bear)],
        "control: while the creature is tapped the spell acts on it"
    );
    runner.state_mut().objects.get_mut(&bear).unwrap().tapped = false;
    assert!(reach(runner.state(), entry)[0].2.is_empty());
    runner.resolve_top();
    assert_eq!(
        runner.state().objects.get(&bear).map(|o| o.zone),
        Some(Zone::Battlefield),
        "engine agreement: the untapped creature is not destroyed"
    );
}

const COMMANDEER: &str = "You may exile two blue cards from your hand rather than pay this spell's mana cost.\nGain control of target noncreature spell. You may choose new targets for it. (If that spell is an artifact, enchantment, or planeswalker, the permanent enters under your control.)";

#[test]
fn a_spell_whose_control_changed_on_the_stack_acts_on_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let boon = scenario
        .add_spell_to_hand_from_oracle(P0, "Boon of Erebos", true, BOON_OF_EREBOS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.add_basic_land(P0, ManaColor::Black);
    let commandeer = scenario
        .add_spell_to_hand_from_oracle(P1, "Commandeer", true, COMMANDEER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.add_basic_land(P1, ManaColor::Blue);
    let mut runner = scenario.build();
    runner.cast(boon).target_object(bear).commit();
    let entry = runner.state().stack.back().expect("Boon of Erebos").id;
    assert!(
        reach(runner.state(), entry)
            .iter()
            .any(|(e, _, acted)| matches!(e, Effect::LoseLife { .. })
                && acted == &vec![TargetRef::Player(P0)]),
        "control: before the steal the life loss acts on its caster"
    );
    engine::game::engine::apply_as_current_for_simulation(
        runner.state_mut(),
        GameAction::PassPriority,
    )
    .expect("P0 passes");
    runner.cast(commandeer).target_object(entry).commit();
    runner.resolve_top();
    while matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("keep the targets");
    }
    let stolen = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == entry)
        .expect("reach guard: the spell is still on the stack");
    assert_eq!(
        (
            stolen.controller,
            runner.state().objects.get(&entry).map(|o| o.controller)
        ),
        (P0, Some(P1)),
        "reach guard: P1 controls the spell its entry records P0 cast"
    );
    assert!(reach(runner.state(), entry)
        .iter()
        .all(|(_, _, acted)| acted.is_empty()));
    runner.resolve_top();
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (20, 18),
        "engine agreement: the new controller loses the life"
    );
}

#[test]
fn a_slot_reads_the_root_of_its_own_entry() {
    let mut state = GameState::new_two_player(42);
    let first = creature(&mut state, PlayerId(0));
    let second = creature(&mut state, PlayerId(0));
    let source = creature(&mut state, PlayerId(1));
    let damage = |target: serde_json::Value| -> Effect {
        serde_json::from_value(serde_json::json!({
            "type": "DealDamage",
            "amount": {"type": "Fixed", "value": 1},
            "target": target,
        }))
        .expect("parsed effect")
    };
    for (aimed, id) in [(first, 900), (second, 901)] {
        let mut root = node(
            damage(serde_json::json!({"type": "Any"})),
            vec![TargetRef::Object(aimed)],
            source,
        );
        root.sub_ability = Some(Box::new(node(
            damage(serde_json::json!({"type": "ParentTargetSlot", "index": 0})),
            Vec::new(),
            source,
        )));
        state.stack.push_back(StackEntry {
            id: ObjectId(id),
            source_id: source,
            controller: PlayerId(1),
            kind: StackEntryKind::ActivatedAbility {
                source_id: source,
                ability: Box::new(root),
            },
        });
    }
    assert_eq!(
        reach(&state, ObjectId(900))[1].2,
        vec![TargetRef::Object(first)],
        "reach guard: the lower entry from the same source reads its own slot"
    );
    assert_eq!(
        reach(&state, ObjectId(901))[1].2,
        vec![TargetRef::Object(second)]
    );
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.priority_player = PlayerId(0);
    let mut runner = GameRunner::from_state(state);
    runner.resolve_top();
    let damage_on = |id| runner.state().objects[&id].damage_marked;
    assert_eq!(
        (damage_on(first), damage_on(second)),
        (0, 2),
        "engine agreement: both nodes of the upper entry hit its own target"
    );
}

#[test]
fn a_batched_trigger_acts_on_every_subject_of_its_batch() {
    let mut state = GameState::new_two_player(42);
    let first = creature(&mut state, PlayerId(1));
    let second = creature(&mut state, PlayerId(1));
    let source = creature(&mut state, PlayerId(1));
    let counter_on_each: Effect = serde_json::from_str(
        r#"{"type":"PutCounter","counter_type":"P1P1","count":{"type":"Fixed","value":1},"target":{"type":"TriggeringSource"}}"#,
    )
    .expect("parsed effect");
    let entry = push_trigger(
        &mut state,
        980,
        source,
        counter_on_each,
        Some(spell_cast(first)),
    );
    state
        .stack_trigger_event_batches
        .insert(entry, vec![spell_cast(first), spell_cast(second)]);
    assert_eq!(
        reach(&state, entry)[0].2,
        vec![TargetRef::Object(first), TargetRef::Object(second)]
    );
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.priority_player = PlayerId(0);
    let mut runner = GameRunner::from_state(state);
    runner.resolve_top();
    let counters_on = |id| runner.state().objects[&id].counters.values().sum::<u32>();
    assert_eq!(
        (counters_on(first), counters_on(second)),
        (1, 1),
        "engine agreement: each subject of the batch gets a counter"
    );
}

const ENTROPIC_BATTLECRUISER: &str = "Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 8+.)\n1+ | Whenever an opponent discards a card, they lose 3 life.\n8+ | Flying, deathtouch\nWhenever this Spacecraft attacks, each opponent discards a card. Each opponent who can't loses 3 life.";
const LAQUATUS_CHAMPION: &str = "When this creature enters, target player loses 6 life.\nWhen this creature leaves the battlefield, that player gains 6 life.\n{B}: Regenerate this creature.";

fn effect(json: serde_json::Value) -> Effect {
    serde_json::from_value(json).expect("parsed effect")
}

/// P0 holds priority on `state`, and the top entry resolves.
fn resolve_raw(mut state: GameState) -> GameRunner {
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state.priority_player = PlayerId(0);
    let mut runner = GameRunner::from_state(state);
    runner.resolve_top();
    runner
}

fn scoped_life_loss() -> Effect {
    effect(serde_json::json!({
        "type": "LoseLife",
        "amount": {"type": "Fixed", "value": 2},
        "target": {"type": "ScopedPlayer"},
    }))
}

#[test]
fn a_damage_trigger_reads_the_damaged_player_through_its_scoped_player() {
    let mut state = GameState::new_two_player(42);
    let source = creature(&mut state, PlayerId(1));
    let entry = push_trigger(
        &mut state,
        990,
        source,
        scoped_life_loss(),
        Some(GameEvent::DamageDealt {
            source_id: source,
            target: TargetRef::Player(PlayerId(0)),
            amount: 2,
            is_combat: true,
            excess: 0,
        }),
    );
    let on_stack = state.stack.back().expect("entry");
    assert!(
        on_stack.controller == PlayerId(1)
            && on_stack
                .ability()
                .is_some_and(|a| a.scoped_player.is_none()),
        "reach guard: P1 controls the trigger and no scoped player is stored on it"
    );
    assert_eq!(
        reach(&state, entry)[0].2,
        vec![TargetRef::Player(PlayerId(0))]
    );
    let runner = resolve_raw(state);
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (18, 20),
        "engine agreement: the damaged player loses 2 life"
    );
}

#[test]
fn a_spell_with_a_lone_player_target_reads_it_through_its_scoped_player() {
    let mut state = GameState::new_two_player(42);
    let graveyard_card = {
        let card = CardId(state.next_object_id);
        create_object(
            &mut state,
            card,
            PlayerId(0),
            "Gone".to_string(),
            Zone::Graveyard,
        )
    };
    let source = spell_object(&mut state, PlayerId(1));
    let mut life_loss = node(scoped_life_loss(), Vec::new(), source);
    life_loss.sub_ability = Some(Box::new(node(
        effect(serde_json::json!({
            "type": "ChangeZoneAll",
            "origin": "Graveyard",
            "destination": "Exile",
            "target": {"type": "Typed", "type_filters": [], "controller": null, "properties": [
                {"type": "Owned", "controller": "ScopedPlayer"},
                {"type": "InZone", "zone": "Graveyard"}
            ]},
        })),
        Vec::new(),
        source,
    )));
    let mut root = node(
        effect(serde_json::json!({
            "type": "TargetOnly",
            "target": {"type": "Typed", "type_filters": [], "controller": "Opponent", "properties": []},
        })),
        vec![TargetRef::Player(PlayerId(0))],
        source,
    );
    root.sub_ability = Some(Box::new(life_loss));
    let entry = push_spell(&mut state, PlayerId(1), root);
    let nodes = reach(&state, entry);
    assert!(
        matches!(nodes[1].0, Effect::LoseLife { .. }) && nodes[1].1.is_empty(),
        "reach guard: the life loss declares nothing"
    );
    assert_eq!(nodes[1].2, vec![TargetRef::Player(PlayerId(0))]);
    let runner = resolve_raw(state);
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (18, 20),
        "engine agreement: the targeted player loses 2 life"
    );
    assert_eq!(
        runner.state().objects[&graveyard_card].zone,
        Zone::Exile,
        "engine agreement: the targeted player's graveyard is exiled"
    );
}

#[test]
fn a_trigger_whose_parent_target_was_not_seeded_reads_its_event_referent() {
    // Holding only its own source, the fallback the resolution-time seeding
    // overwrites, and not seeded at all. `Sacrifice` is answered with the targets
    // the node holds, `Destroy` through its resolver's binding.
    let sacrifice = effect(serde_json::json!({
        "type": "Sacrifice",
        "target": {"type": "ParentTarget"},
        "count": {"type": "Fixed", "value": 1},
    }));
    for (removal, held_source) in [
        (destroy(TargetFilter::ParentTarget), true),
        (destroy(TargetFilter::ParentTarget), false),
        (sacrifice.clone(), true),
        (sacrifice, false),
    ] {
        let mut state = GameState::new_two_player(42);
        let spacecraft = creature(&mut state, PlayerId(1));
        let stationed = creature(&mut state, PlayerId(1));
        let source = creature(&mut state, PlayerId(1));
        let entry = push_trigger(
            &mut state,
            991,
            source,
            removal.clone(),
            Some(GameEvent::Stationed {
                spacecraft_id: spacecraft,
                creature_id: stationed,
                counters_added: 2,
            }),
        );
        let held = if held_source {
            vec![TargetRef::Object(source)]
        } else {
            Vec::new()
        };
        if let Some(root) = state.stack.back_mut().and_then(StackEntry::ability_mut) {
            root.targets = held.clone();
        }
        let nodes = reach(&state, entry);
        assert_eq!(
            nodes[0].1, held,
            "reach guard: the stationed creature was not seeded when the trigger was put on the stack"
        );
        assert_eq!(
            nodes[0].2,
            vec![TargetRef::Object(stationed)],
            "{removal:?}, held source: {held_source}"
        );
        let runner = resolve_raw(state);
        let zone = |id| runner.state().objects[&id].zone;
        assert_eq!(
            (zone(stationed), zone(spacecraft), zone(source)),
            (Zone::Graveyard, Zone::Battlefield, Zone::Battlefield),
            "engine agreement: the stationed creature is removed ({removal:?}, held source: {held_source})"
        );
    }
}

#[test]
fn a_node_with_no_referent_bound_acts_on_nothing() {
    let mut state = GameState::new_two_player(42);
    let below = spell_object(&mut state, PlayerId(0));
    push_spell(
        &mut state,
        PlayerId(0),
        node(Effect::NoOp, Vec::new(), below),
    );
    let own = spell_object(&mut state, PlayerId(1));
    let entry = push_spell(
        &mut state,
        PlayerId(1),
        node(counter(TargetFilter::ParentTarget), Vec::new(), own),
    );
    assert!(
        state.current_trigger_event.is_none() && reach(&state, entry)[0].1.is_empty(),
        "reach guard: the counter declares nothing and no trigger event is bound"
    );
    assert!(reach(&state, entry)[0].2.is_empty());
    let source = creature(&mut state, PlayerId(1));
    let eventless = push_trigger(
        &mut state,
        992,
        source,
        effect(serde_json::json!({
            "type": "LoseLife",
            "amount": {"type": "Fixed", "value": 2},
            "target": {"type": "TriggeringPlayer"},
        })),
        None,
    );
    assert!(
        reach(&state, eventless)[0].2.is_empty(),
        "a trigger with no event answers nothing"
    );
    let runner = resolve_raw(state.clone());
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (20, 18),
        "the engine's fallback is the trigger's controller, which the node does not name"
    );
    state.stack.pop_back();
    let runner = resolve_raw(state);
    assert!(
        runner.state().stack.iter().any(|e| e.id == below),
        "engine agreement: the counter counters nothing"
    );
}

#[test]
fn entropic_battlecruiser_each_opponent_nodes_act_on_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler"]);
    scenario.with_library_top(P1, &["Filler"]);
    let ship = scenario
        .add_creature_from_oracle(P0, "Entropic Battlecruiser", 3, 10, ENTROPIC_BATTLECRUISER)
        .id();
    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .declare_attackers(&[(ship, AttackTarget::Player(P1))])
        .expect("declare attackers");
    let entry = runner
        .state()
        .stack
        .back()
        .filter(|e| e.source_id == ship)
        .expect("reach guard: the attack trigger is on top");
    assert!(
        entry.ability().is_some_and(|a| a.player_scope.is_some())
            && runner.state().players[1].hand.is_empty(),
        "reach guard: the root is repeated for each opponent, and P1 cannot discard"
    );
    let nodes = reach(runner.state(), entry.id);
    assert!(
        nodes.iter().any(|(e, _, _)| matches!(
            e,
            Effect::LoseLife {
                target: Some(TargetFilter::ScopedPlayer),
                ..
            }
        )),
        "reach guard: the life loss names the scoped player"
    );
    assert!(nodes.iter().all(|(_, _, acted)| acted.is_empty()));
    runner.resolve_top();
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (20, 17),
        "engine agreement: the opponent loses 3 life, not the controller"
    );
}

#[test]
fn a_node_above_a_player_scope_still_acts_on_its_target() {
    let mut state = GameState::new_two_player(42);
    let aimed = creature(&mut state, PlayerId(0));
    let source = spell_object(&mut state, PlayerId(1));
    let mut each = node(scoped_life_loss(), Vec::new(), source);
    each.player_scope = Some(
        serde_json::from_value(serde_json::json!({"type": "Opponent"})).expect("player filter"),
    );
    let mut root = node(
        destroy(TargetFilter::Any),
        vec![TargetRef::Object(aimed)],
        source,
    );
    root.sub_ability = Some(Box::new(each.clone()));
    let mut above = state.clone();
    let entry = push_spell(&mut above, PlayerId(1), root);
    let nodes = reach(&above, entry);
    assert_eq!(nodes[0].2, vec![TargetRef::Object(aimed)]);
    assert!(nodes[1].2.is_empty());
    // Below a choice, which leaves the board as it was, only the scope
    // leaves the node unanswered.
    for scoped in [false, true] {
        let mut below = state.clone();
        let mut child = each.clone();
        if !scoped {
            child.player_scope = None;
        }
        let mut root = node(
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(aimed)],
            source,
        );
        root.sub_ability = Some(Box::new(child));
        let entry = push_spell(&mut below, PlayerId(1), root);
        let answer = reach(&below, entry)[1].2.clone();
        if scoped {
            assert!(answer.is_empty(), "the node repeats per opponent");
        } else {
            assert!(
                !answer.is_empty(),
                "reach guard: the same node without its scope is answered"
            );
        }
    }
}

#[test]
fn laquatus_champion_life_loss_acts_on_nothing_until_its_target_is_chosen() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let champion = scenario
        .add_creature_to_hand_from_oracle(P0, "Laquatus's Champion", 6, 3, LAQUATUS_CHAMPION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    scenario.add_basic_land(P0, ManaColor::Black);
    let mut runner = main_phase(scenario);
    runner.cast(champion).commit();
    for _ in 0..4 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    let entry = runner.state().stack.back().expect("entry").id;
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) && runner.state().pending_trigger_entry == Some(entry),
        "reach guard: the trigger is on the stack while its target is chosen"
    );
    assert!(reach(runner.state(), entry)[0].2.is_empty());
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choose P1");
    assert_eq!(
        reach(runner.state(), entry)[0].2,
        vec![TargetRef::Player(P1)]
    );
    runner.resolve_top();
    assert_eq!(
        (runner.life(P0), runner.life(P1)),
        (20, 14),
        "engine agreement: the chosen player loses 6 life"
    );
}

// ---------------------------------------------------------------------------
// Checks the chain resolver makes before an instruction runs.
// ---------------------------------------------------------------------------

fn condition(json: serde_json::Value) -> engine::types::ability::AbilityCondition {
    serde_json::from_value(json).expect("parsed condition")
}

fn creature_condition() -> engine::types::ability::AbilityCondition {
    condition(serde_json::json!({
        "type": "TargetMatchesFilter",
        "filter": {"type": "Typed", "type_filters": ["Creature"], "controller": null, "properties": []},
        "use_lki": false,
    }))
}

fn exile(target: TargetFilter) -> Effect {
    effect(serde_json::json!({
        "type": "ChangeZone",
        "origin": null,
        "destination": "Exile",
        "target": target,
        "owner_library": false,
        "enter_transformed": false,
        "enter_tapped": false,
        "enters_attacking": false,
    }))
}

const SOUL_REND: &str = "Destroy target creature if it's white. A creature destroyed this way can't be regenerated.\nDraw a card at the beginning of the next turn's upkeep.";

#[test]
fn soul_rend_destroy_node_acts_only_on_a_creature_its_condition_admits() {
    for white in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let color = if white {
            ManaColor::White
        } else {
            ManaColor::Green
        };
        let bear = scenario
            .add_creature(P1, "Bear", 2, 2)
            .with_color(vec![color])
            .id();
        let card = scenario
            .add_spell_to_hand_from_oracle(P0, "Soul Rend", true, SOUL_REND)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 1,
            })
            .id();
        for _ in 0..2 {
            scenario.add_basic_land(P0, ManaColor::Black);
        }
        let mut runner = main_phase(scenario);
        runner.cast(card).target_object(bear).commit();
        let entry = runner.state().stack.back().expect("Soul Rend");
        assert!(
            entry.ability().is_some_and(|root| root.condition.is_some()),
            "reach guard: the destroy carries its own condition"
        );
        let entry = entry.id;
        let nodes = reach(runner.state(), entry);
        assert!(
            matches!(nodes[0].0, Effect::Destroy { .. })
                && nodes[0].1 == vec![TargetRef::Object(bear)],
            "reach guard: the destroy is aimed at the bear"
        );
        let expected = if white {
            vec![TargetRef::Object(bear)]
        } else {
            Vec::new()
        };
        assert_eq!(nodes[0].2, expected, "white: {white}");
        runner.resolve_top();
        let expected_zone = if white {
            Zone::Graveyard
        } else {
            Zone::Battlefield
        };
        assert_eq!(
            runner.state().objects.get(&bear).map(|o| o.zone),
            Some(expected_zone),
            "engine agreement, white: {white}"
        );
    }
}

#[test]
fn tail_swipe_cast_outside_a_main_phase_answers_its_pump_and_fight_with_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    let own = scenario.add_creature(P0, "Own", 3, 3).id();
    let theirs = scenario.add_creature(P1, "Theirs", 4, 4).id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Tail Swipe", true, TAIL_SWIPE)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    runner.cast(card).target_objects(&[own, theirs]).commit();
    let entry = runner.state().stack.back().expect("Tail Swipe").id;
    let nodes = reach(runner.state(), entry);
    let pump = nodes
        .iter()
        .find(|(e, _, _)| matches!(e, Effect::Pump { .. }))
        .expect("reach guard: the chain pumps");
    let fight = nodes
        .iter()
        .find(|(e, _, _)| matches!(e, Effect::Fight { .. }))
        .expect("reach guard: the chain fights");
    assert!(
        pump.2.is_empty(),
        "the pump's cast-phase condition is false"
    );
    assert!(
        fight.2.is_empty(),
        "nothing below a false condition is answered"
    );
    runner.resolve_top();
    assert_eq!(
        runner.state().objects.get(&own).map(|o| o.zone),
        Some(Zone::Graveyard),
        "engine agreement: the creatures fight anyway, unpumped"
    );
}

const MOLTEN_RAIN: &str = "Destroy target land. If that land was nonbasic, Molten Rain deals 2 damage to the land's controller.";

#[test]
fn molten_rain_damage_node_whose_condition_reads_the_board_acts_on_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_land_from_oracle(P1, "Waste", "").id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Molten Rain", false, MOLTEN_RAIN)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 1,
        })
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let mut runner = main_phase(scenario);
    runner.cast(card).target_object(land).commit();
    let entry = runner.state().stack.back().expect("Molten Rain").id;
    let nodes = reach(runner.state(), entry);
    assert_eq!(
        nodes[0].2,
        vec![TargetRef::Object(land)],
        "reach guard: the destroy is answered"
    );
    let on_stack = runner
        .state()
        .stack
        .back()
        .and_then(|e| e.ability())
        .expect("root");
    assert!(
        matches!(nodes[1].0, Effect::DealDamage { .. })
            && on_stack
                .sub_ability
                .as_deref()
                .is_some_and(|sub| sub.condition.is_some()),
        "reach guard: the damage is gated on the land"
    );
    assert!(nodes[1].2.is_empty());
    assert!(
        reach(&below_inert_instructions(runner.state(), entry, 1), entry)[1]
            .2
            .is_empty(),
        "below an instruction that changes nothing, the condition is still not evaluated"
    );
    runner.resolve_top();
    assert_eq!(
        runner.life(P1),
        18,
        "engine agreement: the land's controller takes 2, which the authority under-reports"
    );
}

const BURST_LIGHTNING: &str = "Kicker {4} (You may pay an additional {4} as you cast this spell.)\nBurst Lightning deals 2 damage to any target. If this spell was kicked, it deals 4 damage instead.";

#[test]
fn burst_lightning_answers_the_damage_node_its_kicker_selects() {
    for kicked in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let big = scenario.add_creature(P1, "Big", 5, 5).id();
        let card = scenario
            .add_spell_to_hand_from_oracle(P0, "Burst Lightning", true, BURST_LIGHTNING)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .id();
        for _ in 0..5 {
            scenario.add_basic_land(P0, ManaColor::Red);
        }
        let mut runner = main_phase(scenario);
        let cast = runner.cast(card).target_object(big);
        if kicked {
            cast.accept_optional().commit();
        } else {
            cast.decline_optional().commit();
        }
        let entry = runner.state().stack.back().expect("Burst Lightning").id;
        let nodes = reach(runner.state(), entry);
        assert!(
            nodes.len() == 2
                && nodes
                    .iter()
                    .all(|(e, _, _)| matches!(e, Effect::DealDamage { .. })),
            "reach guard: the damage node and its kicked replacement"
        );
        let aimed = vec![TargetRef::Object(big)];
        let (base, instead) = if kicked {
            (Vec::new(), aimed)
        } else {
            (aimed, Vec::new())
        };
        assert_eq!(nodes[0].2, base, "kicked: {kicked}");
        assert_eq!(nodes[1].2, instead, "kicked: {kicked}");
        runner.resolve_top();
        assert_eq!(
            runner.state().objects.get(&big).map(|o| o.damage_marked),
            Some(if kicked { 4 } else { 2 }),
            "engine agreement, kicked: {kicked}"
        );
    }
}

#[test]
fn an_else_branch_acts_on_nothing() {
    let mut state = GameState::new_two_player(42);
    let aimed = creature(&mut state, PlayerId(0));
    let source = spell_object(&mut state, PlayerId(1));
    let mut root = node(
        Effect::TargetOnly {
            target: TargetFilter::Any,
        },
        vec![TargetRef::Object(aimed)],
        source,
    );
    root.condition = Some(creature_condition());
    root.sub_ability = Some(Box::new(node(
        destroy(TargetFilter::ParentTarget),
        Vec::new(),
        source,
    )));
    root.else_ability = Some(Box::new(node(
        exile(TargetFilter::ParentTarget),
        Vec::new(),
        source,
    )));
    let entry = push_spell(&mut state, PlayerId(1), root);
    let nodes = reach(&state, entry);
    assert!(
        matches!(nodes[1].0, Effect::Destroy { .. })
            && nodes[1].2 == vec![TargetRef::Object(aimed)],
        "reach guard: the condition holds and the destroy below it is answered"
    );
    assert!(matches!(nodes[2].0, Effect::ChangeZone { .. }) && nodes[2].2.is_empty());
    let runner = resolve_raw(state);
    assert_eq!(
        runner.state().objects[&aimed].zone,
        Zone::Graveyard,
        "engine agreement: destroyed, not exiled"
    );
}

#[test]
fn a_node_over_any_number_of_target_players_acts_on_nothing() {
    for chosen in [Vec::new(), vec![PlayerId(0), PlayerId(1)]] {
        let mut state = GameState::new_two_player(42);
        for player in [PlayerId(0), PlayerId(1)] {
            for _ in 0..3 {
                let card = CardId(state.next_object_id);
                create_object(&mut state, card, player, "Card".to_string(), Zone::Library);
            }
        }
        let source = spell_object(&mut state, PlayerId(1));
        // Court of Cunning's upkeep trigger, as `data/card-data.json` parses it.
        let mut root = node(
            effect(serde_json::json!({
                "type": "Mill",
                "count": {"type": "Fixed", "value": 2},
                "target": {"type": "Player"},
                "destination": "Graveyard",
            })),
            chosen.iter().map(|p| TargetRef::Player(*p)).collect(),
            source,
        );
        root.multi_target =
            Some(serde_json::from_value(serde_json::json!({"min": 0, "max": null})).unwrap());
        let entry = push_spell(&mut state, PlayerId(1), root);
        assert!(reach(&state, entry)[0].2.is_empty(), "chosen: {chosen:?}");
        let runner = resolve_raw(state);
        let milled = |p: usize| 3 - runner.state().players[p].library.len();
        let expected = if chosen.is_empty() { (0, 0) } else { (2, 2) };
        assert_eq!(
            (milled(0), milled(1)),
            expected,
            "engine agreement: each chosen player mills, and none when none is chosen"
        );
    }
}

#[test]
fn a_repeated_node_acts_on_nothing() {
    let mut state = GameState::new_two_player(42);
    let aimed = creature(&mut state, PlayerId(0));
    let source = spell_object(&mut state, PlayerId(1));
    let mut root = node(
        effect(serde_json::json!({
            "type": "DealDamage",
            "amount": {"type": "Fixed", "value": 1},
            "target": {"type": "Any"},
        })),
        vec![TargetRef::Object(aimed)],
        source,
    );
    root.repeat_for = Some(QuantityExpr::Fixed { value: 0 });
    let entry = push_spell(&mut state, PlayerId(1), root);
    assert!(reach(&state, entry)[0].2.is_empty());
    let runner = resolve_raw(state);
    assert_eq!(
        runner.state().objects[&aimed].damage_marked,
        0,
        "engine agreement: repeated zero times"
    );
}

const EMBERSMITH: &str = "Whenever you cast an artifact spell, you may pay {1}. If you do, this creature deals 1 damage to any target.";

#[test]
fn embersmith_damage_node_below_its_payment_acts_on_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let smith = scenario
        .add_creature_from_oracle(P0, "Embersmith", 2, 1, EMBERSMITH)
        .id();
    let foe = scenario.add_creature(P1, "Foe", 3, 3).id();
    let trinket = scenario
        .add_artifact_to_hand_from_oracle(P0, "Trinket", "")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let mut runner = main_phase(scenario);
    runner.cast(trinket).target_object(foe).commit();
    let entry = runner
        .state()
        .stack
        .back()
        .filter(|e| e.source_id == smith)
        .expect("reach guard: Embersmith's trigger is on top")
        .id;
    let nodes = reach(runner.state(), entry);
    assert!(
        runner
            .state()
            .stack
            .back()
            .and_then(|e| e.ability())
            .is_some_and(|root| root.optional),
        "reach guard: the payment is a \"you may\""
    );
    let damage = nodes
        .iter()
        .position(|(e, _, _)| matches!(e, Effect::DealDamage { .. }))
        .expect("reach guard: the chain deals damage");
    assert!(nodes[damage].2.is_empty());
    assert_eq!(
        reach(
            &below_inert_instructions(runner.state(), entry, damage),
            entry
        )[damage]
            .2,
        vec![TargetRef::Object(foe)],
        "reach guard: below a \"you may\" that changes nothing, the \"if you do\" node acts as if the player does"
    );
    runner.resolve_top();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "engine agreement: whether the node is performed is the player's answer"
    );
}

const CLASH_OF_WILLS: &str = "Counter target spell unless its controller pays {X}.";

#[test]
fn clash_of_wills_with_x_zero_counters_nothing() {
    for x in [0, 1] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bear = scenario
            .add_creature_to_hand(P0, "Bear", 2, 2)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        for _ in 0..2 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let clash = scenario
            .add_spell_to_hand_from_oracle(P1, "Clash of Wills", true, CLASH_OF_WILLS)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Blue],
                generic: 0,
            })
            .id();
        for _ in 0..2 {
            scenario.add_basic_land(P1, ManaColor::Blue);
        }
        let mut runner = main_phase(scenario);
        runner.cast(bear).commit();
        let bear_spell = runner.state().stack.back().expect("bear spell").id;
        engine::game::engine::apply_as_current_for_simulation(
            runner.state_mut(),
            GameAction::PassPriority,
        )
        .expect("P0 passes");
        runner.cast(clash).x(x).target_object(bear_spell).commit();
        let entry = runner.state().stack.back().expect("Clash of Wills").id;
        let expected = if x == 0 {
            Vec::new()
        } else {
            vec![TargetRef::Object(bear_spell)]
        };
        assert_eq!(reach(runner.state(), entry)[0].2, expected, "X = {x}");
        runner.resolve_top();
        let asked = matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. });
        assert_eq!(
            (
                asked,
                runner.state().objects.get(&bear_spell).map(|o| o.zone)
            ),
            if x == 0 {
                (false, Some(Zone::Stack))
            } else {
                (true, Some(Zone::Stack))
            },
            "engine agreement, X = {x}: at X = 0 the spell stays unasked"
        );
    }
}

#[test]
fn a_group_that_no_longer_shares_its_quality_is_not_acted_on() {
    for share in [true, false] {
        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec!["Elf".to_string(), "Goblin".to_string()];
        let first = creature(&mut state, PlayerId(1));
        let second = creature(&mut state, PlayerId(1));
        for (id, subtype) in [
            (first, "Elf"),
            (second, if share { "Elf" } else { "Goblin" }),
        ] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .subtypes
                .push(subtype.to_string());
        }
        let source = spell_object(&mut state, PlayerId(1));
        // Secret Tunnel's group constraint, on a destroy so the outcome is visible.
        let root = node(
            effect(serde_json::json!({
                "type": "Destroy",
                "target": {"type": "Typed", "type_filters": ["Creature"], "controller": null,
                    "properties": [{"type": "SharesQuality", "quality": "CreatureType"}]},
                "cant_regenerate": false,
            })),
            vec![TargetRef::Object(first), TargetRef::Object(second)],
            source,
        );
        let entry = push_spell(&mut state, PlayerId(1), root);
        let expected = if share {
            vec![TargetRef::Object(first), TargetRef::Object(second)]
        } else {
            Vec::new()
        };
        assert_eq!(reach(&state, entry)[0].2, expected, "share: {share}");
        let runner = resolve_raw(state);
        let expected_zone = if share {
            Zone::Graveyard
        } else {
            Zone::Battlefield
        };
        assert_eq!(
            runner.state().objects[&first].zone,
            expected_zone,
            "engine agreement, share: {share}"
        );
    }
}

const CRUEL_REVIVAL: &str = "Destroy target non-Zombie creature. It can't be regenerated. Return up to one target Zombie card from your graveyard to your hand.";

#[test]
fn cruel_revival_return_node_does_not_act_on_the_destroyed_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Cruel Revival", true, CRUEL_REVIVAL)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.add_basic_land(P0, ManaColor::Black);
    let mut runner = main_phase(scenario);
    runner.cast(card).target_object(victim).commit();
    let entry = runner.state().stack.back().expect("Cruel Revival").id;
    let nodes = reach(runner.state(), entry);
    assert_eq!(
        nodes[0].2,
        vec![TargetRef::Object(victim)],
        "reach guard: the destroy is answered"
    );
    assert!(
        matches!(nodes[1].0, Effect::ChangeZone { .. }) && nodes[1].1.is_empty(),
        "reach guard: no Zombie card was chosen for the return"
    );
    assert!(nodes[1].2.is_empty());
    assert!(
        reach(&below_inert_instructions(runner.state(), entry, 1), entry)[1]
            .2
            .is_empty(),
        "below an instruction that changes nothing, the return is still not handed the creature"
    );
    runner.resolve_top();
    assert_eq!(
        runner.state().objects.get(&victim).map(|o| o.zone),
        Some(Zone::Graveyard),
        "engine agreement: the destroyed creature is not returned"
    );
}

#[test]
fn a_child_whose_targets_are_set_during_resolution_acts_on_nothing() {
    // A creature card returned from a graveyard becomes the child's source.
    let mut state = GameState::new_two_player(42);
    let card = {
        let id = CardId(state.next_object_id);
        let obj = create_object(
            &mut state,
            id,
            PlayerId(1),
            "Returned".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        obj
    };
    let source = spell_object(&mut state, PlayerId(1));
    let mut root = node(
        effect(serde_json::json!({
            "type": "ChangeZone", "origin": "Graveyard", "destination": "Battlefield",
            "target": {"type": "Typed", "type_filters": ["Creature"], "controller": null,
                "properties": [{"type": "InZone", "zone": "Graveyard"}]},
            "owner_library": false, "enter_transformed": false,
            "enter_tapped": false, "enters_attacking": false,
        })),
        vec![TargetRef::Object(card)],
        source,
    );
    root.forward_result = true;
    root.sub_ability = Some(Box::new(node(
        destroy(TargetFilter::SelfRef),
        Vec::new(),
        source,
    )));
    let entry = push_spell(&mut state, PlayerId(1), root);
    let nodes = reach(&state, entry);
    assert_eq!(
        nodes[0].2,
        vec![TargetRef::Object(card)],
        "reach guard: the return is answered"
    );
    assert!(nodes[1].2.is_empty(), "forward_result");

    // A choice that forwards its result hands its child nothing now.
    let mut state = GameState::new_two_player(42);
    let aimed = creature(&mut state, PlayerId(0));
    let source = spell_object(&mut state, PlayerId(1));
    let mut root = node(
        Effect::TargetOnly {
            target: TargetFilter::Any,
        },
        vec![TargetRef::Object(aimed)],
        source,
    );
    root.forward_result = true;
    root.sub_ability = Some(Box::new(node(
        destroy(TargetFilter::ParentTarget),
        Vec::new(),
        source,
    )));
    let entry = push_spell(&mut state, PlayerId(1), root);
    assert!(
        reach(&state, entry)[1].2.is_empty(),
        "forward_result below a choice"
    );
    let runner = resolve_raw(state);
    assert_eq!(
        runner.state().objects[&aimed].zone,
        Zone::Battlefield,
        "engine agreement: the forwarding choice hands the destroy nothing"
    );

    // A shield's rider runs once per prevented event, not now.
    let mut state = GameState::new_two_player(42);
    let shielded = creature(&mut state, PlayerId(1));
    let source = spell_object(&mut state, PlayerId(1));
    let mut root = node(
        effect(serde_json::json!({
            "type": "PreventDamage", "amount": "All",
            "target": {"type": "Any"}, "scope": "AllDamage",
        })),
        vec![TargetRef::Object(shielded)],
        source,
    );
    let mut rider = node(
        effect(serde_json::json!({
            "type": "PutCounter", "counter_type": "P1P1",
            "count": {"type": "Fixed", "value": 1}, "target": {"type": "ParentTarget"},
        })),
        Vec::new(),
        source,
    );
    rider.sub_link = engine::types::ability::SubAbilityLink::ContinuationStep;
    root.sub_ability = Some(Box::new(rider));
    let entry = push_spell(&mut state, PlayerId(1), root);
    assert!(reach(&state, entry)[1].2.is_empty(), "shield rider");

    // A multi-source damage child: its sources are the parent's objects.
    let mut state = GameState::new_two_player(42);
    let not_a_creature = {
        let id = CardId(state.next_object_id);
        create_object(
            &mut state,
            id,
            PlayerId(1),
            "Rock".to_string(),
            Zone::Battlefield,
        )
    };
    let recipient = creature(&mut state, PlayerId(0));
    let source = spell_object(&mut state, PlayerId(1));
    let mut root = node(
        effect(serde_json::json!({"type": "TargetOnly", "target": {"type": "Any"}})),
        vec![TargetRef::Object(not_a_creature)],
        source,
    );
    root.sub_ability = Some(Box::new(node(
        effect(serde_json::json!({
            "type": "DealDamage",
            "amount": {"type": "Ref", "qty": {"type": "Power", "scope": {"type": "Target"}}},
            "target": {"type": "Any"}, "damage_source": "EachTarget",
        })),
        vec![TargetRef::Object(recipient)],
        source,
    )));
    let entry = push_spell(&mut state, PlayerId(1), root);
    assert!(reach(&state, entry)[1].2.is_empty(), "each-target sources");
    let runner = resolve_raw(state);
    assert_eq!(
        runner.state().objects[&recipient].damage_marked,
        0,
        "engine agreement: a source that is not a creature deals nothing"
    );
}

const PARIAH: &str = "Enchant creature\nAll damage that would be dealt to you is dealt to enchanted creature instead.";
const SHOCK: &str = "Shock deals 2 damage to any target.";

#[test]
fn pariah_moves_damage_off_its_controller_so_the_damage_node_acts_on_nothing() {
    for with_pariah in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Host", 3, 3).id();
        let pariah = with_pariah.then(|| {
            scenario
                .add_enchantment_from_oracle(P0, "Pariah", PARIAH)
                .id()
        });
        let shock = scenario
            .add_spell_to_hand_from_oracle(P1, "Shock", true, SHOCK)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .id();
        scenario.add_basic_land(P1, ManaColor::Red);
        let mut runner = p1_main_phase(scenario);
        if let Some(pariah) = pariah {
            runner.attach_as_bestowed_aura(pariah, host);
        }
        runner.cast(shock).target_player(P0).commit();
        let entry = runner.state().stack.back().expect("Shock").id;
        let expected = if with_pariah {
            Vec::new()
        } else {
            vec![TargetRef::Player(P0)]
        };
        assert_eq!(
            reach(runner.state(), entry)[0].2,
            expected,
            "Pariah: {with_pariah}"
        );
        runner.resolve_top();
        let outcome = (runner.life(P0), runner.state().objects[&host].damage_marked);
        assert_eq!(
            outcome,
            if with_pariah { (20, 2) } else { (18, 0) },
            "engine agreement, Pariah: {with_pariah}"
        );
    }
}

#[test]
fn a_counter_whose_target_is_the_source_of_a_later_entry_acts_on_nothing() {
    for trigger_above in [false, true] {
        let mut state = GameState::new_two_player(42);
        let spell = spell_object(&mut state, PlayerId(0));
        push_spell(
            &mut state,
            PlayerId(0),
            node(Effect::NoOp, Vec::new(), spell),
        );
        if trigger_above {
            push_trigger(&mut state, 900, spell, Effect::NoOp, None);
        }
        let counter_source = spell_object(&mut state, PlayerId(1));
        let entry = push_spell(
            &mut state,
            PlayerId(1),
            node(
                counter(TargetFilter::StackSpell),
                vec![TargetRef::Object(spell)],
                counter_source,
            ),
        );
        let expected = if trigger_above {
            Vec::new()
        } else {
            vec![TargetRef::Object(spell)]
        };
        assert_eq!(
            reach(&state, entry)[0].2,
            expected,
            "trigger above: {trigger_above}"
        );
        let runner = resolve_raw(state);
        assert_eq!(
            runner.state().stack.iter().any(|e| e.id == spell),
            trigger_above,
            "engine agreement: with a trigger of the spell above it, that trigger is removed instead"
        );
    }
}

#[test]
fn an_entry_whose_root_is_unimplemented_acts_on_nothing() {
    for unimplemented in [false, true] {
        let mut state = GameState::new_two_player(42);
        let aimed = creature(&mut state, PlayerId(0));
        let source = spell_object(&mut state, PlayerId(1));
        let root_effect = if unimplemented {
            serde_json::from_value(serde_json::json!({"type": "Unimplemented", "name": "unparsed", "description": null})).unwrap()
        } else {
            Effect::NoOp
        };
        let mut root = node(root_effect, Vec::new(), source);
        root.sub_ability = Some(Box::new(node(
            destroy(TargetFilter::Any),
            vec![TargetRef::Object(aimed)],
            source,
        )));
        let entry = push_spell(&mut state, PlayerId(1), root);
        let expected = if unimplemented {
            Vec::new()
        } else {
            vec![TargetRef::Object(aimed)]
        };
        assert_eq!(
            reach(&state, entry)[1].2,
            expected,
            "unimplemented: {unimplemented}"
        );
        let runner = resolve_raw(state);
        assert_eq!(
            runner.state().objects[&aimed].zone,
            if unimplemented {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            },
            "engine agreement, unimplemented: {unimplemented}"
        );
    }
}

#[test]
fn a_self_move_relatch_from_another_resolution_is_not_read() {
    let (mut runner, entry) = opponent_draw_trigger(SHEOLDRED, false);
    let state = runner.state_mut();
    let on_stack = state
        .stack
        .iter_mut()
        .find(|e| e.id == entry)
        .expect("entry");
    let source = on_stack.source_id;
    let StackEntryKind::TriggeredAbility { ability, .. } = &mut on_stack.kind else {
        panic!("reach guard: a triggered ability");
    };
    let captured = ability
        .trigger_source
        .as_ref()
        .expect("reach guard: the trigger records its source")
        .identity
        .reference
        .incarnation;
    // The same trigger, exiling its own source.
    ability.effect = exile(TargetFilter::SelfRef);
    // The source moved and came back as a new object during a resolution that is
    // still in progress, which re-latched it.
    let moved = captured + 1;
    state.objects.get_mut(&source).unwrap().incarnation = moved;
    state.resolution_source_relatch = Some(engine::types::game_state::ResolutionSourceRelatch {
        object_id: source,
        original_stamp: captured,
        current_incarnation: moved,
    });
    assert!(reach(runner.state(), entry)[0].2.is_empty());
    runner.state_mut().resolution_source_relatch = None;
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&source].zone,
        Zone::Battlefield,
        "engine agreement: its own resolution does not re-latch the source"
    );
}

#[test]
fn a_root_condition_reads_the_ledgers_resolution_clears() {
    let mut state = GameState::new_two_player(42);
    let aimed = creature(&mut state, PlayerId(0));
    let revealed = creature(&mut state, PlayerId(1));
    state.last_revealed_ids = vec![revealed];
    let source = spell_object(&mut state, PlayerId(1));
    let mut root = node(
        destroy(TargetFilter::Any),
        vec![TargetRef::Object(aimed)],
        source,
    );
    root.condition = Some(condition(serde_json::json!({
        "type": "RevealedHasCardType",
        "card_type": "Creature",
    })));
    let entry = push_spell(&mut state, PlayerId(1), root);
    assert!(
        !state.last_revealed_ids.is_empty(),
        "reach guard: an earlier resolution's revealed card is still recorded"
    );
    assert!(reach(&state, entry)[0].2.is_empty());
    let runner = resolve_raw(state);
    assert_eq!(
        runner.state().objects[&aimed].zone,
        Zone::Battlefield,
        "engine agreement: resolution clears the record before the condition reads it"
    );
}

#[test]
fn a_one_sided_fight_without_its_subject_acts_on_nothing() {
    for with_subject in [true, false] {
        let mut state = GameState::new_two_player(42);
        let recipient = creature(&mut state, PlayerId(0));
        let subject = creature(&mut state, PlayerId(1));
        let source = spell_object(&mut state, PlayerId(1));
        let mut root = node(
            effect(serde_json::json!({
                "type": "TargetOnly",
                "target": {"type": "Typed", "type_filters": ["Creature"], "controller": "You", "properties": []},
            })),
            if with_subject {
                vec![TargetRef::Object(subject)]
            } else {
                Vec::new()
            },
            source,
        );
        root.optional_targeting = true;
        root.sub_ability = Some(Box::new(node(
            effect(serde_json::json!({
                "type": "DealDamage",
                "amount": {"type": "Ref", "qty": {"type": "Power", "scope": {"type": "Target"}}},
                "target": {"type": "Any"}, "damage_source": "Target",
            })),
            vec![TargetRef::Object(recipient)],
            source,
        )));
        let entry = push_spell(&mut state, PlayerId(1), root);
        let answer = reach(&state, entry)[1].2.clone();
        if with_subject {
            assert_eq!(
                answer,
                vec![TargetRef::Object(recipient)],
                "reach guard: with its subject chosen, the damage acts on its recipient"
            );
            continue;
        }
        assert!(answer.is_empty());
        let runner = resolve_raw(state);
        assert_eq!(
            runner.state().objects[&recipient].damage_marked,
            0,
            "engine agreement: with no subject, no damage"
        );
    }
}

#[test]
fn an_exile_whose_until_event_already_happened_acts_on_nothing() {
    let mut state = GameState::new_two_player(42);
    let aimed = creature(&mut state, PlayerId(0));
    let host = creature(&mut state, PlayerId(1));
    let mut root = node(
        exile(TargetFilter::Any),
        vec![TargetRef::Object(aimed)],
        host,
    );
    root.duration = Some(engine::types::ability::Duration::UntilHostLeavesPlay);
    let event = root
        .duration
        .as_ref()
        .and_then(engine::types::ability::Duration::zone_change_event)
        .expect("reach guard: the duration ends on a zone-change event");
    root.context.duration_events.push(event);
    let entry = push_trigger(&mut state, 700, host, Effect::NoOp, None);
    if let Some(StackEntryKind::TriggeredAbility { ability, .. }) =
        state.stack.back_mut().map(|e| &mut e.kind)
    {
        **ability = root;
    }
    assert!(reach(&state, entry)[0].2.is_empty());
    let runner = resolve_raw(state);
    assert_eq!(
        runner.state().objects[&aimed].zone,
        Zone::Battlefield,
        "engine agreement: the creature is not exiled"
    );
}

/// A condition a cast records, and how the root's context records it.
type CastFact = (&'static str, serde_json::Value, fn(&mut ResolvedAbility));

#[test]
fn a_later_node_reads_a_cast_fact_condition_as_its_resolution_does() {
    let rows: Vec<CastFact> = vec![
        (
            "kicked",
            serde_json::json!({"type": "AdditionalCostPaid"}),
            |root| {
                root.context.additional_cost_paid = true;
            },
        ),
        (
            "alternative cost",
            serde_json::json!({"type": "AlternativeManaCostPaid"}),
            |root| root.context.alternative_mana_cost_paid = true,
        ),
        (
            "cast from hand",
            serde_json::json!({"type": "WasCast", "zone": "Hand"}),
            |root| root.context.cast_from_zone = Some(Zone::Hand),
        ),
        (
            "cast in a main phase",
            serde_json::json!({"type": "CastDuringPhase", "phases": ["PreCombatMain", "PostCombatMain"]}),
            |root| root.context.cast_phase = Some(Phase::PreCombatMain),
        ),
    ];
    for (row, fact, record) in rows {
        for (negated, recorded) in [(false, false), (false, true), (true, false)] {
            let mut state = GameState::new_two_player(42);
            let aimed = creature(&mut state, PlayerId(0));
            let source = spell_object(&mut state, PlayerId(1));
            let mut root = node(
                effect(serde_json::json!({"type": "TargetOnly", "target": {"type": "Any"}})),
                vec![TargetRef::Object(aimed)],
                source,
            );
            if recorded {
                record(&mut root);
            }
            let mut child = node(destroy(TargetFilter::ParentTarget), Vec::new(), source);
            child.condition = Some(if negated {
                condition(serde_json::json!({"type": "Not", "condition": fact.clone()}))
            } else {
                condition(fact.clone())
            });
            root.sub_ability = Some(Box::new(child));
            let entry = push_spell(&mut state, PlayerId(1), root);
            let holds = recorded != negated;
            let expected = if holds {
                vec![TargetRef::Object(aimed)]
            } else {
                Vec::new()
            };
            assert_eq!(
                reach(&state, entry)[1].2,
                expected,
                "{row}, recorded {recorded}, negated {negated}"
            );
            let runner = resolve_raw(state);
            assert_eq!(
                runner.state().objects[&aimed].zone,
                if holds {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                },
                "engine agreement: {row}, recorded {recorded}, negated {negated}"
            );
        }
    }
}

#[test]
fn an_exile_of_a_card_in_hand_that_the_resolver_rebinds_acts_on_nothing() {
    let mut state = GameState::new_two_player(42);
    let in_hand = {
        let id = CardId(state.next_object_id);
        create_object(&mut state, id, PlayerId(0), "Held".to_string(), Zone::Hand)
    };
    let in_library = {
        let id = CardId(state.next_object_id);
        create_object(
            &mut state,
            id,
            PlayerId(0),
            "Looked".to_string(),
            Zone::Library,
        )
    };
    // An earlier resolution's "look at" set, which the exile rebinds to.
    let set = engine::types::identifiers::TrackedSetId(5);
    state.tracked_object_sets.insert(set, vec![in_library]);
    let source = spell_object(&mut state, PlayerId(1));
    let entry = push_spell(
        &mut state,
        PlayerId(1),
        node(
            exile(TargetFilter::ParentTarget),
            vec![TargetRef::Object(in_hand)],
            source,
        ),
    );
    assert!(reach(&state, entry)[0].2.is_empty());
    let runner = resolve_raw(state);
    assert_eq!(
        (
            runner.state().objects[&in_hand].zone,
            runner.state().objects[&in_library].zone
        ),
        (Zone::Hand, Zone::Exile),
        "engine agreement: the resolver exiles the looked-at card, not the targeted one"
    );
}

#[test]
fn a_referent_the_parent_produces_during_resolution_is_not_answered() {
    // A trigger whose parent sacrifices a creature it chooses; "return it"
    // names that creature, which the copy cannot know.
    let mut state = GameState::new_two_player(42);
    let fodder = creature(&mut state, PlayerId(1));
    let cast = spell_object(&mut state, PlayerId(0));
    let source = {
        let card = CardId(state.next_object_id);
        create_object(
            &mut state,
            card,
            PlayerId(1),
            "Altar".to_string(),
            Zone::Battlefield,
        )
    };
    let entry = push_trigger(
        &mut state,
        600,
        source,
        effect(serde_json::json!({
            "type": "Sacrifice",
            "target": {"type": "Typed", "type_filters": ["Creature"], "controller": "You", "properties": []},
            "count": {"type": "Fixed", "value": 1},
        })),
        Some(spell_cast(cast)),
    );
    if let Some(StackEntryKind::TriggeredAbility { ability, .. }) =
        state.stack.back_mut().map(|e| &mut e.kind)
    {
        ability.sub_ability = Some(Box::new(node(
            effect(serde_json::json!({
                "type": "ChangeZone", "origin": null, "destination": "Hand",
                "target": {"type": "ParentTarget"}, "owner_library": false,
                "enter_transformed": false, "enter_tapped": false, "enters_attacking": false,
            })),
            Vec::new(),
            source,
        )));
    }
    assert!(reach(&state, entry)[1].2.is_empty());
    let runner = resolve_raw(state);
    assert_eq!(
        (
            runner.state().objects[&cast].zone,
            runner.state().objects[&fodder].zone
        ),
        (Zone::Stack, Zone::Hand),
        "engine agreement: the sacrificed creature is returned, not the spell the event names"
    );

    // The same "return it" below a choice that chose nothing.
    let mut state = GameState::new_two_player(42);
    let mine = creature(&mut state, PlayerId(1));
    let cast = spell_object(&mut state, PlayerId(0));
    let altar = source_permanent(&mut state);
    let entry = push_trigger(
        &mut state,
        601,
        altar,
        effect(serde_json::json!({
            "type": "TargetOnly",
            "target": {"type": "Typed", "type_filters": ["Creature"], "controller": "You", "properties": []},
        })),
        Some(spell_cast(cast)),
    );
    let root = state
        .stack
        .back_mut()
        .and_then(StackEntry::ability_mut)
        .expect("entry");
    root.optional_targeting = true;
    root.sub_ability = Some(Box::new(node(
        effect(serde_json::json!({
            "type": "ChangeZone", "origin": null, "destination": "Hand",
            "target": {"type": "ParentTarget"}, "owner_library": false,
            "enter_transformed": false, "enter_tapped": false, "enters_attacking": false,
        })),
        Vec::new(),
        altar,
    )));
    assert!(reach(&state, entry)[1].2.is_empty());
    let runner = resolve_raw(state);
    assert_eq!(
        (
            runner.state().objects[&cast].zone,
            runner.state().objects[&mine].zone
        ),
        (Zone::Stack, Zone::Battlefield),
        "engine agreement: nothing is returned"
    );
}

fn source_permanent(state: &mut GameState) -> ObjectId {
    let card = CardId(state.next_object_id);
    create_object(
        state,
        card,
        PlayerId(1),
        "Altar".to_string(),
        Zone::Battlefield,
    )
}

#[test]
fn an_instead_node_below_the_first_instruction_acts_on_nothing() {
    let mut state = GameState::new_two_player(42);
    let aimed = creature(&mut state, PlayerId(0));
    state.objects.get_mut(&aimed).unwrap().toughness = Some(9);
    let source = spell_object(&mut state, PlayerId(1));
    let damage = |amount: i32, target: TargetFilter| {
        effect(serde_json::json!({
            "type": "DealDamage",
            "amount": {"type": "Fixed", "value": amount},
            "target": target,
        }))
    };
    let mut root = node(
        effect(serde_json::json!({"type": "TargetOnly", "target": {"type": "Any"}})),
        vec![TargetRef::Object(aimed)],
        source,
    );
    root.context.additional_cost_paid = true;
    let mut base = node(damage(2, TargetFilter::ParentTarget), Vec::new(), source);
    let mut kicked = node(damage(4, TargetFilter::ParentTarget), Vec::new(), source);
    kicked.condition = Some(condition(
        serde_json::json!({"type": "AdditionalCostPaidInstead"}),
    ));
    base.sub_ability = Some(Box::new(kicked));
    root.sub_ability = Some(Box::new(base));
    let entry = push_spell(&mut state, PlayerId(1), root);
    let nodes = reach(&state, entry);
    assert!(nodes[1].2.is_empty() && nodes[2].2.is_empty());
    let runner = resolve_raw(state);
    assert_eq!(
        runner.state().objects[&aimed].damage_marked,
        4,
        "engine agreement: the kicked node replaces the base one"
    );
}

#[test]
fn a_root_condition_reads_no_coin_flip_from_an_earlier_resolution() {
    let mut state = GameState::new_two_player(42);
    let aimed = creature(&mut state, PlayerId(0));
    let source = spell_object(&mut state, PlayerId(1));
    let mut root = node(
        destroy(TargetFilter::Any),
        vec![TargetRef::Object(aimed)],
        source,
    );
    root.condition = Some(condition(
        serde_json::json!({"type": "CoinFlipOutcome", "result": "Lost"}),
    ));
    let entry = push_spell(&mut state, PlayerId(1), root);
    state.resolution_coin_flip = Some(
        serde_json::from_value(serde_json::json!({
            "flipper": 1,
            "result": "Lost",
        }))
        .expect("a coin flip record"),
    );
    assert!(reach(&state, entry)[0].2.is_empty());
    let runner = resolve_raw(state);
    assert_eq!(
        runner.state().objects[&aimed].zone,
        Zone::Battlefield,
        "engine agreement: resolution forgets the earlier flip before the condition reads it"
    );
}

#[test]
fn a_root_condition_counts_its_own_resolution_as_the_resolver_does() {
    // "... if this is the first time this ability has resolved this turn"
    let mut state = GameState::new_two_player(42);
    let aimed = creature(&mut state, PlayerId(0));
    let source = creature(&mut state, PlayerId(1));
    let mut root = node(
        destroy(TargetFilter::Any),
        vec![TargetRef::Object(aimed)],
        source,
    );
    root.ability_index = Some(0);
    root.condition = Some(condition(
        serde_json::json!({"type": "AbilityUseCountThisTurn", "n": 1}),
    ));
    state.stack.push_back(StackEntry {
        id: ObjectId(500),
        source_id: source,
        controller: PlayerId(1),
        kind: StackEntryKind::ActivatedAbility {
            source_id: source,
            ability: Box::new(root),
        },
    });
    assert_eq!(
        reach(&state, ObjectId(500))[0].2,
        vec![TargetRef::Object(aimed)]
    );
    let runner = resolve_raw(state);
    assert_eq!(
        runner.state().objects[&aimed].zone,
        Zone::Graveyard,
        "engine agreement: the resolution counts itself, so the count is 1"
    );
}

const DELAY: &str = "Counter target spell. If the spell is countered this way, exile it with three time counters on it instead of putting it into its owner's graveyard. If it doesn't have suspend, it gains suspend. (At the beginning of its owner's upkeep, they remove a time counter. When the last is removed, they may play it without paying its mana cost. If it's a creature, it has haste.)";

#[test]
fn delay_exile_rider_below_its_counter_acts_on_nothing() {
    let (mut runner, bear_spell, entry) = counter_board(
        "Delay",
        DELAY,
        ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        },
    );
    let nodes = reach(runner.state(), entry);
    let rider = nodes
        .iter()
        .position(|(e, _, _)| {
            matches!(
                e,
                Effect::ChangeZone {
                    target: TargetFilter::ParentTarget,
                    destination: Zone::Exile,
                    ..
                }
            )
        })
        .expect("reach guard: the counter's exile rider");
    assert_eq!(
        nodes[0].2,
        vec![TargetRef::Object(bear_spell)],
        "reach guard: the counter acts on the spell"
    );
    assert!(nodes[rider].2.is_empty());
    assert_eq!(
        reach(
            &below_inert_instructions(runner.state(), entry, rider),
            entry
        )[rider]
            .2,
        vec![TargetRef::Object(bear_spell)],
        "reach guard: below instructions that change nothing, the rider is handed the spell"
    );
    runner.resolve_top();
    settle_prompts(&mut runner);
    assert_eq!(
        runner.state().objects.get(&bear_spell).map(|o| o.zone),
        Some(Zone::Exile),
        "engine agreement: the counter exiles the spell for its rider (an under-report)"
    );
}

#[test]
fn a_node_below_a_choice_published_for_a_scoped_player_acts_on_nothing() {
    // A combat-damage trigger: `resolve_top` scopes its chain to the damaged
    // player, and its `TargetOnly` root then publishes the chosen creature to
    // the chain's tracked set.
    for damaged in [false, true] {
        let mut state = GameState::new_two_player(42);
        let aimed = creature(&mut state, PlayerId(0));
        let source = creature(&mut state, PlayerId(1));
        let event = damaged.then_some(GameEvent::DamageDealt {
            source_id: source,
            target: TargetRef::Player(PlayerId(0)),
            amount: 2,
            is_combat: true,
            excess: 0,
        });
        let entry = push_trigger(
            &mut state,
            990,
            source,
            Effect::TargetOnly {
                target: TargetFilter::Any,
            },
            event,
        );
        let root = state
            .stack
            .back_mut()
            .and_then(StackEntry::ability_mut)
            .expect("entry");
        root.targets = vec![TargetRef::Object(aimed)];
        root.sub_ability = Some(Box::new(node(
            destroy(TargetFilter::ParentTarget),
            Vec::new(),
            source,
        )));
        let nodes = reach(&state, entry);
        if damaged {
            assert!(nodes[1].2.is_empty(), "scoped to the damaged player");
        } else {
            assert_eq!(
                nodes[1].2,
                vec![TargetRef::Object(aimed)],
                "reach guard: with no player scoped, the destroy is handed the creature"
            );
        }
        let runner = resolve_raw(state);
        assert_eq!(
            runner.state().objects[&aimed].zone,
            Zone::Graveyard,
            "engine agreement, damaged {damaged}: the creature is destroyed"
        );
    }
}

fn surveil_one() -> Effect {
    effect(serde_json::json!({
        "type": "Surveil", "count": {"type": "Fixed", "value": 1}, "target": {"type": "Controller"},
    }))
}

fn damage_one() -> Effect {
    effect(serde_json::json!({
        "type": "DealDamage", "amount": {"type": "Fixed", "value": 1}, "target": {"type": "Any"},
    }))
}

fn pump_one() -> Effect {
    effect(serde_json::json!({
        "type": "Pump", "power": {"type": "Fixed", "value": 1},
        "toughness": {"type": "Fixed", "value": 1}, "target": {"type": "Any"},
    }))
}

#[test]
fn a_node_naming_a_card_its_surveil_may_move_acts_on_nothing() {
    // A trigger whose event names a card: a destroy of "that card" reads the
    // event, which no target check at resolution validates.
    for (in_library, pump_between) in [(false, false), (true, false), (true, true)] {
        let mut state = GameState::new_two_player(42);
        let named = if in_library {
            let card = CardId(state.next_object_id);
            create_object(
                &mut state,
                card,
                PlayerId(1),
                "Top".to_string(),
                Zone::Library,
            )
        } else {
            creature(&mut state, PlayerId(0))
        };
        let source = source_permanent(&mut state);
        let entry = push_trigger(
            &mut state,
            960,
            source,
            surveil_one(),
            Some(spell_cast(named)),
        );
        let destroy_named = node(destroy(TargetFilter::TriggeringSource), Vec::new(), source);
        let root = state
            .stack
            .back_mut()
            .and_then(StackEntry::ability_mut)
            .expect("entry");
        root.sub_ability = Some(Box::new(if pump_between {
            let mut pump = node(
                effect(serde_json::json!({
                    "type": "Pump", "power": {"type": "Fixed", "value": 1},
                    "toughness": {"type": "Fixed", "value": 1}, "target": {"type": "SelfRef"},
                })),
                Vec::new(),
                source,
            );
            pump.sub_ability = Some(Box::new(destroy_named));
            pump
        } else {
            destroy_named
        }));
        let answer = reach(&state, entry)
            .last()
            .expect("the destroy node")
            .2
            .clone();
        if in_library {
            assert!(
                answer.is_empty(),
                "the surveil may move the card, pump between: {pump_between}"
            );
        } else {
            assert_eq!(
                answer,
                vec![TargetRef::Object(named)],
                "reach guard: below the surveil, a destroy of the creature the event names is answered"
            );
        }
    }
}

const LEYLINE_OF_THE_VOID: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";

#[test]
fn a_node_below_a_surveil_whose_cards_a_replacement_may_move_acts_on_nothing() {
    // The surveilling player is P1. P0's Leyline applies to P1's graveyard now;
    // P1's own does not, until an earlier instruction has changed the game.
    for (leyline, pump_first) in [(None, true), (Some(P0), false), (Some(P1), true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let aimed = scenario.add_creature(P0, "Aimed", 3, 3).id();
        if let Some(owner) = leyline {
            scenario.add_enchantment_from_oracle(owner, "Leyline of the Void", LEYLINE_OF_THE_VOID);
        }
        let mut runner = scenario.build();
        let state = runner.state_mut();
        let top = {
            let card = CardId(state.next_object_id);
            create_object(state, card, PlayerId(1), "Top".to_string(), Zone::Library)
        };
        let source = spell_object(state, PlayerId(1));
        let mut surveil = node(surveil_one(), Vec::new(), source);
        surveil.sub_ability = Some(Box::new(node(
            damage_one(),
            vec![TargetRef::Object(aimed)],
            source,
        )));
        let root = if pump_first {
            let mut pump = node(pump_one(), vec![TargetRef::Object(aimed)], source);
            pump.sub_ability = Some(Box::new(surveil));
            pump
        } else {
            surveil
        };
        let entry = push_spell(state, PlayerId(1), root);
        let answer = reach(runner.state(), entry)
            .last()
            .expect("the damage node")
            .2
            .clone();
        let Some(owner) = leyline else {
            assert_eq!(
                answer,
                vec![TargetRef::Object(aimed)],
                "reach guard: without a Leyline the damage is answered"
            );
            continue;
        };
        assert!(answer.is_empty(), "Leyline of {owner:?}");
        let mut runner = resolve_raw(runner.state().clone());
        runner
            .act(GameAction::SelectCards { cards: Vec::new() })
            .expect("put the card into the graveyard");
        assert_eq!(
            runner.state().objects.get(&top).map(|o| o.zone),
            Some(if owner == P0 {
                Zone::Exile
            } else {
                Zone::Graveyard
            }),
            "engine agreement, Leyline of {owner:?}: only an opponent's Leyline exiles the card"
        );
    }
}

#[test]
fn a_destroy_below_a_regeneration_of_its_object_acts_on_nothing() {
    for regenerate in [false, true] {
        let mut state = GameState::new_two_player(42);
        let aimed = creature(&mut state, PlayerId(0));
        let source = spell_object(&mut state, PlayerId(1));
        let first = if regenerate {
            effect(serde_json::json!({"type": "Regenerate", "target": {"type": "Any"}}))
        } else {
            effect(serde_json::json!({
                "type": "Pump", "power": {"type": "Fixed", "value": 1},
                "toughness": {"type": "Fixed", "value": 1}, "target": {"type": "Any"},
            }))
        };
        let mut root = node(first, vec![TargetRef::Object(aimed)], source);
        root.sub_ability = Some(Box::new(node(
            destroy(TargetFilter::ParentTarget),
            Vec::new(),
            source,
        )));
        let entry = push_spell(&mut state, PlayerId(1), root);
        let answer = reach(&state, entry)[1].2.clone();
        if !regenerate {
            assert_eq!(
                answer,
                vec![TargetRef::Object(aimed)],
                "reach guard: below a pump the destroy is handed the creature"
            );
            continue;
        }
        assert!(answer.is_empty());
        let runner = resolve_raw(state);
        assert_eq!(
            runner.state().objects[&aimed].zone,
            Zone::Battlefield,
            "engine agreement: the shield replaces the destruction"
        );
    }
}

#[test]
fn a_node_that_chooses_from_the_board_below_a_pump_acts_on_nothing() {
    for pumped in [false, true] {
        let mut state = GameState::new_two_player(42);
        let source = creature(&mut state, PlayerId(1));
        let object = state.objects.get_mut(&source).unwrap();
        (object.power, object.toughness) = (Some(5), Some(5));
        (object.base_power, object.base_toughness) = (Some(5), Some(5));
        let first = if pumped {
            effect(serde_json::json!({
                "type": "Pump", "power": {"type": "Fixed", "value": -3},
                "toughness": {"type": "Fixed", "value": 0}, "target": {"type": "SelfRef"},
            }))
        } else {
            Effect::NoOp
        };
        let mut phase_out = node(
            effect(serde_json::json!({
                "type": "PhaseOut",
                "target": {"type": "Typed", "type_filters": ["Creature"], "controller": null,
                    "properties": [{"type": "PtComparison", "stat": "Power", "scope": "Current",
                        "comparator": "GE", "value": {"type": "Fixed", "value": 4}}]},
            })),
            Vec::new(),
            source,
        );
        // Every creature the filter matches, not a target to choose.
        phase_out.optional_targeting = true;
        let mut root = node(first, Vec::new(), source);
        root.sub_ability = Some(Box::new(phase_out));
        state.stack.push_back(StackEntry {
            id: ObjectId(950),
            source_id: source,
            controller: PlayerId(1),
            kind: StackEntryKind::ActivatedAbility {
                source_id: source,
                ability: Box::new(root),
            },
        });
        let nodes = reach(&state, ObjectId(950));
        assert!(
            matches!(nodes[1].0, Effect::PhaseOut { .. }) && nodes[1].1.is_empty(),
            "reach guard: the phase-out declares nothing"
        );
        if pumped {
            assert!(nodes[1].2.is_empty());
        } else {
            assert_eq!(
                nodes[1].2,
                vec![TargetRef::Object(source)],
                "reach guard: below an instruction that changes nothing, the 5/5 is among the creatures with power 4 or greater"
            );
        }
        let runner = resolve_raw(state);
        assert_eq!(
            runner.state().objects[&source].is_phased_out(),
            !pumped,
            "engine agreement, pumped {pumped}: only a creature still at power 4 or greater phases out"
        );
    }
}

#[test]
fn a_node_below_a_damage_node_a_replacement_may_change_acts_on_nothing() {
    for at_pariah_controller in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let host = scenario.add_creature(P0, "Host", 3, 3).id();
        let aimed = scenario.add_creature(P0, "Aimed", 2, 2).id();
        let pariah = scenario
            .add_enchantment_from_oracle(P0, "Pariah", PARIAH)
            .id();
        let mut runner = p1_main_phase(scenario);
        runner.attach_as_bestowed_aura(pariah, host);
        let state = runner.state_mut();
        let source = spell_object(state, PlayerId(1));
        let damaged = if at_pariah_controller { P0 } else { P1 };
        let mut root = node(damage_one(), vec![TargetRef::Player(damaged)], source);
        root.sub_ability = Some(Box::new(node(
            destroy(TargetFilter::Any),
            vec![TargetRef::Object(aimed)],
            source,
        )));
        let entry = push_spell(state, PlayerId(1), root);
        let nodes = reach(runner.state(), entry);
        if !at_pariah_controller {
            assert_eq!(
                (nodes[0].2.clone(), nodes[1].2.clone()),
                (vec![TargetRef::Player(P1)], vec![TargetRef::Object(aimed)]),
                "reach guard: below damage no replacement applies to, the destroy is answered"
            );
            continue;
        }
        assert!(
            nodes[0].2.is_empty(),
            "reach guard: Pariah may move the damage"
        );
        assert!(nodes[1].2.is_empty());
    }
}

#[test]
fn a_group_whose_shared_quality_a_pump_may_change_acts_on_nothing() {
    for pumped in [false, true] {
        let mut state = GameState::new_two_player(42);
        let first = creature(&mut state, PlayerId(0));
        let second = creature(&mut state, PlayerId(0));
        for id in [first, second] {
            let object = state.objects.get_mut(&id).unwrap();
            (object.base_power, object.base_toughness) = (Some(2), Some(2));
        }
        let source = spell_object(&mut state, PlayerId(1));
        let root_effect = if pumped {
            effect(serde_json::json!({
                "type": "Pump", "power": {"type": "Fixed", "value": 1},
                "toughness": {"type": "Fixed", "value": 0}, "target": {"type": "Any"},
            }))
        } else {
            Effect::TargetOnly {
                target: TargetFilter::Any,
            }
        };
        let mut root = node(root_effect, vec![TargetRef::Object(first)], source);
        root.sub_ability = Some(Box::new(node(
            effect(serde_json::json!({
                "type": "Destroy",
                "target": {"type": "Typed", "type_filters": ["Creature"], "controller": null,
                    "properties": [{"type": "SharesQuality", "quality": "Power"}]},
                "cant_regenerate": false,
            })),
            vec![TargetRef::Object(first), TargetRef::Object(second)],
            source,
        )));
        let entry = push_spell(&mut state, PlayerId(1), root);
        let answer = reach(&state, entry)[1].2.clone();
        if pumped {
            assert!(answer.is_empty());
        } else {
            assert_eq!(
                answer,
                vec![TargetRef::Object(first), TargetRef::Object(second)],
                "reach guard: below a choice the two 2/2s share their power"
            );
        }
        let runner = resolve_raw(state);
        assert_eq!(
            runner.state().objects[&second].zone,
            if pumped {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            },
            "engine agreement, pumped {pumped}: the destroy runs only while the two share a power"
        );
    }
}

const MARTYRS_OF_KORLIS: &str = "As long as this creature is untapped, all damage that would be dealt to you by artifacts is dealt to this creature instead.";
const SOULS_FIRE: &str =
    "Target creature you control deals damage equal to its power to any target.";

#[test]
fn a_damage_node_whose_subject_a_redirect_reads_acts_on_nothing() {
    for (name, text) in [
        ("Self-Destruct", SELF_DESTRUCT),
        ("Soul's Fire", SOULS_FIRE),
    ] {
        for artifact in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let subject = scenario.add_creature(P0, "Subject", 3, 3).id();
            let martyrs = scenario
                .add_creature_from_oracle(P1, "Martyrs of Korlis", 1, 6, MARTYRS_OF_KORLIS)
                .id();
            let card = scenario
                .add_spell_to_hand_from_oracle(P0, name, true, text)
                .with_mana_cost(ManaCost::zero())
                .id();
            let mut runner = scenario.build();
            if artifact {
                let object = runner
                    .state_mut()
                    .objects
                    .get_mut(&subject)
                    .expect("subject");
                object.card_types.core_types.push(CoreType::Artifact);
                object.base_card_types.core_types.push(CoreType::Artifact);
            }
            runner
                .cast(card)
                .target_object(subject)
                .target_player(P1)
                .commit();
            let entry = runner.state().stack.back().expect("the spell").id;
            let nodes = reach(runner.state(), entry);
            let to_player = nodes
                .iter()
                .position(|(e, declared, _)| {
                    matches!(e, Effect::DealDamage { .. })
                        && declared == &vec![TargetRef::Player(P1)]
                })
                .expect("reach guard: a damage node declares P1");
            if artifact {
                assert!(
                    nodes.iter().all(|(_, _, acted_on)| acted_on.is_empty()),
                    "{name}: an artifact subject's damage to P1 goes to Martyrs of Korlis"
                );
            } else {
                assert_eq!(
                    nodes[to_player].2,
                    vec![TargetRef::Player(P1)],
                    "{name}: reach guard: a non-artifact subject damages P1"
                );
            }
            runner.resolve_top();
            assert_eq!(
                (
                    runner.life(P1),
                    runner.state().objects[&martyrs].damage_marked
                ),
                if artifact { (20, 3) } else { (17, 0) },
                "{name}: engine agreement, artifact subject: {artifact}"
            );
        }
    }
}

const DAWN_ELEMENTAL: &str = "Flying\nPrevent all damage that would be dealt to this creature.";

#[test]
fn a_damage_node_that_damages_its_own_source_acts_on_nothing_while_that_damage_is_replaced() {
    for prevented in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let subject = if prevented {
            scenario.add_creature_from_oracle(P0, "Dawn Elemental", 3, 3, DAWN_ELEMENTAL)
        } else {
            scenario.add_creature(P0, "Subject", 3, 3)
        }
        .id();
        let other = scenario.add_creature(P1, "Other", 3, 3).id();
        let card = scenario
            .add_spell_to_hand_from_oracle(P0, "Self-Destruct", true, SELF_DESTRUCT)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        runner.cast(card).target_objects(&[subject, other]).commit();
        let entry = runner.state().stack.back().expect("Self-Destruct").id;
        let nodes = reach(runner.state(), entry);
        let slot = nodes
            .iter()
            .position(|(e, _, _)| {
                e.target_filter() == Some(&TargetFilter::ParentTargetSlot { index: 0 })
            })
            .expect("reach guard: the damage to itself names slot 0");
        assert_eq!(
            nodes[slot - 1].2,
            vec![TargetRef::Object(other)],
            "reach guard: the damage to the other target is answered, prevented: {prevented}"
        );
        let expected = if prevented {
            Vec::new()
        } else {
            vec![TargetRef::Object(subject)]
        };
        assert_eq!(nodes[slot].2, expected, "prevented: {prevented}");
        runner.resolve_top();
        let subject_after = runner.state().objects.get(&subject).map(|o| o.zone);
        assert_eq!(
            subject_after,
            Some(if prevented {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }),
            "engine agreement: the subject's damage to itself, prevented: {prevented}"
        );
    }
}

#[test]
fn a_creature_that_fights_itself_acts_on_nothing_while_that_damage_is_replaced() {
    for prevented in [false, true] {
        let mut scenario = GameScenario::new();
        let fighter = if prevented {
            scenario.add_creature_from_oracle(P0, "Dawn Elemental", 3, 3, DAWN_ELEMENTAL)
        } else {
            scenario.add_creature(P0, "Fighter", 3, 3)
        }
        .id();
        let mut state = scenario.build().state().clone();
        let creature_filter = serde_json::json!(
            {"type": "Typed", "type_filters": ["Creature"], "controller": null, "properties": []}
        );
        let source = spell_object(&mut state, PlayerId(1));
        let root = node(
            effect(serde_json::json!({
                "type": "Fight", "subject": creature_filter, "target": creature_filter,
            })),
            vec![TargetRef::Object(fighter), TargetRef::Object(fighter)],
            source,
        );
        let entry = push_spell(&mut state, PlayerId(1), root);
        let expected = if prevented {
            Vec::new()
        } else {
            vec![TargetRef::Object(fighter), TargetRef::Object(fighter)]
        };
        assert_eq!(
            reach(&state, entry)[0].2,
            expected,
            "prevented: {prevented}"
        );
        let runner = resolve_raw(state);
        let fighter_after = runner.state().objects.get(&fighter).map(|o| o.zone);
        assert_eq!(
            fighter_after,
            Some(if prevented {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }),
            "engine agreement: CR 701.14c damage to itself, prevented: {prevented}"
        );
    }
}

const WARSTORM_SURGE: &str =
    "Whenever a creature you control enters, it deals damage equal to its power to any target.";

/// P1 controls an untapped Martyrs of Korlis. P0 controls Warstorm Surge and
/// casts a 3/3, and its trigger targets P1. Returns the trigger's answer, then
/// P1's life and the damage on Martyrs of Korlis after it resolves.
fn warstorm_surge_at_p1(artifact: bool) -> (Vec<TargetRef>, (i32, u32)) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let surge = scenario
        .add_enchantment_from_oracle(P0, "Warstorm Surge", WARSTORM_SURGE)
        .id();
    let martyrs = scenario
        .add_creature_from_oracle(P1, "Martyrs of Korlis", 1, 6, MARTYRS_OF_KORLIS)
        .id();
    let entering = scenario
        .add_creature_to_hand(P0, "Entering", 3, 3)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    if artifact {
        let object = runner
            .state_mut()
            .objects
            .get_mut(&entering)
            .expect("entering creature");
        object.card_types.core_types.push(CoreType::Artifact);
        object.base_card_types.core_types.push(CoreType::Artifact);
    }
    runner.cast(entering).commit();
    for _ in 0..4 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choose P1");
    let entry = trigger_of(&mut runner, surge);
    let nodes = reach(runner.state(), entry);
    assert!(
        nodes.len() == 1
            && matches!(nodes[0].0, Effect::DealDamage { .. })
            && nodes[0].1 == vec![TargetRef::Player(P1)],
        "reach guard: one damage node, declaring P1"
    );
    let acted_on = nodes[0].2.clone();
    runner.resolve_top();
    let after = (
        runner.life(P1),
        runner.state().objects[&martyrs].damage_marked,
    );
    (acted_on, after)
}

#[test]
fn warstorm_surge_damage_from_a_nonartifact_creature_acts_on_the_chosen_player() {
    let (acted_on, after) = warstorm_surge_at_p1(false);
    assert_eq!(acted_on, vec![TargetRef::Player(P1)]);
    assert_eq!(after, (17, 0), "engine agreement: P1 is dealt 3");
}

#[test]
fn warstorm_surge_damage_from_an_artifact_creature_a_redirect_reads_acts_on_nothing() {
    let (acted_on, after) = warstorm_surge_at_p1(true);
    assert_eq!(acted_on, Vec::new());
    assert_eq!(
        after,
        (20, 3),
        "engine agreement: the damage is dealt to Martyrs of Korlis"
    );
}

const COMPEL_BRUTALITY: &str = "Choose one —\n• Target creature you control deals damage equal to its power to target creature or planeswalker an opponent controls.\n• Target planeswalker you control deals damage equal to its loyalty to target creature or planeswalker an opponent controls.";

/// P0 casts Compel Brutality's second mode: its planeswalker with three
/// loyalty counters is the subject, and the recipient is P1's 3/3, Dawn
/// Elemental where `dawn_elemental`. Returns the recipient, the damage node's
/// answer, and the damage marked on the recipient after it resolves.
///
/// The engine deals no damage here: `deal_damage::damage_source_eligible`
/// requires the subject to be a creature, but the card has the planeswalker
/// deal the damage (CR 120.1: objects deal damage). The answer still names the
/// recipient, because whether it is changed is an outcome the answer does not
/// predict. With that requirement dropped, the engine damages the 3/3 and the
/// answer on Dawn Elemental is empty, as its prevention then applies.
fn compel_brutality_planeswalker_mode(dawn_elemental: bool) -> (ObjectId, Vec<TargetRef>, u32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let walker = scenario
        .add_creature(P0, "Walker", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 3)
        .id();
    let recipient = if dawn_elemental {
        scenario.add_creature_from_oracle(P1, "Dawn Elemental", 3, 3, DAWN_ELEMENTAL)
    } else {
        scenario.add_creature(P1, "Recipient", 3, 3)
    }
    .id();
    let card = scenario
        .add_spell_to_hand_from_oracle(P0, "Compel Brutality", true, COMPEL_BRUTALITY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner
        .cast(card)
        .modes(&[1])
        .target_object(walker)
        .target_object(recipient)
        .commit();
    let entry = runner.state().stack.back().expect("Compel Brutality").id;
    let nodes = reach(runner.state(), entry);
    let damage = nodes
        .iter()
        .position(|(e, declared, _)| {
            matches!(e, Effect::DealDamage { .. })
                && declared == &vec![TargetRef::Object(recipient)]
        })
        .expect("reach guard: a damage node declares the recipient");
    let acted_on = nodes[damage].2.clone();
    runner.resolve_top();
    (
        recipient,
        acted_on,
        runner.state().objects[&recipient].damage_marked,
    )
}

#[test]
fn compel_brutality_planeswalker_mode_acts_on_its_recipient() {
    let (recipient, acted_on, damage) = compel_brutality_planeswalker_mode(false);
    assert_eq!(acted_on, vec![TargetRef::Object(recipient)]);
    assert_eq!(damage, 0, "the engine's current result");
}

#[test]
fn compel_brutality_planeswalker_mode_acts_on_a_recipient_that_prevents_its_damage() {
    let (recipient, acted_on, damage) = compel_brutality_planeswalker_mode(true);
    assert_eq!(acted_on, vec![TargetRef::Object(recipient)]);
    assert_eq!(damage, 0, "the engine's current result");
}
