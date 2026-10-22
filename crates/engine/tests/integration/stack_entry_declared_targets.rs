//! `ability_utils::declared_targets_in_chain` on pending stack entries
//! (CR 115.1, CR 115.10a).

use engine::game::ability_utils::{declared_targets_in_chain, flatten_targets_in_chain};
use engine::game::combat::AttackTarget;
use engine::game::derived_views::derive_views;
use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCondition, ChosenTargetSlots, ControllerRef, CountBinding, Effect, ManaProduction,
    ManaTargetRole, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, TargetRef, ZoneRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameState, PersistedGameState, PersistedRestoreFinalization, RetargetScope,
    ShardChoice, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SABOTAGE_STRATEGIST: &str = "Flying, vigilance\nWhenever one or more creatures attack you, those creatures get -1/-0 until end of turn.\nExhaust — {5}{U}{U}: Put three +1/+1 counters on this creature. (Activate each exhaust ability only once.)";
const FRONTIER_WARMONGER: &str = "Whenever one or more creatures attack one of your opponents or a planeswalker they control, those creatures gain menace until end of turn.";
const ENDLESS_COCKROACHES: &str = "When this creature dies, return it to its owner's hand.";
const CONTEMPT: &str = "Enchant creature\nWhen enchanted creature attacks, return it and this Aura to their owners' hands at end of combat.";
const PACIFISM: &str = "Enchant creature\nEnchanted creature can't attack or block.";
const RIPTIDE_GEARHULK: &str = "Double strike\nProwess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)\nWhen this creature enters, for each opponent, put up to one target nonland permanent that player controls into its owner's library third from the top.";
const SHOCK: &str = "Shock deals 2 damage to any target.";
const MURDER: &str = "Destroy target creature.";
const TAIL_SWIPE: &str = "Choose target creature you control and target creature you don't control. If you cast this spell during your main phase, the creature you control gets +1/+1 until end of turn. Then those creatures fight each other. (Each deals damage equal to its power to the other.)";
const PSIONIC_ENTITY: &str =
    "{T}: This creature deals 2 damage to any target and 3 damage to itself.";
const VAMPIRE_SOVEREIGN: &str =
    "Flying\nWhen this creature enters, target opponent loses 3 life and you gain 3 life.";
const SWOOPING_PTERANODON: &str = "Flying, haste\nWhenever this creature or another Dinosaur you control with flying enters, gain control of target creature an opponent controls until end of turn. Untap that creature. It gains flying and haste until end of turn. At the beginning of the next end step, target land deals 3 damage to that creature.";
const FATAL_FISSURE: &str = "Choose target creature. When that creature dies this turn, you earthbend 4. (Target land you control becomes a 0/0 creature with haste that's still a land. Put four +1/+1 counters on it. When it dies or is exiled, return it to the battlefield tapped.)";
const AEGIS_ANGEL: &str = "Flying (This creature can't be blocked except by creatures with flying or reach.)\nWhen this creature enters, another target permanent gains indestructible for as long as you control this creature. (Effects that say \"destroy\" don't destroy it. A creature with indestructible can't be destroyed by damage.)";

/// `(flatten_targets_in_chain, declared_targets_in_chain)` of the
/// stack entry with `id`.
fn both(state: &GameState, id: ObjectId) -> (Vec<TargetRef>, Vec<TargetRef>) {
    let ability = state
        .stack
        .iter()
        .find(|e| e.id == id)
        .and_then(|e| e.ability())
        .expect("entry with an ability");
    (
        flatten_targets_in_chain(ability),
        declared_targets_in_chain(ability),
    )
}

/// Passes priority until a triggered ability of `source` is on the stack, and
/// returns the newest one.
fn trigger_of(runner: &mut GameRunner, source: ObjectId) -> ObjectId {
    for _ in 0..24 {
        if let Some(entry) = runner.state().stack.iter().rev().find(|e| {
            e.source_id == source && matches!(e.kind, StackEntryKind::TriggeredAbility { .. })
        }) {
            return entry.id;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("no trigger of {source:?} reached the stack");
}

/// The built scenario with P1 active and holding priority.
fn p1_priority(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
    runner
}

fn give_keyword(runner: &mut GameRunner, id: ObjectId, keyword: Keyword) {
    let object = runner.state_mut().objects.get_mut(&id).expect("object");
    object.keywords.push(keyword.clone());
    object.base_keywords.push(keyword);
    runner.state_mut().layers_dirty.mark_full();
}

/// P0's 2/2 attacks P1, who controls Sabotage Strategist.
fn sabotage_board() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    let strategist = {
        let mut b = scenario.add_creature(P1, "Sabotage Strategist", 2, 2);
        b.from_oracle_text_with_keywords(&["Exhaust", "Flying", "Vigilance"], SABOTAGE_STRATEGIST);
        b.id()
    };
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    (runner, attacker, strategist)
}

#[test]
fn a_batched_attack_trigger_targets_none_of_the_attackers_it_holds() {
    let (mut runner, attacker, strategist) = sabotage_board();
    let entry = trigger_of(&mut runner, strategist);
    let (held, declared) = both(runner.state(), entry);
    assert_eq!(
        held,
        vec![TargetRef::Object(attacker)],
        "reach guard: the trigger holds the attacker"
    );
    assert_eq!(declared, Vec::new());
    assert_eq!(
        displayed_targets(runner.state(), entry),
        vec![TargetRef::Object(attacker)]
    );

    give_keyword(&mut runner, attacker, Keyword::Shroud);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&attacker].power,
        Some(1),
        "CR 115.10a + CR 702.18a: shroud does not stop an ability that does not target"
    );
}

#[test]
fn a_granting_attack_trigger_targets_none_of_the_attackers_it_holds() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Bear", 2, 2).id();
    let warmonger = scenario
        .add_creature_from_oracle(P0, "Frontier Warmonger", 4, 4, FRONTIER_WARMONGER)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    let entry = trigger_of(&mut runner, warmonger);
    let (held, declared) = both(runner.state(), entry);
    assert_eq!(
        held,
        vec![TargetRef::Object(attacker)],
        "reach guard: the trigger holds the attacker"
    );
    assert!(
        matches!(
            runner
                .state()
                .stack
                .iter()
                .find(|e| e.id == entry)
                .and_then(|e| e.ability())
                .map(|a| &a.effect),
            Some(Effect::GenericEffect { target: None, .. })
        ),
        "reach guard: the effect has no target filter"
    );
    assert_eq!(declared, Vec::new());
}

#[test]
fn a_dies_trigger_targets_none_of_the_card_it_holds() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let roach = scenario
        .add_creature_from_oracle(P0, "Endless Cockroaches", 1, 1, ENDLESS_COCKROACHES)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = p1_priority(scenario);
    runner.cast(murder).target_objects(&[roach]).commit();
    let entry = trigger_of(&mut runner, roach);
    assert_eq!(
        runner.state().objects[&roach].zone,
        Zone::Graveyard,
        "reach guard: the creature died"
    );
    let (held, declared) = both(runner.state(), entry);
    assert_eq!(
        held,
        vec![TargetRef::Object(roach)],
        "reach guard: the trigger holds the card that died"
    );
    assert_eq!(declared, Vec::new());
}

#[test]
fn a_delayed_trigger_targets_none_of_the_creature_it_holds() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let contempt = scenario
        .add_creature(P1, "Contempt", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], CONTEMPT)
        .id();
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), contempt, bear);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("declare attackers");
    let attack_trigger = trigger_of(&mut runner, contempt);
    let (_, declared_attack) = both(runner.state(), attack_trigger);
    assert_eq!(
        declared_attack,
        Vec::new(),
        "the attack trigger targets nothing"
    );
    let mut delayed = None;
    for _ in 0..24 {
        if let Some(entry) = runner.state().stack.iter().find(|e| {
            e.id != attack_trigger
                && e.source_id == contempt
                && matches!(e.kind, StackEntryKind::TriggeredAbility { .. })
                && e.ability().is_some_and(|a| {
                    matches!(
                        a.effect,
                        Effect::Bounce {
                            target: TargetFilter::TriggeringSource,
                            ..
                        }
                    )
                })
        }) {
            delayed = Some(entry.id);
            break;
        }
        if let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
            let order = (0..triggers.len()).collect();
            runner
                .act(GameAction::OrderTriggers { order })
                .expect("order triggers");
        } else {
            runner.act(GameAction::PassPriority).expect("pass priority");
        }
    }
    let delayed = delayed.expect("reach guard: the delayed return is on the stack");
    let (held, declared) = both(runner.state(), delayed);
    assert!(
        held.contains(&TargetRef::Object(bear)),
        "reach guard: the delayed trigger holds the creature"
    );
    assert_eq!(declared, Vec::new());

    give_keyword(&mut runner, bear, Keyword::Shroud);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Hand,
        "CR 115.10a + CR 702.18a: shroud does not stop an ability that does not target"
    );
}

#[test]
fn shock_targets_the_creature_it_is_cast_at() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    runner.cast(shock).target_objects(&[bear]).commit();
    let entry = runner.state().stack.back().expect("Shock").id;
    let (held, declared) = both(runner.state(), entry);
    assert_eq!(held, vec![TargetRef::Object(bear)]);
    assert_eq!(declared, vec![TargetRef::Object(bear)]);
}

#[test]
fn tail_swipe_targets_both_creatures_its_fight_inherits() {
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
    let ability = runner
        .state()
        .stack
        .back()
        .and_then(|e| e.ability())
        .expect("ability");
    let mut node = Some(ability);
    let mut fight_targets = None;
    while let Some(n) = node {
        if matches!(n.effect, Effect::Fight { .. }) {
            fight_targets = Some(n.targets.clone());
        }
        node = n.sub_ability.as_deref();
    }
    assert_eq!(
        fight_targets,
        Some(Vec::new()),
        "reach guard: the fight holds no targets of its own"
    );
    let (_, declared) = both(runner.state(), entry);
    assert_eq!(
        declared,
        vec![TargetRef::Object(own), TargetRef::Object(theirs)]
    );
}

fn entity_board(aim_at_self: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let entity = scenario
        .add_creature_from_oracle(P1, "Psionic Entity", 2, 2, PSIONIC_ENTITY)
        .id();
    let mut runner = p1_priority(scenario);
    runner
        .state_mut()
        .objects
        .get_mut(&entity)
        .expect("entity")
        .summoning_sick = false;
    let aim = if aim_at_self { entity } else { bear };
    runner
        .act(GameAction::ActivateAbility {
            source_id: entity,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(aim)),
        })
        .expect("choose the target");
    let entry = runner.state().stack.back().expect("ability").id;
    (runner, entry, entity, bear)
}

#[test]
fn psionic_entity_targets_only_what_it_is_aimed_at() {
    let (runner, entry, _, bear) = entity_board(false);
    let ability = runner
        .state()
        .stack
        .back()
        .and_then(|e| e.ability())
        .expect("ability");
    let self_damage = ability
        .sub_ability
        .as_deref()
        .expect("reach guard: a second node");
    assert!(
        matches!(self_damage.effect, Effect::DealDamage { .. }),
        "reach guard: the second node deals damage"
    );
    let (_, declared) = both(runner.state(), entry);
    assert_eq!(declared, vec![TargetRef::Object(bear)]);
}

#[test]
fn psionic_entity_aimed_at_itself_targets_itself() {
    let (runner, entry, entity, _) = entity_board(true);
    let (_, declared) = both(runner.state(), entry);
    assert_eq!(declared, vec![TargetRef::Object(entity)]);
}

#[test]
fn an_enters_trigger_targets_the_permanent_it_declares() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let angel = scenario
        .add_creature_to_hand_from_oracle(P0, "Aegis Angel", 5, 5, AEGIS_ANGEL)
        .from_oracle_text_with_keywords(&["Flying"], AEGIS_ANGEL)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(angel).commit();
    let entry = trigger_of(&mut runner, angel);
    let (held, declared) = both(runner.state(), entry);
    assert_eq!(
        held,
        vec![TargetRef::Object(bear)],
        "reach guard: the trigger holds its target"
    );
    assert_eq!(declared, vec![TargetRef::Object(bear)]);
}

#[test]
fn an_aura_spell_targets_the_creature_it_will_enchant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let pacifism = scenario
        .add_spell_to_hand_from_oracle(P0, "Pacifism", false, PACIFISM)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], PACIFISM)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(pacifism).target_objects(&[bear]).commit();
    let entry = runner.state().stack.back().expect("Pacifism").id;
    let (held, declared) = both(runner.state(), entry);
    assert_eq!(
        held,
        vec![TargetRef::Object(bear)],
        "reach guard: the spell holds its target"
    );
    assert_eq!(declared, vec![TargetRef::Object(bear)]);
}

#[test]
fn a_per_opponent_trigger_targets_only_the_permanents_it_is_aimed_at() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let p2 = PlayerId(2);
    let first = scenario.add_creature(P1, "First Bear", 2, 2).id();
    let second = scenario.add_creature(p2, "Second Bear", 2, 2).id();
    let gearhulk = scenario
        .add_creature_to_hand_from_oracle(P0, "Riptide Gearhulk", 4, 4, RIPTIDE_GEARHULK)
        .from_oracle_text_with_keywords(&["Double strike", "Prowess"], RIPTIDE_GEARHULK)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(gearhulk).commit();
    for _ in 0..12 {
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner.choose_first_legal_target().expect("choose a target");
            }
            _ if runner.state().stack.iter().any(|e| {
                e.source_id == gearhulk && matches!(e.kind, StackEntryKind::TriggeredAbility { .. })
            }) =>
            {
                break;
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
    let entry = trigger_of(&mut runner, gearhulk);
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == entry)
        .and_then(|e| e.ability())
        .expect("ability");
    let (_, declared) = both(runner.state(), entry);
    assert!(
        ability
            .targets
            .iter()
            .any(|t| matches!(t, TargetRef::Player(_))),
        "reach guard: the fanout node also holds the opponents it iterates"
    );
    assert_eq!(
        declared,
        vec![TargetRef::Object(first), TargetRef::Object(second)]
    );
}

fn crimes(state: &GameState, player: PlayerId) -> u32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player")
        .crimes_committed_this_turn
}

#[test]
fn a_batched_attack_trigger_commits_no_crime() {
    let (mut runner, attacker, strategist) = sabotage_board();
    let entry = trigger_of(&mut runner, strategist);
    let (held, _) = both(runner.state(), entry);
    assert_eq!(
        held,
        vec![TargetRef::Object(attacker)],
        "reach guard: the trigger holds the attacker"
    );
    assert_eq!(
        runner.state().objects[&attacker].controller,
        P0,
        "reach guard: the attacker is P1's opponent's"
    );
    assert_eq!(crimes(runner.state(), P1), 0, "CR 700.13");
}

#[test]
fn an_enters_trigger_that_targets_an_opponent_commits_a_crime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sovereign = scenario
        .add_creature_to_hand_from_oracle(P0, "Vampire Sovereign", 3, 4, VAMPIRE_SOVEREIGN)
        .from_oracle_text_with_keywords(&["Flying"], VAMPIRE_SOVEREIGN)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(sovereign).commit();
    let entry = trigger_of(&mut runner, sovereign);
    let (_, declared) = both(runner.state(), entry);
    assert_eq!(
        declared,
        vec![TargetRef::Player(P1)],
        "reach guard: the trigger targets the opponent"
    );
    assert_eq!(crimes(runner.state(), P0), 1, "CR 700.13");
}

/// P0's Swooping Pteranodon has taken P1's 2/2, and P0 controls `lands`
/// Mountains. Passes to the end step until the Pteranodon's delayed trigger is
/// on the stack, choosing the first Mountain if asked. Returns the Mountains,
/// the trigger, whether a target prompt opened, and the target of every
/// `GameEvent::BecomesTarget` emitted from the end of the enters trigger on.
fn pteranodon_end_step(
    lands: usize,
) -> (GameRunner, Vec<ObjectId>, ObjectId, bool, Vec<TargetRef>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let pteranodon = scenario
        .add_creature_to_hand_from_oracle(P0, "Swooping Pteranodon", 3, 3, SWOOPING_PTERANODON)
        .from_oracle_text_with_keywords(&["Flying", "Haste"], SWOOPING_PTERANODON)
        .with_subtypes(vec!["Dinosaur"])
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mountains: Vec<ObjectId> = (0..lands)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    let mut runner = scenario.build();
    runner.cast(pteranodon).commit();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&bear].controller,
        P0,
        "reach guard: the enters trigger resolved"
    );
    let mut prompted = false;
    let mut targeted = Vec::new();
    for _ in 0..40 {
        let state = runner.state();
        if state.phase == Phase::End
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
            && !state.stack.is_empty()
        {
            break;
        }
        let action = if matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. }) {
            prompted = true;
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(mountains[0])),
            }
        } else {
            GameAction::PassPriority
        };
        let result = runner.act(action).expect("advance to the end step");
        targeted.extend(result.events.into_iter().filter_map(|event| match event {
            GameEvent::BecomesTarget { target, .. } => Some(target),
            _ => None,
        }));
    }
    let entry = trigger_of(&mut runner, pteranodon);
    (runner, mountains, entry, prompted, targeted)
}

#[test]
fn an_end_step_trigger_makes_only_its_one_legal_target_a_target() {
    let (runner, mountains, entry, prompted, targeted) = pteranodon_end_step(1);
    assert!(
        !prompted,
        "reach guard: the only land was chosen without a prompt"
    );
    let (held, _) = both(runner.state(), entry);
    assert!(
        held.iter().any(|t| *t != TargetRef::Object(mountains[0])),
        "reach guard: the trigger also holds an object it does not target"
    );
    assert_eq!(
        targeted,
        vec![TargetRef::Object(mountains[0])],
        "CR 115.10a"
    );
}

#[test]
fn an_end_step_trigger_makes_only_its_chosen_target_a_target() {
    let (runner, mountains, entry, prompted, targeted) = pteranodon_end_step(2);
    assert!(prompted, "reach guard: the land was chosen at a prompt");
    let (held, _) = both(runner.state(), entry);
    assert!(
        held.iter().any(|t| *t != TargetRef::Object(mountains[0])),
        "reach guard: the trigger also holds an object it does not target"
    );
    assert_eq!(
        targeted,
        vec![TargetRef::Object(mountains[0])],
        "CR 115.10a"
    );
}

/// A Fatal Fissure board once its delayed trigger is on the stack.
struct FissureTrigger {
    runner: GameRunner,
    bear: ObjectId,
    swamps: Vec<ObjectId>,
    entry: ObjectId,
    /// The query's answer for the trigger's entry at its target prompt, if
    /// one opened.
    at_prompt: Option<Vec<TargetRef>>,
    /// `GameEvent::BecomesTarget` targets and `GameEvent::CrimeCommitted`
    /// players emitted by the actions after Murder is cast.
    targeted: Vec<TargetRef>,
    criminals: Vec<PlayerId>,
}

/// P0, who controls `lands` Swamps, casts Fatal Fissure at P1's 2/2 and it
/// resolves. P0 then casts Murder at the 2/2 and passes until the delayed
/// trigger is on the stack, choosing the first Swamp if asked.
fn fissure_trigger(lands: usize) -> FissureTrigger {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let fissure = scenario
        .add_spell_to_hand_from_oracle(P0, "Fatal Fissure", true, FATAL_FISSURE)
        .from_oracle_text_with_keywords(&["Earthbend"], FATAL_FISSURE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let swamps: Vec<ObjectId> = (0..lands)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    let mut runner = scenario.build();
    runner.cast(fissure).target_objects(&[bear]).commit();
    runner.advance_until_stack_empty();
    runner.cast(murder).target_objects(&[bear]).commit();
    let mut at_prompt = None;
    let mut targeted = Vec::new();
    let mut criminals = Vec::new();
    for _ in 0..40 {
        let state = runner.state();
        let pending = state.stack.iter().rev().find(|e| {
            e.source_id == fissure && matches!(e.kind, StackEntryKind::TriggeredAbility { .. })
        });
        let action = match (&state.waiting_for, pending) {
            (WaitingFor::Priority { .. }, Some(_)) => break,
            (WaitingFor::TriggerTargetSelection { .. }, Some(entry)) => {
                at_prompt = Some(both(state, entry.id).1);
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(swamps[0])),
                }
            }
            _ => GameAction::PassPriority,
        };
        let result = runner.act(action).expect("advance to the delayed trigger");
        for event in result.events {
            match event {
                GameEvent::BecomesTarget { target, .. } => targeted.push(target),
                GameEvent::CrimeCommitted { player_id } => criminals.push(player_id),
                _ => {}
            }
        }
    }
    let entry = trigger_of(&mut runner, fissure);
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "reach guard: the creature the delayed trigger waited on died"
    );
    FissureTrigger {
        runner,
        bear,
        swamps,
        entry,
        at_prompt,
        targeted,
        criminals,
    }
}

#[test]
fn a_delayed_trigger_with_one_legal_land_targets_only_the_land() {
    let fissure = fissure_trigger(1);
    let swamp = TargetRef::Object(fissure.swamps[0]);
    assert_eq!(
        fissure.at_prompt, None,
        "reach guard: the only land was chosen without a prompt"
    );
    let (held, declared) = both(fissure.runner.state(), fissure.entry);
    assert!(
        held.contains(&TargetRef::Object(fissure.bear)),
        "reach guard: the trigger also holds the creature it waited on"
    );
    assert_eq!(declared, vec![swamp.clone()]);
    assert_eq!(fissure.targeted, vec![swamp], "CR 115.10a");
    assert_eq!(fissure.criminals, Vec::new(), "CR 700.13");
}

#[test]
fn a_delayed_trigger_with_a_chosen_land_targets_only_the_land() {
    let fissure = fissure_trigger(2);
    assert!(
        fissure.at_prompt.is_some(),
        "reach guard: the land was chosen at a prompt"
    );
    assert!(
        both(fissure.runner.state(), fissure.entry)
            .0
            .contains(&TargetRef::Object(fissure.bear)),
        "reach guard: the trigger also holds the creature it waited on"
    );
    assert_eq!(fissure.at_prompt, Some(Vec::new()));
    assert_eq!(
        both(fissure.runner.state(), fissure.entry).1,
        vec![TargetRef::Object(fissure.swamps[0])]
    );
    assert_eq!(
        fissure.targeted,
        vec![TargetRef::Object(fissure.swamps[0])],
        "CR 115.10a"
    );
    assert_eq!(fissure.criminals, Vec::new(), "CR 700.13");
}

const GIFT_OF_IMMORTALITY: &str = "Enchant creature\nWhen enchanted creature dies, return that card to the battlefield under its owner's control. Return this card to the battlefield attached to that creature at the beginning of the next end step.";
const BASALT_GOLEM: &str = "This creature can't be blocked by artifact creatures.\nWhenever this creature becomes blocked by a creature, that creature's controller sacrifices it at end of combat. If the player does, they create a 0/2 colorless Wall artifact creature token with defender.";
const KIKI_JIKI: &str = "Haste\n{T}: Create a token that's a copy of target nonlegendary creature you control, except it has haste. Sacrifice it at the beginning of the next end step.";
const REDIRECT: &str = "You may choose new targets for target spell.";
const SPELLSKITE: &str = "{U/P}: Change a target of target spell or ability to this creature. ({U/P} can be paid with either {U} or 2 life.)";
const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";
const ZADA: &str = "Whenever you cast an instant or sorcery spell that targets only Zada, copy that spell for each other creature you control that the spell could target. Each copy targets a different one of those creatures.";
const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";

/// Passes priority, ordering triggers as listed, until `done` holds. Returns
/// every event those actions emitted.
fn pass_until(runner: &mut GameRunner, done: impl Fn(&GameState) -> bool) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..60 {
        if done(runner.state()) {
            return events;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: Vec::new(),
                bands: Vec::new(),
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: Vec::new(),
            },
            other => panic!("unexpected prompt {other:?}"),
        };
        events.extend(runner.act(action).expect("pass").events);
    }
    panic!("the awaited state was not reached");
}

/// The newest triggered stack entry of `source`, if any.
fn pending_trigger_of(state: &GameState, source: ObjectId) -> Option<ObjectId> {
    state
        .stack
        .iter()
        .rev()
        .find(|e| {
            e.source_id == source && matches!(e.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .map(|e| e.id)
}

fn criminals(events: &[GameEvent]) -> Vec<PlayerId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CrimeCommitted { player_id } => Some(*player_id),
            _ => None,
        })
        .collect()
}

#[test]
fn a_delayed_return_targets_none_of_the_creature_it_holds() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let gift = scenario
        .add_enchantment_from_oracle(P0, "Gift of Immortality", GIFT_OF_IMMORTALITY)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], GIFT_OF_IMMORTALITY)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = p1_priority(scenario);
    attach_to(runner.state_mut(), gift, bear);
    runner.cast(murder).target_objects(&[bear]).commit();
    let mut events = pass_until(&mut runner, |state| {
        state.phase == Phase::End && pending_trigger_of(state, gift).is_some()
    });
    let entry = pending_trigger_of(runner.state(), gift).expect("the delayed return");
    let (held, declared) = both(runner.state(), entry);
    assert!(
        held.contains(&TargetRef::Object(bear)),
        "reach guard: the delayed return holds the creature"
    );
    assert_eq!(
        runner.state().objects[&bear].controller,
        P1,
        "reach guard: the creature is P0's opponent's"
    );
    assert_eq!(declared, Vec::new());
    events.extend(pass_until(&mut runner, |state| state.stack.is_empty()));
    assert_eq!(
        runner.state().objects[&gift].attached_to,
        Some(AttachTarget::Object(bear)),
        "reach guard: the return resolved"
    );
    assert_eq!(criminals(&events), Vec::new(), "CR 700.13");
}

#[test]
fn a_delayed_sacrifice_targets_none_of_the_blocker_it_holds() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let golem = scenario
        .add_creature_from_oracle(P0, "Basalt Golem", 2, 4, BASALT_GOLEM)
        .id();
    let blocker = scenario.add_creature(P1, "Blocker", 0, 5).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&golem)
        .expect("golem")
        .summoning_sick = false;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(golem, AttackTarget::Player(P1))])
        .expect("declare attackers");
    let mut events = pass_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. })
    });
    events.extend(
        runner
            .declare_blockers(&[(blocker, golem)])
            .expect("declare blockers")
            .events,
    );
    events.extend(pass_until(&mut runner, |state| {
        state.phase == Phase::EndCombat && pending_trigger_of(state, golem).is_some()
    }));
    let entry = pending_trigger_of(runner.state(), golem).expect("the delayed sacrifice");
    let (held, declared) = both(runner.state(), entry);
    assert!(
        held.contains(&TargetRef::Object(blocker)),
        "reach guard: the delayed sacrifice holds the blocker"
    );
    assert_eq!(
        runner.state().objects[&blocker].controller,
        P1,
        "reach guard: the blocker is P0's opponent's"
    );
    assert_eq!(declared, Vec::new());
    assert_eq!(criminals(&events), Vec::new(), "CR 700.13");
}

#[test]
fn an_end_step_sacrifice_targets_none_of_the_token_it_holds() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kiki = scenario
        .add_creature_from_oracle(P0, "Kiki-Jiki, Mirror Breaker", 2, 2, KIKI_JIKI)
        .from_oracle_text_with_keywords(&["Haste"], KIKI_JIKI)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: kiki,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bear)),
            })
            .expect("choose the creature to copy");
    }
    runner.advance_until_stack_empty();
    let token = runner
        .state()
        .objects
        .values()
        .find(|o| o.zone == Zone::Battlefield && o.id != bear && o.name == "Bear")
        .map(|o| o.id)
        .expect("reach guard: the token copy");
    pass_until(&mut runner, |state| {
        state.phase == Phase::End && pending_trigger_of(state, kiki).is_some()
    });
    let entry = pending_trigger_of(runner.state(), kiki).expect("the delayed sacrifice");
    let (held, declared) = both(runner.state(), entry);
    assert_eq!(
        held,
        vec![TargetRef::Object(token)],
        "reach guard: the delayed sacrifice holds the token"
    );
    assert_eq!(declared, Vec::new());
}

/// `other` is a second creature of P1's.
fn shock_on_the_stack(scenario: &mut GameScenario) -> (ObjectId, ObjectId, ObjectId) {
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "First Bear", 2, 2).id();
    let other = scenario.add_creature(P1, "Other Bear", 2, 2).id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    (first, other, shock)
}

fn declared_of(state: &GameState, id: ObjectId) -> Vec<TargetRef> {
    both(state, id).1
}

#[test]
fn a_spell_given_new_targets_targets_the_new_ones() {
    let mut scenario = GameScenario::new();
    let (first, other, shock) = shock_on_the_stack(&mut scenario);
    let redirect = scenario
        .add_spell_to_hand_from_oracle(P1, "Redirect", true, REDIRECT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(shock).target_objects(&[first]).commit();
    assert_eq!(
        declared_of(runner.state(), shock),
        vec![TargetRef::Object(first)],
        "reach guard: Shock was cast at the first creature"
    );
    runner.act(GameAction::PassPriority).expect("pass to P1");
    runner.cast(redirect).target_objects(&[shock]).commit();
    pass_until(&mut runner, |state| {
        matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::RetargetChoice { .. }
        )
    });
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("choose new targets");
    }
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(other))],
        })
        .expect("retarget");
    let (held, declared) = both(runner.state(), shock);
    assert_eq!(
        held,
        vec![TargetRef::Object(other)],
        "reach guard: the new target was written"
    );
    assert_eq!(declared, vec![TargetRef::Object(other)], "CR 115.7d");
}

#[test]
fn a_spell_given_new_targets_at_a_prompt_without_slot_addresses_targets_the_new_ones() {
    let mut scenario = GameScenario::new();
    let (first, other, shock) = shock_on_the_stack(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(shock).target_objects(&[first]).commit();
    let index = runner
        .state()
        .stack
        .iter()
        .position(|e| e.id == shock)
        .expect("Shock is on the stack");
    // A retarget prompt saved before prompts carried slot addresses.
    runner.state_mut().waiting_for = WaitingFor::RetargetChoice {
        player: P1,
        stack_entry_index: index,
        scope: RetargetScope::All,
        current_targets: vec![TargetRef::Object(first)],
        slots: Vec::new(),
        slot_pools: Vec::new(),
        legal_new_targets: vec![TargetRef::Object(first), TargetRef::Object(other)],
        keep_is_distinct: Vec::new(),
    };
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(other))],
        })
        .expect("retarget");
    let (held, declared) = both(runner.state(), shock);
    assert_eq!(
        held,
        vec![TargetRef::Object(other)],
        "reach guard: the new target was written"
    );
    assert_eq!(declared, vec![TargetRef::Object(other)], "CR 115.7d");
}

#[test]
fn a_spell_whose_target_is_changed_to_a_creature_targets_that_creature() {
    let mut scenario = GameScenario::new();
    let (first, _, shock) = shock_on_the_stack(&mut scenario);
    let spellskite = scenario
        .add_creature_from_oracle(P1, "Spellskite", 0, 4, SPELLSKITE)
        .id();
    let mut runner = scenario.build();
    runner.cast(shock).target_objects(&[first]).commit();
    runner.act(GameAction::PassPriority).expect("pass to P1");
    runner
        .act(GameAction::ActivateAbility {
            source_id: spellskite,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(shock)),
                    })
                    .expect("target Shock");
            }
            WaitingFor::PhyrexianPayment { .. } => {
                runner
                    .act(GameAction::SubmitPhyrexianChoices {
                        choices: vec![ShardChoice::PayLife],
                    })
                    .expect("pay 2 life");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    pass_until(&mut runner, |state| state.stack.len() == 1);
    let (held, declared) = both(runner.state(), shock);
    assert_eq!(
        held,
        vec![TargetRef::Object(spellskite)],
        "reach guard: the new target was written"
    );
    assert_eq!(declared, vec![TargetRef::Object(spellskite)], "CR 115.7b");
}

/// P0 casts Shock at P1's first creature, then Twincast at the Shock, and
/// passes until the copy's retarget prompt opens.
fn twincast_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    let (first, _, shock) = shock_on_the_stack(&mut scenario);
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(shock).target_objects(&[first]).commit();
    runner.cast(twincast).target_objects(&[shock]).commit();
    pass_until(&mut runner, |state| {
        matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::CopyRetarget { .. }
        )
    });
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("choose new targets");
    }
    let WaitingFor::CopyRetarget { copy_id, .. } = runner.state().waiting_for.clone() else {
        panic!("expected the copy's retarget prompt");
    };
    (runner, first, shock, copy_id)
}

#[test]
fn a_copy_given_a_new_target_targets_it() {
    let (mut runner, first, shock, copy) = twincast_board();
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choose the copy's new target");
    let (held, declared) = both(runner.state(), copy);
    assert_eq!(
        held,
        vec![TargetRef::Player(P1)],
        "reach guard: the new target was written"
    );
    assert_eq!(declared, vec![TargetRef::Player(P1)], "CR 707.10c");
    assert_eq!(
        declared_of(runner.state(), shock),
        vec![TargetRef::Object(first)]
    );
}

#[test]
fn a_copy_that_keeps_its_targets_targets_what_the_original_does() {
    let (mut runner, first, _, copy) = twincast_board();
    runner
        .act(GameAction::KeepAllCopyTargets)
        .expect("keep the copy's targets");
    assert_eq!(
        declared_of(runner.state(), copy),
        vec![TargetRef::Object(first)],
        "CR 707.10"
    );
}

#[test]
fn each_copy_made_for_another_creature_targets_that_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zada = scenario
        .add_creature_from_oracle(P0, "Zada, Hedron Grinder", 3, 3, ZADA)
        .as_legendary()
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(growth).target_objects(&[zada]).commit();
    pass_until(&mut runner, |state| {
        state.stack.len() == 2
            && state
                .stack
                .iter()
                .all(|e| matches!(e.kind, StackEntryKind::Spell { .. }))
    });
    let copy = runner.state().stack.back().expect("the copy").id;
    assert_ne!(copy, growth, "reach guard: a copy was put on the stack");
    let (held, declared) = both(runner.state(), copy);
    assert_eq!(
        held,
        vec![TargetRef::Object(bear)],
        "reach guard: the copy's target was rewritten"
    );
    assert_eq!(declared, vec![TargetRef::Object(bear)], "CR 707.10d");
    assert_eq!(
        declared_of(runner.state(), growth),
        vec![TargetRef::Object(zada)]
    );
}

const ISOCHRON_SCEPTER: &str = "Imprint — When this artifact enters, you may exile an instant card with mana value 2 or less from your hand.\n{2}, {T}: You may copy the exiled card. If you do, you may cast the copy without paying its mana cost.";
const DECORUM_DISSERTATION: &str = "Target player draws two cards and loses 2 life.\nParadigm (Then exile this spell. After you first resolve a spell with this name, you may cast a copy of it from exile without paying its mana cost at the beginning of each of your first main phases.)";
const MIZZIXS_MASTERY: &str = "Exile target card that's an instant or sorcery from your graveyard. For each card exiled this way, copy it, and you may cast the copy without paying its mana cost. Exile Mizzix's Mastery.\nOverload {5}{R}{R}{R} (You may cast this spell for its overload cost. If you do, change \"target\" in its text to \"each.\")";
const MASS_MUTINY: &str = "For each opponent, gain control of up to one target creature that player controls until end of turn. Untap those creatures. They gain haste until end of turn.";
const ARC_TRAIL: &str = "Arc Trail deals 2 damage to any target and 1 damage to any other target.";

fn add_colorless(runner: &mut GameRunner, player: PlayerId, amount: usize) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player")
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// The copy on the stack whose `CopyRetarget` prompt is open.
fn copy_being_cast(runner: &GameRunner) -> ObjectId {
    match &runner.state().waiting_for {
        WaitingFor::CopyRetarget { copy_id, .. } => *copy_id,
        other => panic!("expected the copy's target prompt, got {other:?}"),
    }
}

#[test]
fn a_copy_cast_from_isochron_scepter_targets_what_it_is_cast_at() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let scepter = scenario
        .add_artifact_to_hand_from_oracle(P0, "Isochron Scepter", ISOCHRON_SCEPTER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(scepter).commit();
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("imprint");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    assert_eq!(
        runner.state().objects[&shock].zone,
        Zone::Exile,
        "reach guard: Shock was imprinted"
    );
    add_colorless(&mut runner, P0, 2);
    runner
        .act(GameAction::ActivateAbility {
            source_id: scepter,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate");
    for _ in 0..12 {
        match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("copy and cast");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            _ => break,
        }
    }
    let copy = copy_being_cast(&runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("choose the copy's target");
    let (held, declared) = both(runner.state(), copy);
    assert_eq!(
        held,
        vec![TargetRef::Object(bear)],
        "reach guard: the copy targets the chosen creature"
    );
    assert_eq!(declared, vec![TargetRef::Object(bear)], "CR 707.12");
}

#[test]
fn a_paradigm_copy_targets_what_it_is_cast_at() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..8 {
        scenario.add_card_to_library_top(P0, "Library Card");
        scenario.add_card_to_library_top(P1, "Library Card");
    }
    let dissertation = scenario
        .add_spell_to_hand_from_oracle(P0, "Decorum Dissertation", false, DECORUM_DISSERTATION)
        .from_oracle_text_with_keywords(&["Paradigm"], DECORUM_DISSERTATION)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(dissertation).target_player(P1).commit();
    pass_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::CastOffer { .. })
    });
    runner
        .act(GameAction::CastParadigmCopy {
            source: dissertation,
        })
        .expect("cast the copy");
    let copy = copy_being_cast(&runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("choose the copy's target");
    let (held, declared) = both(runner.state(), copy);
    assert_eq!(
        held,
        vec![TargetRef::Player(P0)],
        "reach guard: the copy targets the chosen player"
    );
    assert_eq!(declared, vec![TargetRef::Player(P0)], "CR 707.12");
}

#[test]
fn a_cast_copy_of_a_per_opponent_spell_targets_only_the_creatures_it_is_aimed_at() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "First Bear", 2, 2).id();
    let second = scenario.add_creature(PlayerId(2), "Second Bear", 2, 2).id();
    let mutiny = scenario
        .add_spell_to_graveyard(P0, "Mass Mutiny", false)
        .from_oracle_text(MASS_MUTINY)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let mastery = scenario
        .add_spell_to_hand_from_oracle(P0, "Mizzix's Mastery", false, MIZZIXS_MASTERY)
        .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(mastery).target_objects(&[mutiny]).commit();
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("cast the copy");
            }
            _ => break,
        }
    }
    let copy = copy_being_cast(&runner);
    while let WaitingFor::CopyRetarget {
        target_slots,
        current_slot,
        ..
    } = runner.state().waiting_for.clone()
    {
        let target = target_slots[current_slot].legal_alternatives[0].clone();
        runner
            .act(GameAction::ChooseTarget {
                target: Some(target),
            })
            .expect("choose the copy's targets");
    }
    let (held, declared) = both(runner.state(), copy);
    assert!(
        held.contains(&TargetRef::Object(first)) && held.contains(&TargetRef::Object(second)),
        "reach guard: the copy holds both creatures, got {held:?}"
    );
    assert_eq!(
        declared,
        vec![TargetRef::Object(first), TargetRef::Object(second)],
        "CR 115.1"
    );
}

#[test]
fn a_copy_given_a_new_first_target_still_targets_its_second() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P1, "Second Bear", 2, 2).id();
    let other = scenario.add_creature(P1, "Other Bear", 2, 2).id();
    let arc_trail = scenario
        .add_spell_to_hand_from_oracle(P0, "Arc Trail", false, ARC_TRAIL)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner
        .cast(arc_trail)
        .target_objects(&[first, second])
        .commit();
    assert_eq!(
        declared_of(runner.state(), arc_trail),
        vec![TargetRef::Object(first), TargetRef::Object(second)],
        "reach guard: Arc Trail was cast at both creatures"
    );
    runner.cast(twincast).target_objects(&[arc_trail]).commit();
    pass_until(&mut runner, |state| {
        matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::CopyRetarget { .. }
        )
    });
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("choose new targets");
    }
    let copy = copy_being_cast(&runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(other)),
        })
        .expect("choose the copy's new first target");
    while matches!(runner.state().waiting_for, WaitingFor::CopyRetarget { .. }) {
        runner
            .act(GameAction::ChooseTarget { target: None })
            .expect("keep the copy's other targets");
    }
    let (held, declared) = both(runner.state(), copy);
    assert_eq!(
        held,
        vec![TargetRef::Object(other), TargetRef::Object(second)],
        "reach guard: the copy's first target changed and its second did not"
    );
    assert_eq!(
        declared,
        vec![TargetRef::Object(other), TargetRef::Object(second)],
        "CR 707.10c"
    );
}

#[test]
fn a_spell_given_a_creature_in_place_of_a_player_targets_the_creature() {
    let mut scenario = GameScenario::new();
    let (_, other, shock) = shock_on_the_stack(&mut scenario);
    let redirect = scenario
        .add_spell_to_hand_from_oracle(P1, "Redirect", true, REDIRECT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(shock).target_player(P1).commit();
    assert_eq!(
        declared_of(runner.state(), shock),
        vec![TargetRef::Player(P1)],
        "reach guard: Shock was cast at P1"
    );
    runner.act(GameAction::PassPriority).expect("pass to P1");
    runner.cast(redirect).target_objects(&[shock]).commit();
    pass_until(&mut runner, |state| {
        matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::RetargetChoice { .. }
        )
    });
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("choose new targets");
    }
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(other))],
        })
        .expect("retarget");
    let (held, declared) = both(runner.state(), shock);
    assert_eq!(
        held,
        vec![TargetRef::Object(other)],
        "reach guard: the new target was written"
    );
    assert_eq!(declared, vec![TargetRef::Object(other)], "CR 115.7d");
}

#[test]
fn a_spell_whose_player_target_is_changed_to_a_creature_targets_that_creature() {
    let mut scenario = GameScenario::new();
    let (_, _, shock) = shock_on_the_stack(&mut scenario);
    let spellskite = scenario
        .add_creature_from_oracle(P1, "Spellskite", 0, 4, SPELLSKITE)
        .id();
    let mut runner = scenario.build();
    runner.cast(shock).target_player(P1).commit();
    assert_eq!(
        declared_of(runner.state(), shock),
        vec![TargetRef::Player(P1)],
        "reach guard: Shock was cast at P1"
    );
    runner.act(GameAction::PassPriority).expect("pass to P1");
    runner
        .act(GameAction::ActivateAbility {
            source_id: spellskite,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(shock)),
                    })
                    .expect("target Shock");
            }
            WaitingFor::PhyrexianPayment { .. } => {
                runner
                    .act(GameAction::SubmitPhyrexianChoices {
                        choices: vec![ShardChoice::PayLife],
                    })
                    .expect("pay 2 life");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    pass_until(&mut runner, |state| state.stack.len() == 1);
    let (held, declared) = both(runner.state(), shock);
    assert_eq!(
        held,
        vec![TargetRef::Object(spellskite)],
        "reach guard: the new target was written"
    );
    assert_eq!(declared, vec![TargetRef::Object(spellskite)], "CR 115.7b");
}

const SYMBIOSIS: &str = "Two target creatures each get +2/+2 until end of turn.";
const CONDUCT_ELECTRICITY: &str = "Conduct Electricity deals 6 damage to target creature and 2 damage to up to one target creature token.";

/// P0 casts Symbiosis at `first` and `second`, then Twincast at it, and gives
/// the copy `new_targets` one prompt slot at a time.
fn symbiosis_copy_retargeted(
    new_targets: fn(ObjectId, ObjectId, ObjectId) -> [ObjectId; 2],
) -> (GameRunner, ObjectId, [ObjectId; 3]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let third = scenario.add_creature(P0, "Third Bear", 2, 2).id();
    let symbiosis = scenario
        .add_spell_to_hand_from_oracle(P0, "Symbiosis", true, SYMBIOSIS)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner
        .cast(symbiosis)
        .target_objects(&[first, second])
        .commit();
    runner.cast(twincast).target_objects(&[symbiosis]).commit();
    pass_until(&mut runner, |state| {
        matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::CopyRetarget { .. }
        )
    });
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("choose new targets");
    }
    let copy = copy_being_cast(&runner);
    for target in new_targets(first, second, third) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("choose the copy's target for this slot");
    }
    (runner, copy, [first, second, third])
}

#[test]
fn a_copy_given_targets_that_overlap_its_old_ones_targets_them_in_order() {
    let (runner, copy, [_, second, third]) =
        symbiosis_copy_retargeted(|_, second, third| [second, third]);
    let expected = vec![TargetRef::Object(second), TargetRef::Object(third)];
    let (held, declared) = both(runner.state(), copy);
    assert_eq!(
        held, expected,
        "reach guard: the copy's new targets were written"
    );
    assert_eq!(declared, expected, "CR 707.10c");
}

#[test]
fn a_copy_given_its_targets_in_swapped_order_targets_them_in_that_order() {
    let (runner, copy, [first, second, _]) =
        symbiosis_copy_retargeted(|first, second, _| [second, first]);
    let expected = vec![TargetRef::Object(second), TargetRef::Object(first)];
    let (held, declared) = both(runner.state(), copy);
    assert_eq!(
        held, expected,
        "reach guard: the copy's new targets were written"
    );
    assert_eq!(declared, expected, "CR 707.10c");
}

/// P1 casts Redirect at `spell` and chooses `new_targets` for it.
fn redirect(
    runner: &mut GameRunner,
    redirect: ObjectId,
    spell: ObjectId,
    new_targets: Vec<TargetRef>,
) {
    runner.act(GameAction::PassPriority).expect("pass to P1");
    runner.cast(redirect).target_objects(&[spell]).commit();
    pass_until(runner, |state| {
        matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::RetargetChoice { .. }
        )
    });
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("choose new targets");
    }
    runner
        .act(GameAction::RetargetSpell {
            new_targets: new_targets.into_iter().map(Some).collect(),
        })
        .expect("retarget");
}

/// P0 casts Arc Trail at `first` and `second`; P1 gives it the targets
/// `new_targets` picks.
fn arc_trail_redirected(
    new_targets: fn(ObjectId, ObjectId, ObjectId) -> [ObjectId; 2],
) -> (GameRunner, ObjectId, Vec<TargetRef>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P1, "First Bear", 2, 2).id();
    let second = scenario.add_creature(P1, "Second Bear", 2, 2).id();
    let third = scenario.add_creature(P1, "Third Bear", 2, 2).id();
    let arc_trail = scenario
        .add_spell_to_hand_from_oracle(P0, "Arc Trail", false, ARC_TRAIL)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let redirect_card = scenario
        .add_spell_to_hand_from_oracle(P1, "Redirect", true, REDIRECT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner
        .cast(arc_trail)
        .target_objects(&[first, second])
        .commit();
    let new_targets: Vec<TargetRef> = new_targets(first, second, third)
        .into_iter()
        .map(TargetRef::Object)
        .collect();
    redirect(&mut runner, redirect_card, arc_trail, new_targets.clone());
    (runner, arc_trail, new_targets)
}

#[test]
fn a_spell_given_targets_that_overlap_its_old_ones_targets_them_in_order() {
    let (runner, arc_trail, expected) = arc_trail_redirected(|_, second, third| [second, third]);
    let (held, declared) = both(runner.state(), arc_trail);
    assert_eq!(held, expected, "reach guard: the new targets were written");
    assert_eq!(declared, expected, "CR 115.7d + CR 115.7e");
}

#[test]
fn a_spell_given_its_targets_in_swapped_order_targets_them_in_that_order() {
    let (runner, arc_trail, expected) = arc_trail_redirected(|first, second, _| [second, first]);
    let (held, declared) = both(runner.state(), arc_trail);
    assert_eq!(held, expected, "reach guard: the new targets were written");
    assert_eq!(declared, expected, "CR 115.7d + CR 115.7e");
}

#[test]
fn a_spell_that_targets_one_token_twice_keeps_the_target_left_unchanged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let token = scenario.add_creature(P1, "Token", 3, 3).id();
    let other_token = scenario.add_creature(P1, "Other Token", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Conduct Electricity", true, CONDUCT_ELECTRICITY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let redirect_card = scenario
        .add_spell_to_hand_from_oracle(P1, "Redirect", true, REDIRECT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    for id in [token, other_token] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .expect("token")
            .is_token = true;
    }
    runner.cast(spell).target_objects(&[token, token]).commit();
    assert_eq!(
        declared_of(runner.state(), spell),
        vec![TargetRef::Object(token), TargetRef::Object(token)],
        "reach guard: CR 115.3 lets one token be both targets"
    );
    let expected = vec![TargetRef::Object(token), TargetRef::Object(other_token)];
    redirect(&mut runner, redirect_card, spell, expected.clone());
    let (held, declared) = both(runner.state(), spell);
    assert_eq!(
        held, expected,
        "reach guard: only the second target changed"
    );
    assert_eq!(declared, expected, "CR 115.3 + CR 115.7d");
}

const SWORDS_TO_PLOWSHARES: &str =
    "Exile target creature. Its controller gains life equal to its power.";
const BLOODCHIEFS_THIRST: &str = "Kicker {2}{B} (You may pay an additional {2}{B} as you cast this spell.)\nDestroy target creature or planeswalker with mana value 2 or less. If this spell was kicked, instead destroy target creature or planeswalker.";

#[test]
fn swords_to_plowshares_targets_only_the_creature_it_exiles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let swords = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS_TO_PLOWSHARES)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(swords).target_objects(&[bear]).commit();
    let (held, declared) = both(runner.state(), swords);
    assert_eq!(
        held,
        vec![TargetRef::Object(bear), TargetRef::Object(bear)],
        "reach guard: the life-gain clause holds a copy of the target"
    );
    assert_eq!(declared, vec![TargetRef::Object(bear)], "CR 115.10a");
}

#[test]
fn a_kicked_spell_targets_only_what_its_kicked_clause_is_cast_at() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let thirst = scenario
        .add_spell_to_hand_from_oracle(P0, "Bloodchief's Thirst", false, BLOODCHIEFS_THIRST)
        .from_oracle_text_with_keywords(&["Kicker"], BLOODCHIEFS_THIRST)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, thirst, false, vec![]),
            ManaUnit::new(ManaType::Colorless, thirst, false, vec![]),
            ManaUnit::new(ManaType::Colorless, thirst, false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner
        .cast(thirst)
        .accept_optional()
        .target_objects(&[bear])
        .commit();
    let (held, declared) = both(runner.state(), thirst);
    assert_eq!(
        held,
        vec![TargetRef::Object(bear), TargetRef::Object(bear)],
        "reach guard: the kicked clause's parent holds a copy of its target"
    );
    assert_eq!(declared, vec![TargetRef::Object(bear)], "CR 115.1");
}

#[test]
fn a_copy_of_a_kicked_spell_given_a_new_target_targets_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let other = scenario.add_creature(P1, "Other Bear", 2, 2).id();
    let thirst = scenario
        .add_spell_to_hand_from_oracle(P0, "Bloodchief's Thirst", false, BLOODCHIEFS_THIRST)
        .from_oracle_text_with_keywords(&["Kicker"], BLOODCHIEFS_THIRST)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, thirst, false, vec![]),
            ManaUnit::new(ManaType::Colorless, thirst, false, vec![]),
            ManaUnit::new(ManaType::Colorless, thirst, false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner
        .cast(thirst)
        .accept_optional()
        .target_objects(&[bear])
        .commit();
    runner.cast(twincast).target_objects(&[thirst]).commit();
    pass_until(&mut runner, |state| {
        matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::CopyRetarget { .. }
        )
    });
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("choose new targets");
    }
    let copy = copy_being_cast(&runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(other)),
        })
        .expect("choose the copy's new target");
    let copy_targets = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == copy)
        .and_then(|e| e.ability())
        .map(|ability| ability.targets.clone());
    assert_eq!(
        copy_targets,
        Some(vec![TargetRef::Object(other)]),
        "reach guard: the new target was written"
    );
    assert_eq!(
        declared_of(runner.state(), copy),
        vec![TargetRef::Object(other)],
        "CR 707.10c"
    );
}

#[test]
fn a_kicked_spell_given_a_new_target_targets_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let other = scenario.add_creature(P1, "Other Bear", 2, 2).id();
    let thirst = scenario
        .add_spell_to_hand_from_oracle(P0, "Bloodchief's Thirst", false, BLOODCHIEFS_THIRST)
        .from_oracle_text_with_keywords(&["Kicker"], BLOODCHIEFS_THIRST)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let redirect_card = scenario
        .add_spell_to_hand_from_oracle(P1, "Redirect", true, REDIRECT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, thirst, false, vec![]),
            ManaUnit::new(ManaType::Colorless, thirst, false, vec![]),
            ManaUnit::new(ManaType::Colorless, thirst, false, vec![]),
        ],
    );
    let mut runner = scenario.build();
    runner
        .cast(thirst)
        .accept_optional()
        .target_objects(&[bear])
        .commit();
    assert_eq!(
        declared_of(runner.state(), thirst),
        vec![TargetRef::Object(bear)],
        "reach guard: the kicked spell was cast at the first creature"
    );
    redirect(
        &mut runner,
        redirect_card,
        thirst,
        vec![TargetRef::Object(other)],
    );
    assert_eq!(
        declared_of(runner.state(), thirst),
        vec![TargetRef::Object(other)],
        "CR 115.7d"
    );
}

const THOPTER_ARCHITECT: &str =
    "Whenever an artifact you control enters, target creature gains flying until end of turn.";
const GENEMORPH_IMAGO: &str = "Flying\nLandfall — Whenever a land you control enters, target creature has base power and toughness 3/3 until end of turn. If you control six or more lands, that creature has base power and toughness 6/6 until end of turn instead.";
const SHADOW_ALLEY_DENIZEN: &str = "Whenever another black creature you control enters, target creature gains intimidate until end of turn. (It can't be blocked except by artifact creatures and/or creatures that share a color with it.)";
const NIGHTMARE: &str = "Flying (This creature can't be blocked except by creatures with flying or reach.)\nNightmare's power and toughness are each equal to the number of Swamps you control.";
const REANIMATE: &str = "Put target creature card from a graveyard onto the battlefield under your control. You lose life equal to that card's mana value.";

/// P0 controls Thopter Architect and, if `bear`, a 2/2, and casts a
/// noncreature artifact.
fn architect_board(bear: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let architect = scenario
        .add_creature_from_oracle(P0, "Thopter Architect", 2, 3, THOPTER_ARCHITECT)
        .id();
    if bear {
        scenario.add_creature(P0, "Bear", 2, 2);
    }
    let relic = scenario
        .add_artifact_to_hand_from_oracle(P0, "Relic", "")
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(relic).commit();
    (runner, architect)
}

#[test]
fn an_enters_trigger_whose_only_legal_target_is_its_source_targets_its_source() {
    let (mut runner, architect) = architect_board(false);
    let entry = trigger_of(&mut runner, architect);
    let (held, declared) = both(runner.state(), entry);
    assert_eq!(declared, vec![TargetRef::Object(architect)], "CR 115.1d");
    assert_eq!(held, vec![TargetRef::Object(architect)], "CR 115.1");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&architect]
            .keywords
            .contains(&Keyword::Flying),
        "CR 115.1"
    );
}

#[test]
fn an_enters_trigger_aimed_at_its_source_by_choice_targets_its_source() {
    let (mut runner, architect) = architect_board(true);
    pass_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. })
    });
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(architect)),
        })
        .expect("choose target");
    let entry = trigger_of(&mut runner, architect);
    let (held, declared) = both(runner.state(), entry);
    assert_eq!(declared, vec![TargetRef::Object(architect)], "CR 115.1d");
    assert_eq!(held, vec![TargetRef::Object(architect)], "CR 115.1");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&architect]
            .keywords
            .contains(&Keyword::Flying),
        "CR 115.1"
    );
}

#[test]
fn a_landfall_trigger_whose_only_legal_target_is_its_source_targets_its_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let imago = scenario
        .add_creature(P0, "Genemorph Imago", 1, 3)
        .from_oracle_text_with_keywords(&["Flying", "Landfall"], GENEMORPH_IMAGO)
        .id();
    let forest = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&forest].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest,
            card_id,
        })
        .expect("play land");
    let entry = trigger_of(&mut runner, imago);
    let (held, declared) = both(runner.state(), entry);
    assert_eq!(declared, vec![TargetRef::Object(imago)], "CR 115.1d");
    assert_eq!(held, vec![TargetRef::Object(imago)], "CR 115.1");
    runner.advance_until_stack_empty();
    let imago = &runner.state().objects[&imago];
    assert_eq!(
        (imago.power, imago.toughness),
        (Some(3), Some(3)),
        "CR 115.1"
    );
}

/// P0 reanimates P1's Nightmare, which dies as a 0/0 before Shadow Alley
/// Denizen's trigger is put on the stack, so the Denizen is its only legal
/// target.
#[test]
fn an_enters_trigger_left_with_only_its_source_to_target_commits_no_crime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let denizen = scenario
        .add_creature_from_oracle(P0, "Shadow Alley Denizen", 1, 1, SHADOW_ALLEY_DENIZEN)
        .with_color(vec![ManaColor::Black])
        .id();
    let nightmare = scenario
        .add_creature_to_graveyard(P1, "Nightmare", 0, 0)
        .from_oracle_text_with_keywords(&["Flying"], NIGHTMARE)
        .with_color(vec![ManaColor::Black])
        .id();
    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(reanimate).target_objects(&[nightmare]).commit();
    assert_eq!(
        crimes(runner.state(), P0),
        1,
        "reach guard: targeting a card in P1's graveyard is a crime"
    );
    let events = pass_until(&mut runner, |state| {
        pending_trigger_of(state, denizen).is_some()
    });
    let entry = pending_trigger_of(runner.state(), denizen).expect("the Denizen's trigger");
    assert_eq!(
        runner.state().objects[&nightmare].zone,
        Zone::Graveyard,
        "reach guard: the Nightmare died"
    );
    let (held, declared) = both(runner.state(), entry);
    assert_eq!(declared, vec![TargetRef::Object(denizen)], "CR 115.1d");
    assert_eq!(held, vec![TargetRef::Object(denizen)], "CR 115.1");
    assert_eq!(criminals(&events), Vec::new(), "CR 700.13");
}

/// Not a printed card: a paid "instead" clause followed by another targeted
/// instruction.
const KICKED_INSTEAD_THEN_TARGET_PLAYER: &str = "Kicker {2}{B} (You may pay an additional {2}{B} as you cast this spell.)\nDestroy target creature or planeswalker with mana value 2 or less. If this spell was kicked, instead destroy target creature or planeswalker. Target player loses 2 life.";

/// P0 holds `KICKED_INSTEAD_THEN_TARGET_PLAYER` and its kicker mana; P1 controls
/// two creatures. Returns the scenario, the spell, and the creatures.
fn kicked_then_player_board() -> (GameScenario, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let other = scenario.add_creature(P1, "Other Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Kicked Then Player",
            false,
            KICKED_INSTEAD_THEN_TARGET_PLAYER,
        )
        .from_oracle_text_with_keywords(&["Kicker"], KICKED_INSTEAD_THEN_TARGET_PLAYER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Black, spell, false, vec![]),
            ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
            ManaUnit::new(ManaType::Colorless, spell, false, vec![]),
        ],
    );
    (scenario, spell, bear, other)
}

/// Reach guards: the spell was kicked, its "instead" clause is the paid child,
/// the parent holds a copy of the child's target, and the later clause holds
/// the player.
fn assert_kicked_child_and_later_player(state: &GameState, spell: ObjectId, creature: ObjectId) {
    let ability = state
        .stack
        .iter()
        .find(|e| e.id == spell)
        .and_then(|e| e.ability())
        .expect("the spell is on the stack");
    assert!(
        ability.context.additional_cost_paid,
        "reach guard: the spell was kicked"
    );
    assert_eq!(
        ability
            .sub_ability
            .as_deref()
            .and_then(|child| child.condition.clone()),
        Some(AbilityCondition::AdditionalCostPaidInstead),
        "reach guard: the kicked clause is the parent's child"
    );
    assert_eq!(
        flatten_targets_in_chain(ability),
        vec![
            TargetRef::Object(creature),
            TargetRef::Object(creature),
            TargetRef::Player(P1),
        ],
        "reach guard: the parent holds a copy of the child's target"
    );
}

#[test]
fn a_kicked_spell_with_a_later_target_chosen_one_by_one_targets_both() {
    let (scenario, spell, bear, _) = kicked_then_player_board();
    let mut runner = scenario.build();
    runner
        .cast(spell)
        .accept_optional()
        .target_object(bear)
        .target_player(P1)
        .commit();
    assert_kicked_child_and_later_player(runner.state(), spell, bear);
    assert_eq!(
        declared_of(runner.state(), spell),
        vec![TargetRef::Object(bear), TargetRef::Player(P1)],
        "CR 601.2c"
    );
}

#[test]
fn a_kicked_spell_with_a_later_target_chosen_together_targets_both() {
    let (scenario, spell, bear, _) = kicked_then_player_board();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the spell is castable");
    loop {
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalCostChoice { .. } => GameAction::DecideOptionalCost { pay: true },
            WaitingFor::TargetSelection { .. } => GameAction::SelectTargets {
                targets: vec![TargetRef::Object(bear), TargetRef::Player(P1)],
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while casting: {other:?}"),
        };
        runner.act(action).expect("the cast advances");
    }
    assert_kicked_child_and_later_player(runner.state(), spell, bear);
    assert_eq!(
        declared_of(runner.state(), spell),
        vec![TargetRef::Object(bear), TargetRef::Player(P1)],
        "CR 601.2c"
    );
}

#[test]
fn a_kicked_spell_with_a_later_target_given_a_new_kicked_target_targets_it() {
    let (mut scenario, spell, bear, other) = kicked_then_player_board();
    let redirect_card = scenario
        .add_spell_to_hand_from_oracle(P1, "Redirect", true, REDIRECT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner
        .cast(spell)
        .accept_optional()
        .target_object(bear)
        .target_player(P1)
        .commit();
    redirect(
        &mut runner,
        redirect_card,
        spell,
        vec![TargetRef::Object(other), TargetRef::Player(P1)],
    );
    assert_kicked_child_and_later_player(runner.state(), spell, other);
    assert_eq!(
        declared_of(runner.state(), spell),
        vec![TargetRef::Object(other), TargetRef::Player(P1)],
        "CR 115.7d"
    );
}

fn displayed_targets(state: &GameState, id: ObjectId) -> Vec<TargetRef> {
    derive_views(state, Some(P0)).stack_entry_details[&id]
        .targets
        .iter()
        .map(|display| display.target.clone())
        .collect()
}

fn strip_target_records(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(map) => {
            let removed = usize::from(map.remove("chosen_target_slots").is_some());
            removed + map.values_mut().map(strip_target_records).sum::<usize>()
        }
        serde_json::Value::Array(items) => items.iter_mut().map(strip_target_records).sum(),
        _ => 0,
    }
}

/// Saves `state` as the app persists it, less every `chosen_target_slots` key,
/// as a save written before target records were kept has none, and restores it
/// as the app does. Returns the restored state and the number of keys removed.
fn restore_without_target_records(state: &GameState) -> (GameState, usize) {
    let mut saved =
        serde_json::to_value(PersistedGameState::capture(state.clone())).expect("serialize");
    let removed = strip_target_records(&mut saved);
    let restored = serde_json::from_value::<PersistedGameState>(saved)
        .expect("decode")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("admit")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restore");
    (restored, removed)
}

/// Whether the stack entry with `id` carries no target record.
fn has_no_target_record(state: &GameState, id: ObjectId) -> bool {
    let ability = state
        .stack
        .iter()
        .find(|e| e.id == id)
        .and_then(|e| e.ability())
        .expect("entry with an ability");
    serde_json::to_value(ability)
        .expect("serialize")
        .get("chosen_target_slots")
        .is_none()
}

#[test]
fn a_spell_restored_from_a_save_without_target_records_targets_and_shows_its_target() {
    let mut scenario = GameScenario::new();
    let (first, _, shock) = shock_on_the_stack(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(shock).target_objects(&[first]).commit();
    let (restored, removed) = restore_without_target_records(runner.state());
    assert!(removed > 0, "reach guard: the save held a target record");
    assert!(
        has_no_target_record(&restored, shock),
        "reach guard: the restored entry has no record"
    );
    assert_eq!(
        declared_of(&restored, shock),
        vec![TargetRef::Object(first)],
        "CR 601.2c"
    );
    assert_eq!(
        displayed_targets(&restored, shock),
        vec![TargetRef::Object(first)]
    );
}

#[test]
fn a_trigger_in_a_save_written_before_target_records_targets_the_opponent_it_was_aimed_at() {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(
        &include_bytes!("../fixtures/f4_user_mode1_no_offer_4p.json.gz")[..],
    )
    .read_to_string(&mut json)
    .expect("inflate");
    let envelope: serde_json::Value = serde_json::from_str(&json).expect("parse");
    let restored = serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("decode")
        .into_game_state()
        .expect("restore");
    let trigger = ObjectId(415);
    let (held, declared) = both(&restored, trigger);
    assert_eq!(
        held,
        vec![TargetRef::Player(PlayerId(1))],
        "reach guard: the saved trigger holds its target"
    );
    assert!(
        has_no_target_record(&restored, trigger),
        "reach guard: the save has no target record"
    );
    assert_eq!(declared, held, "CR 603.3d");
}

#[test]
fn hindering_light_counters_a_spell_restored_from_a_save_without_target_records() {
    let mut scenario = GameScenario::new();
    let (bear, _, shock) = shock_on_the_stack(&mut scenario);
    let hindering_light = scenario
        .add_spell_to_hand_from_oracle(P1, "Hindering Light", true, HINDERING_LIGHT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(shock).target_object(bear).commit();
    runner.act(GameAction::PassPriority).expect("P0 passes");
    let (restored, removed) = restore_without_target_records(runner.state());
    assert!(removed > 0, "reach guard: the save held a target record");
    assert!(
        has_no_target_record(&restored, shock),
        "reach guard: the restored Shock has no record"
    );
    let mut runner = GameRunner::from_state(restored);
    assert_eq!(
        cast_at_its_only_target(&mut runner, hindering_light),
        Ok(()),
        "CR 115.9b"
    );
    pass_until(&mut runner, |state| state.stack.is_empty());
    assert_eq!(runner.state().objects[&shock].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        0,
        "CR 701.6a: the Shock was countered"
    );
}

/// P0 casts Shock, at a generic cost of {1} with one Mountain, at `target`
/// with manual payment; the game is saved while it waits for that payment,
/// restored without target records, and paid.
fn shock_paid_after_a_restore(mut scenario: GameScenario, target: ObjectId) -> GameRunner {
    scenario.at_phase(Phase::PreCombatMain);
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&shock].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: shock,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("cast");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            })
            .expect("target");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach guard: saved while waiting for payment"
    );
    let (restored, removed) = restore_without_target_records(runner.state());
    assert!(removed > 0, "reach guard: the save held a target record");
    let mut runner = GameRunner::from_state(restored);
    runner.act(GameAction::PassPriority).expect("pay");
    assert!(
        runner.state().stack.iter().any(|e| e.id == shock),
        "reach guard: Shock is on the stack"
    );
    runner
}

#[test]
fn a_cast_paid_after_a_restore_without_target_records_is_a_crime() {
    let mut scenario = GameScenario::new();
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let runner = shock_paid_after_a_restore(scenario, bear);
    assert_eq!(crimes(runner.state(), P0), 1, "CR 700.13");
}

#[test]
fn a_cast_paid_after_a_restore_without_target_records_triggers_heroic() {
    let (scenario, crusader) = crusader_board();
    let runner = shock_paid_after_a_restore(scenario, crusader);
    assert!(
        pending_trigger_of(runner.state(), crusader).is_some(),
        "CR 115.9b"
    );
}

const DIVINATION: &str = "Draw two cards.";
const FOUNTAIN_OF_YOUTH: &str = "{2}, {T}: You gain 1 life.";
const ABZAN_CHARM: &str = "Choose one \u{2014}\n\u{2022} Exile target creature with power 3 or greater.\n\u{2022} You draw two cards and you lose 2 life.\n\u{2022} Distribute two +1/+1 counters among one or two target creatures.";

/// Whether every node of `ability` records that it chose no target.
fn records_no_target(ability: &ResolvedAbility) -> bool {
    ability.chosen_target_slots == ChosenTargetSlots::Recorded(Vec::new())
        && ability.sub_ability.as_deref().is_none_or(records_no_target)
        && ability
            .else_ability
            .as_deref()
            .is_none_or(records_no_target)
}

fn entry_ability(state: &GameState, source: ObjectId) -> &ResolvedAbility {
    state
        .stack
        .iter()
        .find(|e| e.source_id == source)
        .and_then(|e| e.ability())
        .expect("entry with an ability")
}

#[test]
fn an_untargeted_spell_is_recorded_as_choosing_no_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let divination = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(divination).commit();
    assert!(
        records_no_target(entry_ability(runner.state(), divination)),
        "CR 601.2c"
    );
}

#[test]
fn an_untargeted_mode_is_recorded_as_choosing_no_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let charm = scenario
        .add_spell_to_hand_from_oracle(P0, "Abzan Charm", true, ABZAN_CHARM)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(charm).modes(&[1]).commit();
    assert!(
        records_no_target(entry_ability(runner.state(), charm)),
        "CR 601.2c"
    );
}

#[test]
fn an_untargeted_activated_ability_is_recorded_as_choosing_no_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fountain = scenario
        .add_artifact_from_oracle(P0, "Fountain of Youth", FOUNTAIN_OF_YOUTH)
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: fountain,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate");
    assert!(
        records_no_target(entry_ability(runner.state(), fountain)),
        "CR 602.2b"
    );
}

const CHARMING_PRINCE: &str = "When this creature enters, choose one \u{2014}\n\u{2022} Scry 2.\n\u{2022} You gain 3 life.\n\u{2022} Exile another target creature you own. Return it to the battlefield under your control at the beginning of the next end step.";
const OPT: &str = "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.";

#[test]
fn an_untargeted_mode_of_a_trigger_is_recorded_as_choosing_no_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let prince = scenario
        .add_creature_to_hand_from_oracle(P0, "Charming Prince", 2, 2, CHARMING_PRINCE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(prince).commit();
    pass_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::AbilityModeChoice { .. })
    });
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("choose the life mode");
    assert!(
        records_no_target(entry_ability(runner.state(), prince)),
        "CR 603.3c"
    );
}

#[test]
fn an_untargeted_copy_cast_from_isochron_scepter_is_recorded_as_choosing_no_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opt = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, OPT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let scepter = scenario
        .add_artifact_to_hand_from_oracle(P0, "Isochron Scepter", ISOCHRON_SCEPTER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(scepter).commit();
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("imprint");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    assert_eq!(
        runner.state().objects[&opt].zone,
        Zone::Exile,
        "reach guard: Opt was imprinted"
    );
    add_colorless(&mut runner, P0, 2);
    runner
        .act(GameAction::ActivateAbility {
            source_id: scepter,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate");
    let copy = (0..12)
        .find_map(|_| {
            let copy = runner
                .state()
                .stack
                .iter()
                .find(|e| matches!(e.kind, StackEntryKind::Spell { .. }))
                .map(|e| e.id);
            if copy.is_none() {
                match runner.state().waiting_for {
                    WaitingFor::OptionalEffectChoice { .. } => runner
                        .act(GameAction::DecideOptionalEffect { accept: true })
                        .map(|_| ())
                        .expect("copy and cast"),
                    _ => runner
                        .act(GameAction::PassPriority)
                        .map(|_| ())
                        .expect("pass"),
                }
            }
            copy
        })
        .expect("reach guard: the copy is on the stack");
    let copy_ability = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == copy)
        .and_then(|e| e.ability())
        .expect("the copy has an ability");
    assert!(records_no_target(copy_ability), "CR 707.12");
}

#[test]
fn an_untargeted_copy_cast_by_mizzixs_mastery_is_recorded_as_choosing_no_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let opt = scenario
        .add_spell_to_graveyard(P0, "Opt", true)
        .from_oracle_text(OPT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let mastery = scenario
        .add_spell_to_hand_from_oracle(P0, "Mizzix's Mastery", false, MIZZIXS_MASTERY)
        .from_oracle_text_with_keywords(&["Overload"], MIZZIXS_MASTERY)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(mastery).target_objects(&[opt]).commit();
    let copy = (0..12)
        .find_map(|_| {
            let copy = runner
                .state()
                .stack
                .iter()
                .find(|e| e.id != mastery && matches!(e.kind, StackEntryKind::Spell { .. }))
                .map(|e| e.id);
            if copy.is_none() {
                match runner.state().waiting_for.clone() {
                    WaitingFor::ChooseFromZoneChoice { cards, .. } => runner
                        .act(GameAction::SelectCards { cards })
                        .map(|_| ())
                        .expect("cast the copy"),
                    _ => runner
                        .act(GameAction::PassPriority)
                        .map(|_| ())
                        .expect("pass"),
                }
            }
            copy
        })
        .expect("reach guard: the copy is on the stack");
    assert!(
        records_no_target(entry_ability(runner.state(), copy)),
        "CR 707.12"
    );
}

const GERMINATION_PRACTICUM: &str = "Put two +1/+1 counters on each creature you control.\nParadigm (Then exile this spell. After you first resolve a spell with this name, you may cast a copy of it from exile without paying its mana cost at the beginning of each of your first main phases.)";

#[test]
fn an_untargeted_paradigm_copy_is_recorded_as_choosing_no_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..8 {
        scenario.add_card_to_library_top(P0, "Library Card");
        scenario.add_card_to_library_top(P1, "Library Card");
    }
    let practicum = scenario
        .add_spell_to_hand_from_oracle(P0, "Germination Practicum", false, GERMINATION_PRACTICUM)
        .from_oracle_text_with_keywords(&["Paradigm"], GERMINATION_PRACTICUM)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(practicum).commit();
    pass_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::CastOffer { .. })
    });
    runner
        .act(GameAction::CastParadigmCopy { source: practicum })
        .expect("cast the copy");
    let copy = runner
        .state()
        .stack
        .iter()
        .find(|e| matches!(e.kind, StackEntryKind::Spell { .. }))
        .and_then(|e| e.ability())
        .expect("reach guard: the copy is on the stack");
    assert!(records_no_target(copy), "CR 707.12");
}

#[test]
fn a_trigger_saved_and_restored_still_targets_none_of_the_attackers_it_holds() {
    let (mut runner, attacker, strategist) = sabotage_board();
    let entry = trigger_of(&mut runner, strategist);
    let saved =
        serde_json::to_value(PersistedGameState::capture(runner.state().clone())).expect("save");
    let restored = serde_json::from_value::<PersistedGameState>(saved)
        .expect("decode")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("admit")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restore");
    let (held, declared) = both(&restored, entry);
    assert_eq!(
        held,
        vec![TargetRef::Object(attacker)],
        "reach guard: the trigger holds the attacker"
    );
    assert_eq!(declared, Vec::new(), "CR 115.10a");
}

#[test]
fn swords_to_plowshares_makes_the_creature_it_exiles_a_target_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let swords = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS_TO_PLOWSHARES)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(swords).target_objects(&[bear]).resolve();
    let became_target = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::BecomesTarget { target, source_id, .. }
                    if *source_id == swords && *target == TargetRef::Object(bear)
            )
        })
        .count();
    assert_eq!(became_target, 1, "CR 601.2c");
}

#[test]
fn swords_to_plowshares_shows_the_creature_it_exiles_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let swords = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS_TO_PLOWSHARES)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(swords).target_objects(&[bear]).commit();
    assert_eq!(
        displayed_targets(runner.state(), swords),
        vec![TargetRef::Object(bear)]
    );
}

const BURST_LIGHTNING: &str = "Kicker {4} (You may pay an additional {4} as you cast this spell.)\nBurst Lightning deals 2 damage to any target. If this spell was kicked, it deals 4 damage instead.";

#[test]
fn a_kicked_spell_whose_kicked_clause_reuses_its_target_is_a_crime() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let big = scenario.add_creature(P1, "Big", 5, 5).id();
    let burst = scenario
        .add_spell_to_hand_from_oracle(P0, "Burst Lightning", true, BURST_LIGHTNING)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let mut runner = scenario.build();
    runner
        .cast(burst)
        .target_object(big)
        .accept_optional()
        .commit();
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == burst)
        .and_then(|e| e.ability())
        .expect("Burst Lightning is on the stack");
    assert!(
        ability.context.additional_cost_paid,
        "reach guard: the spell was kicked"
    );
    assert_eq!(
        ability
            .sub_ability
            .as_deref()
            .map(|child| child.targets.clone()),
        Some(Vec::new()),
        "reach guard: the kicked clause holds no target of its own"
    );
    assert_eq!(crimes(runner.state(), P0), 1, "CR 700.13");
}

/// The first half of Trial // Error.
const TRIAL: &str =
    "Return all creatures blocking or blocked by target creature to their owner's hand.";
const TRUE_POLYMORPH: &str =
    "Target artifact or creature becomes a copy of another target artifact or creature.";

/// Casts `spell` and answers its target prompt with one `SelectTargets`.
fn cast_selecting_targets_together(
    runner: &mut GameRunner,
    spell: ObjectId,
    targets: Vec<TargetRef>,
) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the spell is castable");
    let mut targets = Some(targets);
    loop {
        let action = match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => GameAction::SelectTargets {
                targets: targets.take().expect("one target prompt"),
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while casting: {other:?}"),
        };
        runner.act(action).expect("the cast advances");
    }
}

/// P1 controls two creatures; P0 holds Trial. Returns the scenario, Trial and
/// the first creature.
fn trial_board() -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    scenario.add_creature(P1, "Other Bear", 2, 2);
    let trial = scenario
        .add_spell_to_hand_from_oracle(P0, "Trial", true, TRIAL)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    (scenario, trial, bear)
}

/// Reach guards: the spell is one `BounceAll` node, and that node holds the
/// target.
fn assert_trial_holds(state: &GameState, trial: ObjectId, creature: ObjectId) {
    let ability = state
        .stack
        .iter()
        .find(|e| e.id == trial)
        .and_then(|e| e.ability())
        .expect("Trial is on the stack");
    assert!(
        matches!(ability.effect, Effect::BounceAll { .. }) && ability.sub_ability.is_none(),
        "reach guard: one `BounceAll` node"
    );
    assert_eq!(
        ability.targets,
        vec![TargetRef::Object(creature)],
        "reach guard: the node holds its target"
    );
}

#[test]
fn a_spell_naming_the_creatures_in_combat_with_its_target_chosen_one_by_one_targets_it() {
    let (scenario, trial, bear) = trial_board();
    let mut runner = scenario.build();
    runner.cast(trial).target_object(bear).commit();
    assert_trial_holds(runner.state(), trial, bear);
    assert_eq!(
        declared_of(runner.state(), trial),
        vec![TargetRef::Object(bear)],
        "CR 601.2c"
    );
}

#[test]
fn a_spell_naming_the_creatures_in_combat_with_its_target_chosen_together_targets_it() {
    let (scenario, trial, bear) = trial_board();
    let mut runner = scenario.build();
    cast_selecting_targets_together(&mut runner, trial, vec![TargetRef::Object(bear)]);
    assert_trial_holds(runner.state(), trial, bear);
    assert_eq!(
        declared_of(runner.state(), trial),
        vec![TargetRef::Object(bear)],
        "CR 601.2c"
    );
}

#[test]
fn true_polymorph_targets_the_creature_it_changes_then_the_one_it_copies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Own", 1, 1).id();
    let theirs = scenario.add_creature(P1, "Theirs", 5, 5).id();
    let polymorph = scenario
        .add_spell_to_hand_from_oracle(P0, "True Polymorph", true, TRUE_POLYMORPH)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    cast_selecting_targets_together(
        &mut runner,
        polymorph,
        vec![TargetRef::Object(own), TargetRef::Object(theirs)],
    );
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == polymorph)
        .and_then(|e| e.ability())
        .expect("True Polymorph is on the stack");
    assert!(
        matches!(ability.effect, Effect::BecomeCopy { .. }) && ability.sub_ability.is_none(),
        "reach guard: one copy node"
    );
    assert_eq!(
        ability.targets,
        vec![TargetRef::Object(own), TargetRef::Object(theirs)],
        "reach guard: the node holds both targets"
    );
    assert_eq!(
        declared_of(runner.state(), polymorph),
        vec![TargetRef::Object(own), TargetRef::Object(theirs)],
        "CR 601.2c"
    );
}

const AKROAN_CRUSADER: &str = "Heroic — Whenever you cast a spell that targets this creature, create a 1/1 red Soldier creature token with haste. (It can attack and {T} as soon as it comes under your control.)";
const BLOOMING_BLAST: &str = "Gift a Treasure (You may promise an opponent a gift as you cast this spell. If you do, they create a Treasure token before its other effects. It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")\nBlooming Blast deals 2 damage to target creature. If the gift was promised, Blooming Blast also deals 3 damage to that creature's controller.";
const HINDERING_LIGHT: &str =
    "Counter target spell that targets you or a permanent you control.\nDraw a card.";
const RADIATE: &str = "Choose target instant or sorcery spell that targets only a single permanent or player. Copy that spell for each other permanent or player the spell could target. Each copy targets a different one of those permanents and players.";
const BOLT_BEND: &str = "This spell costs {3} less to cast if you control a creature with power 4 or greater.\nChange the target of target spell or ability with a single target.";

fn add_blooming_blast(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Blooming Blast", true, BLOOMING_BLAST)
        .from_oracle_text_with_keywords(&["Gift"], BLOOMING_BLAST)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

/// Reach guards: Blooming Blast is on the stack, its first node holds no
/// target, and it declares `target` alone.
fn assert_blast_targets_below_its_root(state: &GameState, blast: ObjectId, target: ObjectId) {
    let ability = state
        .stack
        .iter()
        .find(|e| e.id == blast)
        .and_then(|e| e.ability())
        .expect("Blooming Blast is on the stack");
    assert_eq!(
        ability.targets,
        Vec::new(),
        "reach guard: the first node holds no target"
    );
    assert_eq!(
        declared_of(state, blast),
        vec![TargetRef::Object(target)],
        "reach guard: the spell declares its target"
    );
}

/// P0 controls Akroan Crusader, and P1 a 2/2. Returns the scenario and the
/// Crusader.
fn crusader_board() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crusader = scenario
        .add_creature(P0, "Akroan Crusader", 1, 1)
        .from_oracle_text_with_keywords(&["Heroic"], AKROAN_CRUSADER)
        .id();
    scenario.add_creature(P1, "Bear", 2, 2);
    (scenario, crusader)
}

#[test]
fn blooming_blast_without_its_gift_at_akroan_crusader_triggers_heroic() {
    let (mut scenario, crusader) = crusader_board();
    let blast = add_blooming_blast(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(blast).target_object(crusader).commit();
    assert_blast_targets_below_its_root(runner.state(), blast, crusader);
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == blast)
        .and_then(|e| e.ability());
    assert_eq!(
        ability.and_then(|a| a.context.gift_recipient),
        None,
        "reach guard: the gift was not promised"
    );
    assert!(
        pending_trigger_of(runner.state(), crusader).is_some(),
        "CR 115.9b"
    );
}

#[test]
fn blooming_blast_with_its_gift_promised_at_akroan_crusader_triggers_heroic() {
    let (mut scenario, crusader) = crusader_board();
    let blast = add_blooming_blast(&mut scenario);
    let mut runner = scenario.build();
    runner
        .cast(blast)
        .target_object(crusader)
        .accept_optional()
        .commit();
    assert_blast_targets_below_its_root(runner.state(), blast, crusader);
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == blast)
        .and_then(|e| e.ability());
    assert_eq!(
        ability.and_then(|a| a.context.gift_recipient),
        Some(P1),
        "reach guard: the gift was promised"
    );
    assert!(
        pending_trigger_of(runner.state(), crusader).is_some(),
        "CR 115.9b"
    );
}

#[test]
fn shock_at_akroan_crusader_triggers_heroic() {
    let (mut scenario, crusader) = crusader_board();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(shock).target_object(crusader).commit();
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == shock)
        .and_then(|e| e.ability())
        .expect("Shock is on the stack");
    assert_eq!(
        ability.targets,
        vec![TargetRef::Object(crusader)],
        "reach guard: the first node holds the target"
    );
    assert!(
        pending_trigger_of(runner.state(), crusader).is_some(),
        "CR 115.9b"
    );
}

/// P0 controls Zada and another creature. Returns the scenario, Zada and the
/// other creature.
fn zada_board() -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zada = scenario
        .add_creature(P0, "Zada, Hedron Grinder", 3, 3)
        .from_oracle_text_with_keywords(&[], ZADA)
        .id();
    let other = scenario.add_creature(P0, "Bear", 2, 2).id();
    (scenario, zada, other)
}

#[test]
fn blooming_blast_at_zada_alone_triggers_zada() {
    let (mut scenario, zada, _) = zada_board();
    let blast = add_blooming_blast(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(blast).target_object(zada).commit();
    assert_blast_targets_below_its_root(runner.state(), blast, zada);
    assert!(
        pending_trigger_of(runner.state(), zada).is_some(),
        "CR 115.9c"
    );
}

#[test]
fn arc_trail_at_zada_and_another_creature_does_not_trigger_zada() {
    let (mut scenario, zada, other) = zada_board();
    let arc_trail = scenario
        .add_spell_to_hand_from_oracle(P0, "Arc Trail", false, ARC_TRAIL)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    cast_selecting_targets_together(
        &mut runner,
        arc_trail,
        vec![TargetRef::Object(zada), TargetRef::Object(other)],
    );
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == arc_trail)
        .and_then(|e| e.ability())
        .expect("Arc Trail is on the stack");
    assert_eq!(
        ability.targets,
        vec![TargetRef::Object(zada)],
        "reach guard: the first node holds only Zada"
    );
    assert_eq!(
        declared_of(runner.state(), arc_trail),
        vec![TargetRef::Object(zada), TargetRef::Object(other)],
        "reach guard: the spell declares both creatures"
    );
    assert!(
        pending_trigger_of(runner.state(), zada).is_none(),
        "CR 115.9c"
    );
}

/// P0 casts Blooming Blast at a creature it controls while it controls
/// another. Returns the runner, Blooming Blast, its target and the other
/// creature.
fn blast_on_the_stack(scenario: GameScenario) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = scenario;
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "Bear", 2, 2).id();
    let second = scenario.add_creature(P0, "Other Bear", 2, 3).id();
    let blast = add_blooming_blast(&mut scenario);
    let mut runner = scenario.build();
    runner.cast(blast).target_object(first).commit();
    assert_blast_targets_below_its_root(runner.state(), blast, first);
    (runner, blast, first, second)
}

/// Casts `spell`, whose only legal target the engine chooses, and returns
/// what the cast answered.
fn cast_at_its_only_target(runner: &mut GameRunner, spell: ObjectId) -> Result<(), String> {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

#[test]
fn hindering_light_can_target_a_spell_whose_target_sits_below_its_gift() {
    let mut scenario = GameScenario::new();
    let hindering_light = scenario
        .add_spell_to_hand_from_oracle(P0, "Hindering Light", true, HINDERING_LIGHT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let (mut runner, blast, _, _) = blast_on_the_stack(scenario);
    assert_eq!(
        cast_at_its_only_target(&mut runner, hindering_light),
        Ok(()),
        "CR 115.9b"
    );
    assert_eq!(
        declared_of(runner.state(), hindering_light),
        vec![TargetRef::Object(blast)]
    );
}

#[test]
fn radiate_can_target_a_spell_whose_single_target_sits_below_its_gift() {
    let mut scenario = GameScenario::new();
    let radiate = scenario
        .add_spell_to_hand_from_oracle(P0, "Radiate", true, RADIATE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let (mut runner, blast, _, _) = blast_on_the_stack(scenario);
    assert_eq!(
        cast_at_its_only_target(&mut runner, radiate),
        Ok(()),
        "CR 115.9c"
    );
    assert_eq!(
        declared_of(runner.state(), radiate),
        vec![TargetRef::Object(blast)]
    );
}

#[test]
fn bolt_bend_moves_the_target_of_a_spell_whose_target_sits_below_its_gift() {
    let mut scenario = GameScenario::new();
    let bolt_bend = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt Bend", true, BOLT_BEND)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let (mut runner, blast, first, second) = blast_on_the_stack(scenario);
    runner.cast(bolt_bend).target_object(blast).commit();
    pass_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::RetargetChoice { .. })
    });
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(second))],
        })
        .expect("the new target is legal");
    pass_until(&mut runner, |state| state.stack.is_empty());
    assert_eq!(
        (
            runner.state().objects[&first].damage_marked,
            runner.state().objects[&second].damage_marked
        ),
        (0, 2),
        "CR 115.9a + CR 115.7"
    );
}

const SEEDS_OF_STRENGTH: &str = "Target creature gets +1/+1 until end of turn.\nTarget creature gets +1/+1 until end of turn.\nTarget creature gets +1/+1 until end of turn.";
const MUCK_DRUBB: &str = "Flash\nWhen this creature enters, change the target of target spell that targets only a single creature to this creature.\nMadness {2}{B} (If you discard this card, discard it into exile. When you do, cast it for its madness cost or put it into your graveyard.)";
const NO_LEGAL_TARGETS: &str = "ActionNotAllowed(\"No legal targets available\")";

/// P0 casts Seeds of Strength at creatures it controls, choosing `picks`
/// (indices into those creatures) in one `SelectTargets`. Returns the runner
/// and Seeds.
fn seeds_on_the_stack(scenario: GameScenario, picks: [usize; 3]) -> (GameRunner, ObjectId) {
    let mut scenario = scenario;
    scenario.at_phase(Phase::PreCombatMain);
    let creatures = [
        scenario.add_creature(P0, "Bear", 2, 2).id(),
        scenario.add_creature(P0, "Other Bear", 2, 2).id(),
    ];
    let seeds = scenario
        .add_spell_to_hand_from_oracle(P0, "Seeds of Strength", true, SEEDS_OF_STRENGTH)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    let targets: Vec<TargetRef> = picks
        .iter()
        .map(|&pick| TargetRef::Object(creatures[pick]))
        .collect();
    cast_selecting_targets_together(&mut runner, seeds, targets.clone());
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == seeds)
        .and_then(|e| e.ability())
        .expect("Seeds of Strength is on the stack");
    assert_eq!(
        ability.targets,
        targets[..1].to_vec(),
        "reach guard: the first node holds one target"
    );
    assert_eq!(
        declared_of(runner.state(), seeds),
        targets,
        "reach guard: the spell declares every target it chose"
    );
    (runner, seeds)
}

fn add_radiate(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Radiate", true, RADIATE)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

#[test]
fn radiate_can_target_a_spell_that_targets_one_creature_three_times() {
    let mut scenario = GameScenario::new();
    let radiate = add_radiate(&mut scenario);
    let (mut runner, seeds) = seeds_on_the_stack(scenario, [0, 0, 0]);
    assert_eq!(
        cast_at_its_only_target(&mut runner, radiate),
        Ok(()),
        "CR 115.9c"
    );
    assert_eq!(
        declared_of(runner.state(), radiate),
        vec![TargetRef::Object(seeds)]
    );
}

#[test]
fn radiate_cannot_target_a_spell_that_targets_two_creatures() {
    let mut scenario = GameScenario::new();
    let radiate = add_radiate(&mut scenario);
    let (mut runner, _) = seeds_on_the_stack(scenario, [0, 0, 1]);
    assert_eq!(
        cast_at_its_only_target(&mut runner, radiate),
        Err(NO_LEGAL_TARGETS.to_string()),
        "CR 115.9c"
    );
}

#[test]
fn bolt_bend_cannot_target_a_spell_that_targets_one_creature_three_times() {
    let mut scenario = GameScenario::new();
    let bolt_bend = scenario
        .add_spell_to_hand_from_oracle(P0, "Bolt Bend", true, BOLT_BEND)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let (mut runner, _) = seeds_on_the_stack(scenario, [0, 0, 0]);
    assert_eq!(
        cast_at_its_only_target(&mut runner, bolt_bend),
        Err(NO_LEGAL_TARGETS.to_string()),
        "CR 115.9a"
    );
}

#[test]
fn muck_drubbs_trigger_can_target_a_spell_that_targets_one_creature_three_times() {
    let mut scenario = GameScenario::new();
    let drubb = scenario
        .add_creature_to_hand(P0, "Muck Drubb", 3, 3)
        .from_oracle_text_with_keywords(&["Flash", "Madness"], MUCK_DRUBB)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let (mut runner, seeds) = seeds_on_the_stack(scenario, [0, 0, 0]);
    runner.cast(drubb).commit();
    pass_until(&mut runner, |state| {
        !state.stack.iter().any(|e| e.id == drubb)
    });
    let trigger = pending_trigger_of(runner.state(), drubb);
    assert!(trigger.is_some(), "CR 115.9c + CR 603.3d");
    assert_eq!(
        trigger.map(|trigger| declared_of(runner.state(), trigger)),
        Some(vec![TargetRef::Object(seeds)])
    );
}

const LOKI: &str = "Whenever you cast a spell that targets only a single creature, gain control of that creature until end of turn. If it's your turn, untap that creature and it gains haste until end of turn.";
const LONGSTALK_BRAWL: &str = "Gift a tapped Fish (You may promise an opponent a gift as you cast this spell. If you do, they create a tapped 1/1 blue Fish creature token before its other effects.)\nChoose target creature you control and target creature you don't control. Put a +1/+1 counter on the creature you control if the gift was promised. Then those creatures fight each other.";

fn add_loki(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Loki, God of Lies", 3, 3)
        .from_oracle_text_with_keywords(&[], LOKI)
        .id()
}

#[test]
fn loki_triggers_on_a_spell_that_targets_one_creature_three_times() {
    let mut scenario = GameScenario::new();
    let loki = add_loki(&mut scenario);
    let (runner, _) = seeds_on_the_stack(scenario, [0, 0, 0]);
    assert!(
        pending_trigger_of(runner.state(), loki).is_some(),
        "CR 115.9c"
    );
}

#[test]
fn loki_does_not_trigger_on_a_spell_that_targets_two_creatures() {
    let mut scenario = GameScenario::new();
    let loki = add_loki(&mut scenario);
    let (runner, _) = seeds_on_the_stack(scenario, [0, 0, 1]);
    assert!(
        pending_trigger_of(runner.state(), loki).is_none(),
        "CR 115.9c"
    );
}

#[test]
fn loki_triggers_on_a_spell_whose_single_target_sits_below_its_gift() {
    let mut scenario = GameScenario::new();
    let loki = add_loki(&mut scenario);
    let (runner, _, _, _) = blast_on_the_stack(scenario);
    assert!(
        pending_trigger_of(runner.state(), loki).is_some(),
        "CR 115.9c"
    );
}

#[test]
fn loki_does_not_trigger_on_a_spell_whose_two_targets_sit_below_its_gift() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let loki = add_loki(&mut scenario);
    let own = scenario.add_creature(P0, "Bear", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Other Bear", 2, 2).id();
    let brawl = scenario
        .add_spell_to_hand_from_oracle(P0, "Longstalk Brawl", false, LONGSTALK_BRAWL)
        .from_oracle_text_with_keywords(&["Gift"], LONGSTALK_BRAWL)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(brawl).target_objects(&[own, theirs]).commit();
    let ability = runner
        .state()
        .stack
        .iter()
        .find(|e| e.id == brawl)
        .and_then(|e| e.ability())
        .expect("Longstalk Brawl is on the stack");
    assert_eq!(
        ability.targets,
        Vec::new(),
        "reach guard: the first node holds no target"
    );
    assert_eq!(
        ability.context.gift_recipient, None,
        "reach guard: the gift was not promised"
    );
    assert_eq!(
        declared_of(runner.state(), brawl),
        vec![TargetRef::Object(own), TargetRef::Object(theirs)],
        "reach guard: the spell declares both creatures"
    );
    assert!(
        pending_trigger_of(runner.state(), loki).is_none(),
        "CR 115.9c"
    );
}

const PREY_UPON: &str = "Target creature you control fights target creature you don't control. (Each deals damage equal to its power to the other.)";
const CAROM: &str = "The next 1 damage that would be dealt to target creature this turn is dealt to another target creature instead.\nDraw a card.";
const AWE_STRIKE: &str = "The next time target creature would deal damage this turn, prevent that damage. You gain life equal to the damage prevented this way.";

/// Returns the scenario, the spell `name` in P0's hand, P0's `Bear`, and P1's
/// `Ogre` and `Other Ogre`.
fn two_creatures_each_board(
    name: &str,
    is_instant: bool,
    oracle: &str,
) -> (GameScenario, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario.add_creature(P0, "Bear", 2, 2).id();
    scenario.add_creature(P0, "Other Bear", 2, 2);
    let theirs = scenario.add_creature(P1, "Ogre", 3, 3).id();
    let other = scenario.add_creature(P1, "Other Ogre", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, name, is_instant, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    (scenario, spell, own, theirs, other)
}

#[test]
fn prey_upon_given_both_creatures_together_targets_both() {
    let (scenario, prey_upon, own, theirs, _) =
        two_creatures_each_board("Prey Upon", false, PREY_UPON);
    let mut runner = scenario.build();
    cast_selecting_targets_together(
        &mut runner,
        prey_upon,
        vec![TargetRef::Object(own), TargetRef::Object(theirs)],
    );
    assert_eq!(
        declared_of(runner.state(), prey_upon),
        vec![TargetRef::Object(own), TargetRef::Object(theirs)],
        "CR 601.2c"
    );
    assert_eq!(crimes(runner.state(), P0), 1, "CR 700.13");
}

#[test]
fn carom_given_both_creatures_together_targets_both() {
    let (scenario, carom, _, theirs, other) = two_creatures_each_board("Carom", true, CAROM);
    let mut runner = scenario.build();
    cast_selecting_targets_together(
        &mut runner,
        carom,
        vec![TargetRef::Object(theirs), TargetRef::Object(other)],
    );
    assert_eq!(
        declared_of(runner.state(), carom),
        vec![TargetRef::Object(theirs), TargetRef::Object(other)],
        "CR 601.2c"
    );
    assert_eq!(crimes(runner.state(), P0), 1, "CR 700.13");
}

#[test]
fn awe_strike_given_its_creature_in_a_target_list_targets_it() {
    let (scenario, awe_strike, _, theirs, _) =
        two_creatures_each_board("Awe Strike", true, AWE_STRIKE);
    let mut runner = scenario.build();
    cast_selecting_targets_together(&mut runner, awe_strike, vec![TargetRef::Object(theirs)]);
    assert_eq!(
        declared_of(runner.state(), awe_strike),
        vec![TargetRef::Object(theirs)],
        "CR 601.2c"
    );
    assert_eq!(crimes(runner.state(), P0), 1, "CR 700.13");
}

#[test]
fn a_mana_spell_given_its_recipient_and_count_source_one_by_one_targets_both() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Role Split Ritual, as in `mana_target_recipient_and_count_source.rs`.
    let spell = scenario
        .add_spell_to_hand(P0, "Role Split Ritual", false)
        .with_mana_cost(ManaCost::zero())
        .with_ability(Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::TargetZoneCardCount {
                        zone: ZoneRef::Hand,
                        scope: ControllerRef::TargetOpponent,
                        binding: CountBinding::Explicit,
                    },
                },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: Some(ManaTargetRole::Both {
                recipient: TargetFilter::Player,
                count_source: TargetFilter::Player,
            }),
        })
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the spell is castable");
    let p2 = PlayerId(2);
    for player in [P1, p2] {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(player)),
            })
            .expect("the target is accepted");
    }
    assert_eq!(
        declared_of(runner.state(), spell),
        vec![TargetRef::Player(P1), TargetRef::Player(p2)],
        "CR 601.2c"
    );
    assert_eq!(crimes(runner.state(), P0), 1, "CR 700.13");
}

const BRIGHTMARE: &str = "When this creature enters, tap up to one target creature. You gain life equal to that creature's power.";

#[test]
fn an_enters_trigger_given_no_target_in_a_target_list_is_recorded_as_choosing_no_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Bear", 2, 2);
    let brightmare = scenario
        .add_creature_to_hand_from_oracle(P0, "Brightmare", 2, 3, BRIGHTMARE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    runner.cast(brightmare).commit();
    pass_until(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. })
    });
    runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("decline the target");
    assert!(
        records_no_target(entry_ability(runner.state(), brightmare)),
        "CR 603.3d"
    );
}
